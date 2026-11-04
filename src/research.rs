use crate::elapsed_ms;
use crate::prelude::*;

use crate::chat_client::ChatConfig;
use crate::chat_text::{is_bidi_or_control, strip_url_schemes, truncate_text};
use crate::research_dossier::{fit_dossier, Interrupted};
use crate::research_registry::{ResearchJob, ResearchStatus};
use crate::search::{clean_text, SearchClient, SearchFailure, SearchHit};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormalizedUrl {
    /// URL as shown to the user: original query order, no tracking parameters.
    pub(crate) display: String,
    /// Deduplication key: sorted query parameters.
    pub(crate) canonical: String,
    /// Lower-case host without a leading `www.`.
    pub(crate) domain: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResearchSource {
    pub(crate) id: usize,
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) domain: String,
    pub(crate) snippet: String,
    /// 1-based query numbers that returned this URL.
    pub(crate) queries: Vec<usize>,
    pub(crate) best_position: usize,
}

pub(crate) struct ResearchOutcome {
    pub(crate) result: String,
    pub(crate) sources: Vec<ResearchSource>,
    pub(crate) queries_used: usize,
}

#[derive(Default)]
pub(crate) struct ResearchState {
    pub(crate) sources: Vec<ResearchSource>,
    pub(crate) used: bool,
    pub(crate) announced: bool,
}

impl ChatConfig {
    /// Validates input, applies the cooldown, and runs up to `budget` search
    /// calls. Returns a tool-facing string and the sources that survived
    /// ranking and dossier truncation.
    pub(crate) async fn research(
        &self,
        owner: bool,
        user_id: UserId,
        chat_id: ChatId,
        question: &str,
        raw_queries: &[String],
        budget: usize,
    ) -> Result<ResearchOutcome, String> {
        let question = question.split_whitespace().collect::<Vec<_>>().join(" ");
        let question_max_chars = config().research.question_max_chars;
        if question.is_empty() || question.chars().count() > question_max_chars {
            return Err(format!("нужен вопрос до {question_max_chars} символов"));
        }
        let (queries, dropped_queries) = validate_research_queries(raw_queries)?;
        if budget < config().research.min_queries {
            return Err("лимит поиска на ответ".into());
        }
        let execute = queries.len().min(budget);
        let truncated_queries = dropped_queries + (queries.len() - execute);
        tracing::info!(queries = execute, "research plan");
        if !owner {
            self.begin_research(user_id).await?;
        }
        let job = self
            .research_registry
            .start(chat_id, user_id, &question, execute)
            .await?;
        tracing::debug!(
            research_id = job.id(),
            chat_id = chat_id.0,
            queries = execute,
            "research started"
        );
        let research_started = time::Instant::now();
        let (result, sources) = collect_research(
            Arc::clone(&self.search),
            &question,
            queries.get(..execute).unwrap_or_default(),
            truncated_queries,
            Duration::from_secs(config().research.timeout_secs),
            Some(job),
        )
        .await;
        tracing::info!(
            sources = sources.len(),
            elapsed_ms = elapsed_ms(research_started),
            "research done"
        );
        Ok(ResearchOutcome {
            result,
            sources,
            queries_used: execute,
        })
    }

    async fn begin_research(&self, user_id: UserId) -> Result<(), String> {
        let mut last = self.last_research.lock().await;
        let now = time::Instant::now();
        if last
            .get(&user_id)
            .is_some_and(|previous| now.duration_since(*previous) < self.research_cooldown)
        {
            return Err("ресерч не чаще раза в 20 секунд".into());
        }
        if last.len() > 1024 {
            last.retain(|_, previous| now.duration_since(*previous) < self.research_cooldown);
        }
        last.insert(user_id, now);
        Ok(())
    }
}

