use crate::prelude::*;
use crate::search::{SearchClient, SearchProvider};
use crate::{llm_request_built, llm_response};
use crate::{
    CompleteError, Completion, CompletionRequest, LlmSettings, ReasoningJudge, ReasoningMode,
    ResearchRegistry, TitleResult, ToolRegistry, Usage, COMMUNICATION_GUIDE, FEATURE_TITLE_PROMPT,
    SYSTEM_PROMPT,
};
use serde_json::json;
use tokio::sync::{Mutex, OwnedSemaphorePermit};

use crate::chat_urls::RecentUrls;

#[derive(Clone)]
pub(crate) struct FeatureProposal {
    pub(crate) dir: std::path::PathBuf,
    pub(crate) owner_id: UserId,
}

#[derive(Clone)]
pub(crate) struct ChatConfig {
    pub(crate) base_url: Url,
    pub(crate) system_prompt: Arc<str>,
    pub(crate) api_key: String,
    pub(crate) model: String,
    pub(crate) search: Arc<SearchClient>,
    pub(crate) tools: ToolRegistry,
    pub(crate) http: reqwest::Client,
    requests: Arc<Semaphore>,
    last_reply: Arc<Mutex<HashMap<UserId, time::Instant>>>,
    pub(crate) last_research: Arc<Mutex<HashMap<UserId, time::Instant>>>,
    pub(crate) research_registry: Arc<ResearchRegistry>,
    last_active: Arc<Mutex<HashMap<ChatId, time::Instant>>>,
    pub(crate) recent_urls: Arc<Mutex<HashMap<ChatId, RecentUrls>>>,
    pub(crate) profile_cache: Arc<Mutex<crate::chat_profile::ProfileCache>>,
    pub(crate) research_cooldown: Duration,
    pub(crate) code_dir: PathBuf,
    judge: Arc<ReasoningJudge>,
}

