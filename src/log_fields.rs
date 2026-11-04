use crate::*;

/// Two shapes only: `abc***xy` for a credential of 12+ characters, `***` below
/// that. The width never follows the input, because the length of a key is
/// itself metadata about the secret.
const MASK_SHORT: &str = "***";
/// Ceilings for values that reach a log field. A log line stays greppable
/// without letting one message, prompt or tool argument fill the disk.
pub(crate) const MAX_TEXT_CHARS: usize = 200;
pub(crate) const MAX_JSON_CHARS: usize = 400;

/// `BOT_TOKEN` and `LLM_API_KEY` never reach this helper: callers log only
/// whether they are configured, so no form of the value, not even a mask, can
/// be reconstructed.
///
/// The mask has a fixed shape: three visible leading characters, three stars,
/// two visible trailing characters for a value of 12+ characters, and a bare
/// `***` below that. The length of the secret is never revealed.
pub(crate) fn mask_secret(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 12 {
        return MASK_SHORT.to_owned();
    }
    let head: String = chars.iter().take(3).collect();
    let tail: String = chars.iter().skip(chars.len() - 2).collect();
    format!("{head}***{tail}")
}

/// Masks credential-like tokens inside free text and caps the result.
///
/// Ordinary words and identifiers survive, so a trace line stays readable,
/// while every token the credential detector flags is replaced as a whole.
/// This is the `trace` backstop: raw user messages, prompts and model replies
/// must not be logged at `info` or `debug` in the first place.
pub(crate) fn sanitize_text(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        let probe = word.trim_matches(|ch: char| {
            matches!(
                ch,
                '"' | '\'' | ',' | ':' | ';' | '{' | '}' | '[' | ']' | '(' | ')'
            )
        });
        if !probe.is_empty() && crate::memory_guard::credential_like(probe) {
            out.push_str(&mask_secret(probe));
        } else {
            out.push_str(word);
        }
        if out.chars().count() >= MAX_TEXT_CHARS {
            break;
        }
    }
    cap(&out, MAX_TEXT_CHARS)
}

/// Masks sensitive object fields and credential-like values inside JSON tool
/// arguments without collapsing the whole object into one `***` blob.
pub(crate) fn sanitize_json(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(sanitize_text(text)),
        Value::Array(items) => Value::Array(items.iter().map(sanitize_json).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let field = if sensitive_key(key) {
                        Value::String(
                            value
                                .as_str()
                                .map_or_else(|| mask_secret(&value.to_string()), mask_secret),
                        )
                    } else {
                        sanitize_json(value)
                    };
                    (key.clone(), field)
                })
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

pub(crate) fn sanitize_json_text(value: &Value) -> String {
    cap(&sanitize_json(value).to_string(), MAX_JSON_CHARS)
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "api_key" | "apikey" | "authorization" | "cookie" | "password" | "secret" | "token"
    ) || key.ends_with("_key")
        || key.ends_with("_token")
        || key.ends_with("_secret")
}

pub(crate) fn elapsed_ms(start: time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Request line for a search provider: the query itself stays out, only its
/// length does.
pub(crate) fn search_request(provider: &'static str, url: &Url, query: &str) {
    tracing::debug!(
        provider,
        host = url.host_str(),
        query_len = query.chars().count(),
        "search request"
    );
}

pub(crate) fn search_response(provider: &'static str, status: u16, bytes: usize) {
    tracing::debug!(provider, status, bytes, "search response");
}

fn known_recency(recency: Option<&str>) -> Option<&str> {
    recency.filter(|value| matches!(*value, "day" | "week" | "month" | "year"))
}

/// Only schema-approved values reach a log field; anything else keeps its
/// length and collapses to `None`.
pub(crate) fn content_kind(content_type: &str) -> &'static str {
    let lowered = content_type.to_ascii_lowercase();
    if lowered.contains("html") {
        "html"
    } else if lowered.contains("json") {
        "json"
    } else if lowered.contains("xml") {
        "xml"
    } else if lowered.starts_with("image/") {
        "image"
    } else if lowered.starts_with("text/") {
        "text"
    } else {
        "other"
    }
}

pub(crate) fn finish_kind(reason: &str) -> &'static str {
    match reason {
        "stop" => "stop",
        "length" => "length",
        "tool_calls" => "tool_calls",
        "content_filter" => "content_filter",
        _ => "other",
    }
}