pub(crate) async fn collect_research(
    client: Arc<SearchClient>,
    question: &str,
    queries: &[String],
    truncated_queries: usize,
    timeout: Duration,
    job: Option<ResearchJob<'_>>,
) -> (String, Vec<ResearchSource>) {
    let mut tasks = tokio::task::JoinSet::new();
    let mut task_indices = HashMap::new();
    let results_per_query = config().research.results_per_query;
    for (index, query) in queries.iter().cloned().enumerate() {
        let client = Arc::clone(&client);
        let handle = tasks.spawn(async move {
            (
                index,
                client
                    .public_web_search(&query, results_per_query, None)
                    .await,
            )
        });
        task_indices.insert(handle.id(), index);
    }
    let mut completed = vec![false; queries.len()];
    let mut results = Vec::new();
    let mut receiver = job.as_ref().map(ResearchJob::receiver);
    let collected = time::timeout(timeout, async {
        loop {
            let joined = match receiver.as_mut() {
                Some(receiver) => tokio::select! {
                    joined = tasks.join_next() => joined,
                    _ = wait_cancel(receiver) => return Err(()),
                },
                None => tasks.join_next().await,
            };
            let Some(joined) = joined else {
                return Ok(());
            };
            record_research_task(joined, &task_indices, &mut completed, &mut results);
            if let Some(job) = job.as_ref() {
                let (done, sources) = research_progress(&results);
                job.progress(done, sources).await;
            }
        }
    })
    .await;
    let status = match collected {
        Ok(Ok(())) => ResearchStatus::Completed,
        Ok(Err(())) => ResearchStatus::Canceled,
        Err(_) => ResearchStatus::TimedOut,
    };
    if status != ResearchStatus::Completed {
        tasks.abort_all();
        while let Some(joined) = tasks.join_next().await {
            record_research_task(joined, &task_indices, &mut completed, &mut results);
        }
    }
    for (index, done) in completed.iter().enumerate() {
        if !done {
            results.push((index, Err(SearchFailure::Canceled)));
        }
    }
    results.sort_by_key(|(index, _)| *index);
    let (completed_queries, sources_collected) = research_progress(&results);
    let interrupted = (status != ResearchStatus::Completed)
        .then(|| Interrupted::new(completed_queries, queries.len(), sources_collected));

    let mut failures = Vec::new();
    let mut by_key: HashMap<String, ResearchSource> = HashMap::new();
    for (index, result) in results {
        match result {
            Ok(hits) => {
                tracing::debug!(query = index + 1, hits = hits.len(), "research query hits");
                for (position, hit) in hits.into_iter().enumerate() {
                    let Some((key, source)) = research_source(hit, index + 1, position) else {
                        continue;
                    };
                    match by_key.entry(key) {
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            let existing = entry.get_mut();
                            if !existing.queries.contains(&(index + 1)) {
                                existing.queries.push(index + 1);
                            }
                            existing.best_position = existing.best_position.min(position);
                            if existing.snippet.is_empty() && !source.snippet.is_empty() {
                                existing.snippet = source.snippet;
                            }
                            if existing.title == existing.domain && source.title != source.domain {
                                existing.title = source.title;
                            }
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(source);
                        }
                    }
                }
            }
            Err(failure) => {
                tracing::debug!(query = index + 1, failure = ?failure, "research query failed");
                failures.push((index + 1, failure));
            }
        }
    }
    if by_key.is_empty() {
        let result = match interrupted.as_ref() {
            // An interrupted pass never established that the provider is down:
            // the header already says how much was collected, and claiming
            // "поиск недоступен" for a user-initiated cancel would be wrong.
            Some(interrupted) => interrupted.header(),
            None if !queries.is_empty() && failures.len() == queries.len() => {
                "поиск недоступен".to_string()
            }
            None => "ничего не найдено".to_string(),
        };
        finish_job(job, status, completed_queries, 0).await;
        return (result, Vec::new());
    }
    let mut sources: Vec<ResearchSource> = by_key.into_values().collect();
    sources.sort_by(|left, right| {
        right
            .queries
            .len()
            .cmp(&left.queries.len())
            .then(left.best_position.cmp(&right.best_position))
            .then(left.url.cmp(&right.url))
    });
    let max_sources = config().research.max_sources;
    let hidden_sources = sources.len().saturating_sub(max_sources);
    sources.truncate(max_sources);
    for (index, source) in sources.iter_mut().enumerate() {
        source.id = index + 1;
        source.queries.sort_unstable();
        source.queries.dedup();
    }
    let dossier = fit_dossier(
        question,
        queries,
        &sources,
        &failures,
        truncated_queries,
        hidden_sources,
        interrupted.as_ref(),
    );
    finish_job(job, status, completed_queries, sources_collected).await;
    dossier
}

/// Terminal state is recorded even when the pass was canceled or timed out:
/// `ResearchJob::finish` keeps the earlier `canceled` status set by
/// `research_cancel` and only fills in the final counts.
async fn finish_job(
    job: Option<ResearchJob<'_>>,
    status: ResearchStatus,
    completed_queries: usize,
    sources: usize,
) {
    if let Some(job) = job {
        job.finish(status, completed_queries, sources).await;
    }
}

async fn wait_cancel(receiver: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = receiver.wait_for(|canceled| *canceled).await;
}

/// Completed queries and distinct source URLs gathered so far; the running
/// pass publishes both to the registry after every finished query.
fn research_progress(results: &[(usize, Result<Vec<SearchHit>, SearchFailure>)]) -> (usize, usize) {
    let mut done = 0;
    let mut seen = std::collections::HashSet::new();
    for (_, result) in results {
        let Ok(hits) = result else {
            continue;
        };
        done += 1;
        for hit in hits {
            if let Some(normalized) = normalize_url(&hit.url) {
                seen.insert(normalized.canonical);
            }
        }
    }
    (done, seen.len())
}