impl ChatConfig {
    pub(crate) fn new(
        base_url: Url,
        api_key: String,
        model: String,
        search_url: Url,
        search_provider: SearchProvider,
        tavily_key: Option<String>,
    ) -> Self {
        let settings = config();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(settings.llm.request_timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        // Never follow redirects for search: the target host is fixed and a
        // redirect must not become a request to an internal service.
        let search_http = reqwest::Client::builder()
            .timeout(Duration::from_secs(settings.search.http_timeout_secs))
            .redirect(Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let system_prompt: Arc<str> =
            Arc::from(format!("{COMMUNICATION_GUIDE}\n\n---\n\n{SYSTEM_PROMPT}"));
        let judge = Arc::new(ReasoningJudge {
            mode: ReasoningMode::from_settings(&settings.llm),
            classifier: None,
            slots: None,
        });
        Self {
            base_url,
            system_prompt,
            api_key,
            model,
            search: Arc::new(SearchClient {
                url: search_url,
                provider: search_provider,
                tavily_key,
                http: search_http,
                engine_cooldowns: Arc::new(std::sync::Mutex::new(HashMap::new())),
            }),
            tools: ToolRegistry::builtin(),
            http,
            requests: Arc::new(Semaphore::new(settings.chat.concurrency)),
            last_reply: Arc::new(Mutex::new(HashMap::new())),
            last_research: Arc::new(Mutex::new(HashMap::new())),
            research_registry: Arc::new(ResearchRegistry::from_config()),
            last_active: Arc::new(Mutex::new(HashMap::new())),
            recent_urls: Arc::new(Mutex::new(HashMap::new())),
            profile_cache: Arc::new(Mutex::new(crate::chat_profile::ProfileCache::default())),
            research_cooldown: Duration::from_secs(settings.research.cooldown_secs),
            code_dir: PathBuf::from(&settings.paths.code_dir),
            judge,
        }
    }

    /// Enables the `auto` classifier. Production attaches it once from the
    /// loaded settings; a chat config without it keeps the policy default, which
    /// also keeps the test suite off the network.
    pub(crate) fn with_reasoning_classifier(self, settings: &LlmSettings) -> Self {
        let (classifier, slots) = build_classifier(settings, &self.judge.mode);
        self.with_classifier(classifier, slots)
    }

    /// The testable half of the attachment seam: it takes an already built
    /// client and never reads the environment or the global config.
    pub(crate) fn with_classifier(
        mut self,
        classifier: Option<crate::jev::JevClient>,
        slots: Option<Arc<Semaphore>>,
    ) -> Self {
        self.judge = Arc::new(ReasoningJudge {
            mode: self.judge.mode.clone(),
            classifier,
            slots,
        });
        self
    }

    /// A cheap handle a turn can move into a spawned task: it carries only the
    /// mode, the client and the semaphore, never the tool registry or the pool.
    pub(crate) fn judge(&self) -> Arc<ReasoningJudge> {
        self.judge.clone()
    }

    pub(crate) async fn acquire(&self) -> Option<OwnedSemaphorePermit> {
        self.requests.clone().acquire_owned().await.ok()
    }

    /// Spaces replies of one user by `chat.cooldown_ms` instead of dropping a
    /// quick follow-up question.
    pub(crate) async fn pace(&self, user_id: UserId) {
        loop {
            let cooldown = Duration::from_millis(config().chat.cooldown_ms);
            let max_entries = config().chat.max_pace_entries;
            let wait = {
                let mut last = self.last_reply.lock().await;
                let now = time::Instant::now();
                let wait = last
                    .get(&user_id)
                    .map(|previous| cooldown.saturating_sub(now.duration_since(*previous)))
                    .unwrap_or_default();
                if wait.is_zero() {
                    if last.len() > max_entries {
                        last.retain(|_, previous| now.duration_since(*previous) < cooldown);
                    }
                    last.insert(user_id, now);
                }
                wait
            };
            if wait.is_zero() {
                return;
            }
            time::sleep(wait).await;
        }
    }

    pub(crate) async fn note_reply(&self, chat_id: ChatId) {
        let mut active = self.last_active.lock().await;
        let now = time::Instant::now();
        let window = Duration::from_secs(config().chat.followup_window_secs);
        let limit = config().chat.max_active_chats;
        if active.len() >= limit && !active.contains_key(&chat_id) {
            active.retain(|_, at| now.duration_since(*at) < window);
            if active.len() >= limit {
                if let Some(oldest) = active
                    .iter()
                    .min_by_key(|(_, at)| **at)
                    .map(|(chat, _)| *chat)
                {
                    active.remove(&oldest);
                }
            }
        }
        active.insert(chat_id, now);
    }
    pub(crate) async fn clear_followup(&self, chat_id: ChatId) {
        let mut active = self.last_active.lock().await;
        active.remove(&chat_id);
    }

    pub(crate) async fn followup_active(&self, chat_id: ChatId) -> bool {
        let mut active = self.last_active.lock().await;
        let now = time::Instant::now();
        match active.get(&chat_id).copied() {
            Some(at)
                if now.duration_since(at)
                    < Duration::from_secs(config().chat.followup_window_secs) =>
            {
                true
            }
            Some(_) => {
                active.remove(&chat_id);
                false
            }
            None => false,
        }
    }

    fn endpoint(&self) -> Result<Url, String> {
        let mut url = self.base_url.clone();
        url.set_path(&format!(
            "{}/chat/completions",
            url.path().trim_end_matches('/')
        ));
        Ok(url)
    }

    /// One model call with no tools, for the internal title generator.
    async fn one_shot(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Result<Completion, String> {
        let msgs = [
            json!({"role": "system", "content": system}),
            json!({"role": "user", "content": user}),
        ];
        let mut req = CompletionRequest::new(&msgs, true, max_tokens).without_reasoning();
        req.temperature = 0.2;
        self.complete_with(&req, false, false)
            .await
            .map_err(|_| "request".to_string())
    }

    /// The GitHub issue title is written by the owner's approval, because the
    /// host orchestrator that creates the issue stays agent-free.
    pub(crate) async fn feature_title(&self, goal: &str) -> TitleResult {
        let user = format!("заявка:\n{goal}");
        let completion = match self
            .one_shot(
                FEATURE_TITLE_PROMPT,
                &user,
                config().chat.feature_title_max_tokens,
            )
            .await
        {
            Ok(completion) => completion,
            Err(reason) => {
                return TitleResult {
                    title: Err(reason),
                    usage: None,
                }
            }
        };
        let usage = completion.usage;
        let raw = completion
            .message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let line = raw
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        if line.is_empty() {
            return TitleResult {
                title: Err("empty".into()),
                usage,
            };
        }
        let title = line
            .trim_matches(|ch: char| ch.is_whitespace() || "\"'«»`*#".contains(ch))
            .trim_end_matches(['.', '!', '?', ':', ';', ','])
            .trim();
        let mut short: String = title.chars().take(70).collect();
        if title.chars().count() > 70 {
            if let Some(cut) = short.rfind(' ') {
                short.truncate(cut);
            }
        }
        let short = short.trim().to_owned();
        let title = if short.is_empty() {
            Err("empty".to_owned())
        } else {
            Ok(short)
        };
        TitleResult { title, usage }
    }

    pub(crate) async fn complete(
        &self,
        request: &CompletionRequest<'_>,
    ) -> Result<Completion, CompleteError> {
        match self.complete_with(request, true, request.reasoning).await {
            Err(CompleteError::Rejected) if self.judge.mode.is_configured() => {
                let mut fallback = *request;
                fallback.reasoning = true;
                match self.complete_with(&fallback, true, false).await {
                    Err(CompleteError::Rejected) => {
                        self.complete_with(&fallback, false, false).await
                    }
                    other => other,
                }
            }
            Err(CompleteError::Rejected) => self.complete_with(request, false, false).await,
            other => other,
        }
    }

    #[tracing::instrument(skip_all, fields(model = %self.model, max_tokens = request.max_tokens, tools = with_tools, reasoning = with_reasoning))]
    pub(crate) async fn complete_with(
        &self,
        request: &CompletionRequest<'_>,
        with_tools: bool,
        with_reasoning: bool,
    ) -> Result<Completion, CompleteError> {
        let complete_started = time::Instant::now();
        let mut body = serde_json::Map::new();
        body.insert("model".to_owned(), json!(self.model));
        body.insert("messages".to_owned(), json!(request.messages));
        body.insert("max_tokens".to_owned(), json!(request.max_tokens));
        body.insert("temperature".to_owned(), json!(request.temperature));
        let effort = match (&self.judge.mode, with_reasoning) {
            (ReasoningMode::Fixed(effort), true) => Some(effort.as_str()),
            (ReasoningMode::Auto(policy), true) => Some(policy.resolve(request.effort).as_str()),
            _ => None,
        };
        if let Some(effort) = effort {
            body.insert("reasoning_effort".to_owned(), json!(effort));
        } else if !request.reasoning && self.judge.mode.is_configured() {
            body.insert("reasoning_effort".to_owned(), json!("none"));
        }
        if with_tools {
            if let Some(tools) = self.tools.payload_for_mode(request.tool_mode()) {
                body.insert("tools".to_owned(), tools);
                body.insert("tool_choice".to_owned(), json!("auto"));
            }
        }
        let body = Value::Object(body);
        let endpoint = self.endpoint().map_err(|_| CompleteError::Failed)?;
        let policy = &config().retry.llm;
        let max_bytes = config().llm.max_response_bytes;
        let policy_attempts = policy.attempts;
        let per_attempt = Duration::from_secs(config().llm.request_timeout_secs);
        let deadline = time::Instant::now() + per_attempt.saturating_mul(policy_attempts);
        let payload = body.to_string();
        llm_request_built(request.messages.len(), payload.len(), request.temperature);
        let mut attempt = 0u32;
        loop {
            tracing::debug!(attempt = attempt + 1, "llm attempt");
            let remaining = deadline.saturating_duration_since(time::Instant::now());
            if remaining.is_zero() {
                return Err(CompleteError::Timeout);
            }
            let attempt_timeout = remaining.min(per_attempt);
            let request = self
                .http
                .post(endpoint.clone())
                .bearer_auth(&self.api_key)
                .header("content-type", "application/json")
                .body(payload.clone());
            let response = match time::timeout(attempt_timeout, request.send()).await {
                Ok(Ok(response)) => response,
                Ok(Err(err)) => {
                    tracing::warn!(attempt = attempt + 1, error = %err, "LLM transport error");
                    if policy.retry_transport && policy.should_retry(attempt) {
                        if !policy.backoff(attempt, deadline).await {
                            return Err(CompleteError::Timeout);
                        }
                        attempt += 1;
                        continue;
                    }
                    return Err(CompleteError::Failed);
                }
                Err(_) => {
                    tracing::warn!(attempt = attempt + 1, "LLM request timed out");
                    if policy.retry_transport && policy.should_retry(attempt) {
                        if !policy.backoff(attempt, deadline).await {
                            return Err(CompleteError::Timeout);
                        }
                        attempt += 1;
                        continue;
                    }
                    return Err(CompleteError::Timeout);
                }
            };
            let status = response.status();
            if matches!(status.as_u16(), 400 | 404 | 422) {
                tracing::warn!(status = status.as_u16(), "LLM rejected request");
                return Err(CompleteError::Rejected);
            }
            if !status.is_success() {
                tracing::warn!(
                    status = status.as_u16(),
                    attempt = attempt + 1,
                    "LLM HTTP error"
                );
                if policy.retry_status(status.as_u16()) && policy.should_retry(attempt) {
                    if !policy.backoff(attempt, deadline).await {
                        return Err(CompleteError::Status(status.as_u16()));
                    }
                    attempt += 1;
                    continue;
                }
                return Err(CompleteError::Status(status.as_u16()));
            }
            let remaining = deadline.saturating_duration_since(time::Instant::now());
            if remaining.is_zero() {
                return Err(CompleteError::Timeout);
            }
            let bytes = match time::timeout(remaining.min(per_attempt), response.bytes()).await {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(_)) | Err(_) => {
                    if policy.retry_transport && policy.should_retry(attempt) {
                        if !policy.backoff(attempt, deadline).await {
                            return Err(CompleteError::Timeout);
                        }
                        attempt += 1;
                        continue;
                    }
                    return Err(CompleteError::Timeout);
                }
            };
            if bytes.len() > max_bytes {
                tracing::warn!(
                    bytes = bytes.len(),
                    max = max_bytes,
                    "LLM response too large"
                );
                return Err(CompleteError::Failed);
            }
            let value: Value = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(_) => {
                    tracing::warn!(bytes = bytes.len(), "LLM response is not JSON");
                    return Err(CompleteError::Failed);
                }
            };
            let usage = Usage::from_response(&value);
            let Some(mut msg) = value.pointer("/choices/0/message").cloned() else {
                tracing::warn!("LLM response has no message");
                return Err(CompleteError::Failed);
            };
            let finish_reason = value
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str)
                .unwrap_or("");
            llm_response(
                complete_started,
                finish_reason,
                usage.map(|u| u.input_tokens as u64),
                usage.map(|u| u.output_tokens as u64),
            );
            if with_reasoning && self.judge.mode.is_configured() {
                if let (Some(obj), Some(fr)) = (
                    msg.as_object_mut(),
                    value
                        .pointer("/choices/0/finish_reason")
                        .and_then(Value::as_str),
                ) {
                    obj.insert("finish_reason".to_owned(), Value::String(fr.to_owned()));
                }
            }
            return Ok(Completion {
                message: msg,
                usage,
            });
        }
    }

