use crate::github_render::OUTPUT_CHARS;
use crate::read_url::truncate_chars;
use crate::*;
use serde_json::json;
use std::sync::{LazyLock, Mutex};

const ACCEPT: &str = "application/vnd.github+json";
const MAX_NUMBER: u64 = 100_000_000;
const MAX_QUERY_CHARS: usize = 200;
const MAX_LABEL_CHARS: usize = 100;
const MAX_COMMENT_CHARS: usize = 2000;
const MAX_COMMENTS: u64 = 20;
const CACHE_ENTRIES: usize = 256;
/// Qualifiers that can widen a search past the configured repository.
const REPO_QUALIFIERS: [&str; 3] = ["repo:", "org:", "user:"];

/// Bearer token is read from the environment on every call so an operator can
/// rotate it without a restart. It is never stored in the config, logged, or
/// echoed back to the chat. Without it reads stay anonymous and writes fail
/// with an explicit message instead of a confusing 401.
pub(crate) fn token_from_env() -> Option<String> {
    ["GITHUB_TOKEN", "GH_TOKEN"]
        .iter()
        .filter_map(|name| env::var(name).ok())
        .map(|token| token.trim().to_owned())
        .find(|token| !token.is_empty())
}

static CLIENT: LazyLock<Option<reqwest::Client>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(config().github.timeout_secs))
        // The host is fixed by config; a redirect must not become a request to
        // an address the model influenced.
        .redirect(Policy::none())
        .no_proxy()
        .build()
        .ok()
});

fn client() -> Result<reqwest::Client, String> {
    CLIENT
        .clone()
        .ok_or_else(|| "GitHub-клиент недоступен".to_owned())
}

struct CachedIssue {
    stored_at: time::Instant,
    text: String,
}

type IssueKey = (String, u64, bool);

static CACHE: LazyLock<Mutex<HashMap<IssueKey, CachedIssue>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_get(settings: &GithubSettings, number: u64, include_comments: bool) -> Option<String> {
    let key = (settings.api_base.clone(), number, include_comments);
    let mut cache = CACHE.lock().ok()?;
    let ttl = Duration::from_secs(settings.cache_ttl_secs);
    match cache.get(&key) {
        Some(entry) if entry.stored_at.elapsed() < ttl => Some(entry.text.clone()),
        Some(_) => {
            cache.remove(&key);
            None
        }
        None => None,
    }
}

fn cache_put(settings: &GithubSettings, number: u64, include_comments: bool, text: String) {
    let Ok(mut cache) = CACHE.lock() else {
        return;
    };
    let ttl = Duration::from_secs(settings.cache_ttl_secs);
    if cache.len() >= CACHE_ENTRIES {
        cache.retain(|_, entry| entry.stored_at.elapsed() < ttl);
        while cache.len() >= CACHE_ENTRIES {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.stored_at)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    cache.remove(&key);
                }
                None => break,
            }
        }
    }
    cache.insert(
        (settings.api_base.clone(), number, include_comments),
        CachedIssue {
            stored_at: time::Instant::now(),
            text,
        },
    );
}

fn cache_clear(settings: &GithubSettings, number: u64) {
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|key, _| !(key.0 == settings.api_base && key.1 == number));
    }
}

