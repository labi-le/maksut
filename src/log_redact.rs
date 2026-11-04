use crate::*;

/// One log line can be a whole multi-line upstream body interpolated into
/// `{err}`; the tool answer has a fixed character budget shared with the header.
pub(crate) const LINE_MAX_CHARS: usize = 500;

/// Raw sanitizer input ceiling. A single fmt event may carry a whole stderr
/// dump, and scanning it several times per call is avoidable work; the tail is
/// dropped before any pattern runs, so a secret past this point never survives.
const RAW_LINE_MAX_CHARS: usize = 4096;

/// Environment values masked in every returned line. Values shorter than
/// [`MIN_SECRET_CHARS`] are skipped so a test fixture or a one-letter secret
/// cannot redact ordinary words.
const SECRET_ENV: [&str; 3] = ["BOT_TOKEN", "LLM_API_KEY", "SEARCH_API_KEY"];
const MIN_SECRET_CHARS: usize = 6;
const MASK: &str = "[redacted]";

pub(crate) fn secret_values() -> Vec<String> {
    SECRET_ENV
        .iter()
        .filter_map(|name| env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| value.chars().count() >= MIN_SECRET_CHARS)
        .collect()
}

/// A log line safe to hand to the model: URL query strings and credentials are
/// replaced by [`MASK`], and the line is bounded to [`LINE_MAX_CHARS`].
pub(crate) fn sanitize_line(line: &str, secrets: &[String]) -> String {
    let line = head(line, RAW_LINE_MAX_CHARS).trim_end_matches(['\n', '\r']);
    let mut text = strip_url_queries(line);
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret.as_str(), MASK);
    }
    let text = mask_credential_tokens(&text);
    truncate_chars(&text, LINE_MAX_CHARS)
}

pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => {
            let mut truncated = text.split_at(end).0.to_owned();
            truncated.push('…');
            truncated
        }
        None => text.to_owned(),
    }
}

fn head(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((end, _)) => text.split_at(end).0,
        None => text,
    }
}

/// Query strings carry API keys and signed tokens, so only the URL up to `?`
/// survives.
fn strip_url_queries(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((index, marker_len)) = find_url(rest) {
        let (url_start, after_marker) = rest.split_at(index + marker_len);
        out.push_str(url_start);
        let end = after_marker
            .find(char::is_whitespace)
            .unwrap_or(after_marker.len());
        let (tail, after_tail) = after_marker.split_at(end);
        match tail.find('?') {
            Some(query_start) => {
                out.push_str(tail.split_at(query_start).0);
                out.push('?');
                out.push_str(MASK);
            }
            None => out.push_str(tail),
        }
        rest = after_tail;
    }
    out.push_str(rest);
    out
}

fn find_url(text: &str) -> Option<(usize, usize)> {
    let https = text.find("https://").map(|index| (index, "https://".len()));
    let http = text.find("http://").map(|index| (index, "http://".len()));
    match (https, http) {
        (Some(https), Some(http)) => Some(if https.0 <= http.0 { https } else { http }),
        (found, None) | (None, found) => found,
    }
}

/// `memory_guard::credential_like` decides what looks like a credential; the
/// whole whitespace-delimited token is replaced so no fragment of the value
/// survives.
fn mask_credential_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for chunk in text.split_inclusive(char::is_whitespace) {
        let token = chunk.trim_end_matches(char::is_whitespace);
        if !token.is_empty() && crate::memory_guard::credential_like(token) {
            out.push_str(MASK);
            out.push_str(chunk.split_at(token.len()).1);
        } else {
            out.push_str(chunk);
        }
    }
    out
}