pub(crate) fn search_finished(
    provider: crate::search::SearchProvider,
    recency: Option<&str>,
    hits: usize,
    elapsed: u64,
) {
    tracing::info!(
        provider = ?provider,
        recency = known_recency(recency),
        recency_len = recency.map_or(0, |value| value.chars().count()),
        hits,
        elapsed_ms = elapsed,
        "search finished"
    );
}

pub(crate) fn search_failed(
    provider: crate::search::SearchProvider,
    recency: Option<&str>,
    error: &crate::search::SearchFailure,
    elapsed: u64,
) {
    tracing::warn!(
        provider = ?provider,
        recency = known_recency(recency),
        recency_len = recency.map_or(0, |value| value.chars().count()),
        error = ?error,
        elapsed_ms = elapsed,
        "search failed"
    );
}

pub(crate) fn code_inspect_finished(files: usize, matches: usize, started: time::Instant) {
    tracing::debug!(
        files,
        matches,
        elapsed_ms = elapsed_ms(started),
        "code inspection finished"
    );
}

pub(crate) fn read_url_extracted(chars: usize, links: usize) {
    tracing::debug!(chars, links, "read_url extracted");
}

pub(crate) fn read_url_response_type(content_type: &str) {
    tracing::debug!(
        content_type = content_kind(content_type),
        content_type_len = content_type.chars().count(),
        "read_url response type"
    );
}

pub(crate) fn delivery_batch(delivered: usize, failed: bool) {
    tracing::info!(delivered, failed, "batch result");
}

pub(crate) fn queue_write_failed(path: &Path, error: &std::io::Error) {
    tracing::warn!(file = ?path.file_name(), kind = ?error.kind(), "queue write failed");
}

pub(crate) fn tool_call_args(args: &Value, args_len: usize) {
    tracing::debug!(
        args = ?args
            .as_object()
            .map(|map| map.keys().map(String::as_str).collect::<Vec<_>>()),
        args_len,
        "tool call"
    );
}

pub(crate) fn tool_result(tool: &str, result_bytes: usize) {
    tracing::debug!(tool, result_bytes, "tool result");
}

pub(crate) fn telegraph_start(host: Option<&str>, chars: usize, nodes: usize) {
    tracing::info!(host, chars, nodes, "telegraph publish start");
}

pub(crate) fn telegraph_finished(host: Option<&str>, started: time::Instant) {
    tracing::info!(
        host,
        elapsed_ms = elapsed_ms(started),
        "telegraph publish finished"
    );
}

pub(crate) fn telegraph_fallback(chars: usize) {
    tracing::warn!(
        chars,
        "telegraph publish unavailable; falling back to chunks"
    );
}

pub(crate) fn media_not_selected(stage: &str, detail: &str) {
    tracing::warn!(stage, detail, "media not selected");
}

pub(crate) fn download_failed(stage: &str, detail: &str, started: time::Instant) {
    tracing::warn!(
        stage,
        detail,
        elapsed_ms = elapsed_ms(started),
        "download failed"
    );
}

pub(crate) fn media_finished(
    found: usize,
    skipped: usize,
    delivered: usize,
    started: time::Instant,
) {
    tracing::info!(
        found,
        skipped,
        delivered,
        elapsed_ms = elapsed_ms(started),
        "media handled"
    );
}

pub(crate) fn chat_context(history: usize, has_summary: bool, prompt: &str) {
    tracing::debug!(
        history,
        has_summary,
        prompt_chars = prompt.chars().count(),
        "chat context loaded"
    );
}

pub(crate) fn llm_round(round: usize, max_tokens: u32, reasoning: bool, research: bool) {
    tracing::debug!(round, max_tokens, reasoning, research, "llm round");
}

pub(crate) fn chat_reply_delivered(reply: &ChatReply, delivered: bool, text: bool) {
    tracing::info!(
        delivered,
        text,
        sticker = reply.sticker.is_some(),
        decision = reply.decision.is_some(),
        "chat reply delivered"
    );
}

pub(crate) fn llm_request_built(messages: usize, bytes: usize, temperature: f64) {
    tracing::debug!(messages, bytes, temperature, "llm request built");
}

pub(crate) fn llm_response(
    started: time::Instant,
    finish_reason: &str,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) {
    tracing::info!(
        elapsed_ms = elapsed_ms(started),
        finish_reason = finish_kind(finish_reason),
        input_tokens,
        output_tokens,
        "llm response"
    );
}

fn cap(text: &str, max: usize) -> String {
    let mut short: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        short.push('…');
    }
    short
}