/// One GitHub API call. The path is built from operator config plus a numeric
/// id or an encoded query only, so a model argument can never move the request
/// to another host or repository.
async fn fetch(
    settings: &GithubSettings,
    token: Option<&str>,
    method: reqwest::Method,
    path: &str,
    query: &[(&str, String)],
    payload: Option<&Value>,
) -> Result<Value, String> {
    let base = settings.api_base.trim_end_matches('/');
    let mut url = Url::parse(&format!("{base}{path}"))
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .ok_or_else(|| "github.api_base настроен неверно".to_owned())?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    let mut request = client()?
        .request(method, url)
        .header(reqwest::header::ACCEPT, ACCEPT)
        .header(reqwest::header::USER_AGENT, "maksut");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(payload) = payload {
        request = request.json(payload);
    }
    let response = time::timeout(Duration::from_secs(settings.timeout_secs), request.send())
        .await
        .map_err(|_| "GitHub не ответил вовремя".to_owned())?
        .map_err(|_| "GitHub недоступен".to_owned())?;
    let status = response.status().as_u16();
    let body = read_body(response, settings.max_response_bytes).await?;
    match status {
        200..=299 => {
            serde_json::from_slice(&body).map_err(|_| "GitHub вернул неожиданный ответ".to_owned())
        }
        404 => Err("не найдено: проверь номер issue или PR".to_owned()),
        401 | 403 => Err(format!(
            "GitHub API отказал (HTTP {status}): проверь GITHUB_TOKEN и лимит запросов"
        )),
        422 => Err("GitHub отклонил запрос (HTTP 422)".to_owned()),
        _ => Err(format!("GitHub API ответил HTTP {status}")),
    }
}

async fn read_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("ответ GitHub слишком большой".to_owned());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "GitHub недоступен".to_owned())?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err("ответ GitHub слишком большой".to_owned());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) fn valid_number(number: u64) -> bool {
    (1..=MAX_NUMBER).contains(&number)
}

pub(crate) async fn issue(
    settings: &GithubSettings,
    token: Option<&str>,
    number: u64,
    include_comments: bool,
) -> String {
    if !valid_number(number) {
        return "укажи номер issue или PR".to_owned();
    }
    if let Some(cached) = cache_get(settings, number, include_comments) {
        return cached;
    }
    let issue_path = format!("/repos/{}/issues/{number}", settings.repo);
    let issue = match fetch(
        settings,
        token,
        reqwest::Method::GET,
        &issue_path,
        &[],
        None,
    )
    .await
    {
        Ok(value) => value,
        Err(reason) => return reason,
    };
    let pull = if issue.get("pull_request").is_some() {
        let pull_path = format!("/repos/{}/pulls/{number}", settings.repo);
        fetch(settings, token, reqwest::Method::GET, &pull_path, &[], None)
            .await
            .ok()
    } else {
        None
    };
    let (comments, note) = if include_comments {
        // The per-issue comments endpoint has no `sort`/`direction` and returns
        // ascending ids, so the newest page must be addressed explicitly.
        let total = issue
            .get("comments")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let last_page = total.div_ceil(MAX_COMMENTS).max(1);
        let comments_path = format!("/repos/{}/issues/{number}/comments", settings.repo);
        let query = [
            ("per_page", MAX_COMMENTS.to_string()),
            ("page", last_page.to_string()),
        ];
        match fetch(
            settings,
            token,
            reqwest::Method::GET,
            &comments_path,
            &query,
            None,
        )
        .await
        {
            Ok(Value::Array(items)) => (items, String::new()),
            Ok(_) => (Vec::new(), " (неожиданный ответ API)".to_owned()),
            Err(reason) => (Vec::new(), format!(" (недоступны: {reason})")),
        }
    } else {
        (Vec::new(), String::new())
    };
    let text = truncate_chars(
        &crate::github_render::issue(&issue, pull.as_ref(), &comments, &note),
        OUTPUT_CHARS,
    )
    .0;
    cache_put(settings, number, include_comments, text.clone());
    text
}

pub(crate) struct SearchFilters<'a> {
    pub(crate) state: &'a str,
    pub(crate) label: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) limit: usize,
}

/// Cheap pre-checks shared with the dispatcher, so a refused search never
/// spends one of the per-reply allowances.
pub(crate) fn validate_search(query: &str, label: &str) -> Result<(), String> {
    let query = query.trim();
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(format!("слишком длинный запрос (макс {MAX_QUERY_CHARS})"));
    }
    // `repo:`/`org:`/`user:` repeat as a union in GitHub search, so a query
    // could widen the search past the configured repository.
    let lowered = query.to_ascii_lowercase();
    if REPO_QUALIFIERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return Err("репозиторий фиксирован: убери repo:, org: и user: из запроса".to_owned());
    }
    let label = label.trim();
    if label.chars().count() > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
        return Err("недопустимая метка".to_owned());
    }
    Ok(())
}

