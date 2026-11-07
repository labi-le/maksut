//! `chat_search` argument parsing and the bounded lexical scorer.
//!
//! Kept apart from retrieval so `src/chat_search.rs` stays inside the module
//! size bound. Dates are MSK calendar days; the scorer only reads the query
//! words and one candidate row.

use crate::*;

pub(crate) struct SearchRequest {
    pub(crate) query: String,
    pub(crate) author_id: Option<i64>,
    pub(crate) since: Option<i64>,
    pub(crate) until: Option<i64>,
    pub(crate) limit: usize,
}

/// Parses `chat_search` arguments. `since`/`until` are `YYYY-MM-DD` dates in
/// MSK; a malformed date is a caller error, never silently dropped.
pub(crate) fn parse_search_request(args: &Value) -> Result<SearchRequest, String> {
    let raw = args.get("query").and_then(Value::as_str).unwrap_or("");
    let query = crate::chat_text::truncate_text(raw.trim(), config().memory.embedding_max_chars);
    if query.is_empty() {
        return Err("укажи query".into());
    }
    let author_id = match args.get("author_id").and_then(Value::as_i64) {
        Some(id) if id > 0 => Some(id),
        Some(_) => return Err("author_id должен быть положительным Telegram id".into()),
        None => None,
    };
    let since = parse_optional_date(args, "since")?;
    let until = parse_optional_date(args, "until")?;
    if let (Some(since), Some(until)) = (since, until) {
        if since >= until {
            return Err("since должен быть раньше until".into());
        }
    }
    let max = config().memory.search_limit.max(1);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(max)
        .clamp(1, max);
    Ok(SearchRequest {
        query,
        author_id,
        since,
        until,
        limit,
    })
}

fn parse_optional_date(args: &Value, key: &str) -> Result<Option<i64>, String> {
    let raw = args.get(key).and_then(Value::as_str).unwrap_or("").trim();
    if raw.is_empty() {
        return Ok(None);
    }
    parse_date_msk(raw)
        .map(Some)
        .ok_or_else(|| format!("{key}: ожидается дата YYYY-MM-DD"))
}

/// Midnight MSK of a `YYYY-MM-DD` date as unix seconds.
fn parse_date_msk(raw: &str) -> Option<i64> {
    let mut parts = raw.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1970..=2100).contains(&year) {
        return None;
    }
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    // A date without a time means the start of that day in MSK (UTC+3).
    Some(days_from_civil(year, month, day) * 86_400 - 3 * 3_600)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(crate) fn lexical_score(query_words: &[(&str, &str)], text: &str, author: &str) -> f32 {
    let text_lower = text.to_lowercase();
    let author_lower = author.to_lowercase();
    let mut score = 0.0f32;
    let mut matched = 0usize;
    for &(word, stem) in query_words {
        let exact_text = text_lower.contains(word);
        let stem_text = !exact_text && text_lower.contains(stem);
        let exact_author = author_lower.contains(word);
        if exact_text {
            score += 0.45;
        } else if stem_text {
            score += 0.30;
        } else if exact_author {
            score += 0.25;
        }
        if exact_text || stem_text || exact_author {
            matched += 1;
        }
    }
    if query_words.len() > 1 && matched == query_words.len() {
        score += 0.35;
    }
    score.min(1.0)
}

pub(crate) fn stem_slice(word: &str) -> &str {
    let count = word.chars().count();
    if count < 5 {
        return word;
    }
    let keep = count.saturating_sub(if count >= 6 { 2 } else { 1 });
    word.char_indices()
        .nth(keep)
        .and_then(|(idx, _)| word.get(..idx))
        .unwrap_or(word)
}