    pub(crate) async fn web_search_with(&self, q: &str, rec: Option<&str>, lim: usize) -> String {
        self.search.web_search_with(q, rec, lim).await
    }
}

/// Builds the `auto` classifier once at startup. A missing `JEV_API_KEY`
/// disables classification loudly and leaves the policy default in place, so a
/// chat turn never depends on the tiny model being reachable; a malformed
/// `[llm.reasoning]` section already fails startup validation.
fn build_classifier(
    settings: &LlmSettings,
    mode: &ReasoningMode,
) -> (Option<crate::jev::JevClient>, Option<Arc<Semaphore>>) {
    if mode.policy().is_none() {
        return (None, None);
    }
    let key = env::var("JEV_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let Some(key) = key else {
        tracing::warn!("llm.reasoning_effort = auto without JEV_API_KEY: using the fallback tier");
        return (None, None);
    };
    let url = match crate::parse_http_endpoint(&settings.reasoning.url, "llm.reasoning.url") {
        Ok(url) => url,
        Err(error) => {
            tracing::warn!(%error, "reasoning classifier disabled");
            return (None, None);
        }
    };
    let policy = &settings.reasoning;
    match crate::jev::JevClient::new(
        url,
        key,
        policy.model.clone(),
        Duration::from_millis(policy.timeout_ms),
        policy.max_response_bytes,
        policy.min_confidence,
    ) {
        Ok(client) => (
            Some(client),
            Some(Arc::new(Semaphore::new(policy.concurrency))),
        ),
        Err(error) => {
            tracing::warn!(%error, "reasoning classifier disabled");
            (None, None)
        }
    }
}