pub(crate) async fn search(
    settings: &GithubSettings,
    token: Option<&str>,
    query: &str,
    filters: SearchFilters<'_>,
) -> String {
    if let Err(reason) = validate_search(query, filters.label) {
        return reason;
    }
    let query = query.trim();
    let label = filters.label.trim();
    let limit = filters.limit.clamp(1, 10);
    if matches!(filters.kind, "commit" | "commits") {
        let per_page = if query.is_empty() || query == "*" {
            limit
        } else {
            30
        };
        let path = format!("/repos/{}/commits", settings.repo);
        let params = [("per_page", per_page.to_string())];
        let value = match fetch(settings, token, reqwest::Method::GET, &path, &params, None).await {
            Ok(value) => value,
            Err(reason) => return reason,
        };
        return crate::github_render::commits(&value, query, limit, &settings.repo);
    }
    let mut qualified = if query.is_empty() || query == "*" {
        format!("repo:{}", settings.repo)
    } else {
        format!("{query} repo:{}", settings.repo)
    };
    if matches!(filters.state, "open" | "closed") {
        qualified.push_str(&format!(" state:{}", filters.state));
    }
    if !label.is_empty() {
        let quoted = label.replace('"', "");
        qualified.push_str(&format!(" label:\"{quoted}\""));
    }
    match filters.kind {
        "issue" => qualified.push_str(" is:issue"),
        "pr" => qualified.push_str(" is:pr"),
        _ => {}
    }
    let query = [
        ("q", qualified),
        ("per_page", limit.to_string()),
        ("sort", "created".to_owned()),
        ("order", "desc".to_owned()),
    ];
    let value = match fetch(
        settings,
        token,
        reqwest::Method::GET,
        "/search/issues",
        &query,
        None,
    )
    .await
    {
        Ok(value) => value,
        Err(reason) => return reason,
    };
    crate::github_render::search(&value, limit, &settings.repo)
}

/// Cheap pre-checks shared by the dispatcher, so a rejected comment never
/// spends the single per-reply allowance. Returns the trimmed body.
pub(crate) fn validate_comment<'a>(token: Option<&str>, body: &'a str) -> Result<&'a str, String> {
    let body = body.trim();
    if body.is_empty() {
        return Err("пустой комментарий не отправляю".to_owned());
    }
    if body.chars().count() > MAX_COMMENT_CHARS {
        return Err(format!(
            "комментарий длиннее {MAX_COMMENT_CHARS} символов, сократи"
        ));
    }
    if crate::memory_guard::credential_like(body) {
        return Err("не отправляю: в тексте похоже на секрет или токен".to_owned());
    }
    if token.is_none() {
        return Err("GITHUB_TOKEN не настроен: писать комментарии нельзя".to_owned());
    }
    Ok(body)
}

pub(crate) async fn comment(
    settings: &GithubSettings,
    token: Option<&str>,
    number: u64,
    body: &str,
) -> String {
    if !valid_number(number) {
        return "укажи номер issue или PR".to_owned();
    }
    let body = match validate_comment(token, body) {
        Ok(body) => body,
        Err(reason) => return reason,
    };
    let Some(token) = token else {
        return "GITHUB_TOKEN не настроен: писать комментарии нельзя".to_owned();
    };
    let path = format!("/repos/{}/issues/{number}/comments", settings.repo);
    let payload = json!({"body": body});
    match fetch(
        settings,
        Some(token),
        reqwest::Method::POST,
        &path,
        &[],
        Some(&payload),
    )
    .await
    {
        Ok(value) => {
            cache_clear(settings, number);
            match value
                .get("html_url")
                .and_then(Value::as_str)
                .filter(|url| url.starts_with("https://github.com/"))
            {
                Some(url) => format!("комментарий отправлен: {url}"),
                None => "комментарий отправлен".to_owned(),
            }
        }
        Err(reason) => reason,
    }
}