fn record_research_task(
    joined: Result<(usize, Result<Vec<SearchHit>, SearchFailure>), tokio::task::JoinError>,
    task_indices: &HashMap<tokio::task::Id, usize>,
    completed: &mut [bool],
    results: &mut Vec<(usize, Result<Vec<SearchHit>, SearchFailure>)>,
) {
    match joined {
        Ok((index, result)) => {
            if let Some(done) = completed.get_mut(index) {
                *done = true;
            }
            results.push((index, result));
        }
        Err(error) => {
            if let Some(index) = task_indices.get(&error.id()).copied() {
                if let Some(done) = completed.get_mut(index) {
                    if !*done {
                        *done = true;
                        results.push((index, Err(SearchFailure::Canceled)));
                    }
                }
            }
        }
    }
}

fn validate_research_queries(raw: &[String]) -> Result<(Vec<String>, usize), String> {
    let query_min_chars = config().research.query_min_chars;
    let query_max_chars = config().research.query_max_chars;
    let max_queries = config().research.max_queries;
    let mut seen = std::collections::HashSet::new();
    let mut queries = Vec::new();
    for raw in raw {
        let query = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let length = query.chars().count();
        if !(query_min_chars..=query_max_chars).contains(&length) {
            continue;
        }
        let key = query.to_lowercase();
        if seen.insert(key) {
            queries.push(query);
        }
    }
    if queries.len() < config().research.min_queries {
        return Err("нужно 2-4 разных запроса".into());
    }
    let dropped = queries.len().saturating_sub(max_queries);
    queries.truncate(max_queries);
    Ok((queries, dropped))
}

fn research_source(
    hit: SearchHit,
    query: usize,
    position: usize,
) -> Option<(String, ResearchSource)> {
    let normalized = normalize_url(&hit.url)?;
    let title = sanitize_research_text(&hit.title, config().research.title_chars);
    let title = if title.is_empty() {
        normalized.domain.clone()
    } else {
        title
    };
    let snippet = sanitize_research_text(&hit.snippet, config().research.snippet_chars);
    Some((
        normalized.canonical.clone(),
        ResearchSource {
            id: 0,
            title,
            url: normalized.display,
            domain: normalized.domain,
            snippet,
            queries: vec![query],
            best_position: position,
        },
    ))
}

/// Canonical form for deduplication and a display form for the user. Query
/// parameters keep their original order in `display`; only `canonical` sorts
/// them. Tracking parameters are removed case-insensitively.
pub(crate) fn normalize_url(raw: &str) -> Option<NormalizedUrl> {
    let mut url = Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none_or(|host| host.is_empty())
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_fragment(None);
    if matches!(
        (url.scheme(), url.port()),
        ("http", Some(80)) | ("https", Some(443))
    ) {
        url.set_port(None).ok()?;
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (key, value) in url.query_pairs() {
        if is_tracking_parameter(&key) {
            continue;
        }
        let pair = (key.into_owned(), value.into_owned());
        if !pairs.contains(&pair) {
            pairs.push(pair);
        }
    }
    url.set_query(None);
    if !pairs.is_empty() {
        let mut query = url.query_pairs_mut();
        for (key, value) in &pairs {
            query.append_pair(key, value);
        }
    }
    let display = url.to_string();
    if display.chars().count() > config().research.url_max_chars {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let domain = host
        .strip_prefix("www.")
        .filter(|rest| rest.contains('.'))
        .unwrap_or(host.as_str())
        .to_owned();
    let mut canonical = url;
    let mut sorted = pairs;
    sorted.sort();
    canonical.set_query(None);
    if !sorted.is_empty() {
        let mut query = canonical.query_pairs_mut();
        for (key, value) in &sorted {
            query.append_pair(key, value);
        }
    }
    Some(NormalizedUrl {
        display,
        canonical: canonical.to_string(),
        domain,
    })
}

fn is_tracking_parameter(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_")
        || matches!(
            key.as_str(),
            "fbclid" | "gclid" | "yclid" | "mc_cid" | "mc_eid" | "igshid"
        )
}

/// Removes markup, decodes HTML entities, strips bidi/control characters,
/// neutralizes citation brackets and mentions, and cuts on a word boundary.
pub(crate) fn sanitize_research_text(raw: &str, max_chars: usize) -> String {
    let raw_limit = max_chars.saturating_mul(8).max(512);
    let raw = truncate_text(raw, raw_limit);
    let cleaned = clean_text(&raw);
    let mut text = String::with_capacity(cleaned.len());
    for ch in cleaned.chars() {
        if is_bidi_or_control(ch) {
            continue;
        }
        match ch {
            '[' => text.push('('),
            ']' => text.push(')'),
            '@' => text.push(' '),
            _ => text.push(ch),
        }
    }
    let text = strip_url_schemes(&text);
    truncate_text(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        max_chars,
    )
}
