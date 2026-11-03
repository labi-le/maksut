//! Publishes long-form or explicitly requested article/code replies to `telegra.ph`
//! (`>= 900` chars on explicit article/detail requests, `>= 1800` chars across
//! multiple lines/paragraphs/code blocks, or `> chat.reply_max_chars`), returning
//! a concise chat message with a brief preview and the verified `https://telegra.ph/...`
//! article link; falls back to `split_message` when Telegraph is unreachable or
//! the text carries a credential (`credential_like`).

use crate::chat_text::{fence_open_ticks, find_closing_ticks, is_fence_close, shorten};
use crate::config::config;
use crate::permitted_ipv4;
use crate::prelude::*;
use serde_json::{json, Value};
use std::sync::LazyLock;
use tokio::sync::Mutex;

const TELEGRAPH_HOST: &str = "api.telegra.ph";
const TELEGRAPH_MAX_NODES_BYTES: usize = 60 * 1024;
const TELEGRAPH_MAX_RESPONSE_BYTES: usize = 16 * 1024;
const TELEGRAPH_PREVIEW_CHARS: usize = 500;
const TELEGRAPH_TITLE_CHARS: usize = 80;
const TELEGRAPH_FALLBACK_TITLE: &str = "Maksut";

static TELEGRAPH_TOKEN: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Removes what the model writes for the Telegram MarkdownV2 rendering: the
/// backslash escapes it puts before punctuation (`\-`, `\.`) and the doubled
/// `**`/`__` emphasis markers. Single `*` and `_` survive because Calendar
/// specs (`*-*-*`) and identifiers use them as data.
fn strip_marks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.peek().copied() {
                Some(next) if next.is_ascii_punctuation() => {
                    out.push(next);
                    chars.next();
                }
                _ => out.push(ch),
            },
            '`' => {}
            '*' | '_' => {
                if chars.peek() == Some(&ch) {
                    chars.next();
                } else {
                    out.push(ch);
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

/// `Some((hashes, text))` for a `#`-prefixed heading line.
fn heading_line(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim();
    let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = trimmed.trim_start_matches('#').trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((hashes, strip_marks(rest)))
}

/// `Some(text)` when the whole line is one `**bold**` run, the shape the model
/// uses for section titles.
fn bold_heading(line: &str) -> Option<String> {
    let inner = line.trim().strip_prefix("**")?.strip_suffix("**")?;
    if inner.contains("**") || inner.trim().is_empty() {
        return None;
    }
    Some(strip_marks(inner))
}

/// The line as reader-facing text: heading markers gone, emphasis gone.
fn plain_line(line: &str) -> String {
    if let Some((_, text)) = heading_line(line) {
        return text;
    }
    bold_heading(line).unwrap_or_else(|| strip_marks(line))
}

/// `(index, text)` of the first line at or after `from` that carries prose,
/// skipping fenced code blocks the same way `markdown_to_telegraph_nodes` does;
/// the model writes that line as the article title.
fn prose_line(text: &str, from: usize) -> Option<(usize, &str)> {
    let mut fence_ticks = 0;
    for (index, line) in text.lines().enumerate() {
        if fence_ticks > 0 {
            if is_fence_close(line, fence_ticks) {
                fence_ticks = 0;
            }
            continue;
        }
        if let Some(ticks) = fence_open_ticks(line) {
            fence_ticks = ticks;
            continue;
        }
        let trimmed = line.trim();
        if index >= from && !trimmed.is_empty() {
            return Some((index, trimmed));
        }
    }
    None
}

/// Page title for a reply: its first prose line without markers, `Maksut` when
/// the reply has no usable line.
pub(crate) fn article_title(text: &str) -> String {
    let title = prose_line(text, 0)
        .map(|(_, line)| plain_line(line))
        .unwrap_or_default();
    let title: String = title.trim().chars().take(TELEGRAPH_TITLE_CHARS).collect();
    if title.trim().is_empty() {
        TELEGRAPH_FALLBACK_TITLE.to_owned()
    } else {
        title
    }
}

/// Paragraph children: inline `code` spans become `code` nodes, everything else
/// is prose with the markers stripped. Backtick runs pair by equal length as in
/// markdown, so ```` ``code`` ```` is one span; an unmatched run is a typo and
/// its text stays prose instead of swallowing the rest of the paragraph.
fn inline_nodes(text: &str) -> Vec<Value> {
    let mut nodes = Vec::new();
    let mut prose = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        prose.push_str(&strip_marks(rest.get(..open).unwrap_or_default()));
        let after_prefix = rest.get(open..).unwrap_or_default();
        let ticks = after_prefix.len() - after_prefix.trim_start_matches('`').len();
        let after_open = after_prefix.get(ticks..).unwrap_or_default();
        let Some(close) = find_closing_ticks(after_open, ticks) else {
            prose.push_str(&strip_marks(after_open));
            rest = "";
            break;
        };
        if !prose.is_empty() {
            nodes.push(Value::String(std::mem::take(&mut prose)));
        }
        let span = after_open.get(..close).unwrap_or_default();
        let inner = if ticks > 1 && span.len() > 2 && span.starts_with(' ') && span.ends_with(' ') {
            span.get(1..span.len() - 1).unwrap_or(span)
        } else {
            span
        };
        if !inner.is_empty() {
            nodes.push(json!({"tag": "code", "children": [inner]}));
        }
        rest = after_open.get(close + ticks..).unwrap_or_default();
    }
    prose.push_str(&strip_marks(rest));
    if !prose.is_empty() {
        nodes.push(Value::String(prose));
    }
    if nodes.is_empty() {
        nodes.push(Value::String(String::new()));
    }
    nodes
}

/// Page title and body nodes for one reply: the title is the reply's first
/// prose line and that line leaves the body, so the page does not repeat it.
pub(crate) fn telegraph_document(text: &str) -> (String, Vec<Value>) {
    let title = article_title(text);
    let title_line = prose_line(text, 0).map(|(index, _)| index);
    let mut body = String::with_capacity(text.len());
    for (index, line) in text.lines().enumerate() {
        if Some(index) == title_line {
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    (title, markdown_to_telegraph_nodes(&body))
}

/// Converts markdown-like reply text (paragraphs, `#`/`**bold**` headings,
/// fenced code blocks, and `источники:` lines) into Telegraph Node JSON
/// (`[{"tag": "p"|"h3"|"h4"|"pre", ...}]`), with inline backticks as `code`.
pub(crate) fn markdown_to_telegraph_nodes(text: &str) -> Vec<Value> {
    let mut nodes = Vec::new();
    let mut in_code = false;
    let mut fence_ticks = 0;
    let mut code_buf = String::new();
    let mut para_buf = String::new();
    let mut used_bytes = 0usize;

    let flush_para = |nodes: &mut Vec<Value>, para: &mut String, used: &mut usize| {
        let trimmed = para.trim();
        if !trimmed.is_empty() && *used < TELEGRAPH_MAX_NODES_BYTES {
            *used += trimmed.len() + 32;
            nodes.push(json!({"tag": "p", "children": inline_nodes(trimmed)}));
        }
        para.clear();
    };
    let flush_code = |nodes: &mut Vec<Value>, code: &mut String, used: &mut usize| {
        let trimmed = code.trim_end_matches('\n');
        if !trimmed.is_empty() && *used < TELEGRAPH_MAX_NODES_BYTES {
            *used += trimmed.len() + 48;
            nodes.push(json!({"tag": "pre", "children": [trimmed]}));
        }
        code.clear();
    };

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if in_code {
            if is_fence_close(line, fence_ticks) {
                flush_code(&mut nodes, &mut code_buf, &mut used_bytes);
                in_code = false;
                fence_ticks = 0;
            } else {
                if !code_buf.is_empty() {
                    code_buf.push('\n');
                }
                code_buf.push_str(line);
            }
            continue;
        }
        if let Some(ticks) = fence_open_ticks(line) {
            flush_para(&mut nodes, &mut para_buf, &mut used_bytes);
            in_code = true;
            fence_ticks = ticks;
            continue;
        }
        if line.trim().is_empty() {
            flush_para(&mut nodes, &mut para_buf, &mut used_bytes);
            continue;
        }
        if let Some((level, text)) = heading_line(line) {
            flush_para(&mut nodes, &mut para_buf, &mut used_bytes);
            if used_bytes < TELEGRAPH_MAX_NODES_BYTES {
                used_bytes += text.len() + 32;
                let tag = if level <= 2 { "h3" } else { "h4" };
                nodes.push(json!({"tag": tag, "children": inline_nodes(&text)}));
            }
            continue;
        }
        if let Some(text) = bold_heading(line) {
            flush_para(&mut nodes, &mut para_buf, &mut used_bytes);
            if used_bytes < TELEGRAPH_MAX_NODES_BYTES {
                used_bytes += text.len() + 32;
                nodes.push(json!({"tag": "h3", "children": inline_nodes(&text)}));
            }
            continue;
        }
        if !para_buf.is_empty() {
            para_buf.push('\n');
        }
        para_buf.push_str(line);
        if para_buf.len() >= 1500 {
            flush_para(&mut nodes, &mut para_buf, &mut used_bytes);
        }
    }
    if in_code {
        flush_code(&mut nodes, &mut code_buf, &mut used_bytes);
    }
    flush_para(&mut nodes, &mut para_buf, &mut used_bytes);
    if nodes.is_empty() {
        nodes.push(json!({"tag": "p", "children": inline_nodes(shorten(text, 2000))}));
    }
    nodes
}

/// Formats the short chat summary message pointing to the published Telegraph
/// page: the first prose line after the article title, with markdown markers
/// removed, plus the article URL.
pub(crate) fn format_telegraph_chat_message(text: &str, page_url: &str) -> String {
    let title = article_title(text);
    let after_title = prose_line(text, 0).map_or(0, |(index, _)| index + 1);
    let preview = prose_line(text, after_title)
        .map(|(_, line)| plain_line(line))
        .unwrap_or_else(|| title.clone());
    let preview = shorten(&preview, TELEGRAPH_PREVIEW_CHARS);
    let preview = preview.trim_end_matches(['.', '…']);
    format!("{preview}\n\nполный текст и код: {page_url}")
}

/// Validates that a URL returned by the Telegraph API is a safe `https://telegra.ph/<slug>` link.
pub(crate) fn valid_telegraph_url(raw: &str) -> Option<String> {
    let url = Url::parse(raw.trim()).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("telegra.ph")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.path().len() <= 1
    {
        return None;
    }
    Some(url.into())
}

async fn telegraph_client(base: &Url) -> Option<reqwest::Client> {
    let host = base.host_str()?;
    let port = base.port_or_known_default().unwrap_or(443);
    let timeout = Duration::from_secs(config().read_url.timeout_secs.min(10));
    let addrs: Vec<SocketAddr> = time::timeout(timeout, lookup_host((host, port)))
        .await
        .ok()?
        .ok()?
        .filter(|addr| permitted_ipv4(addr.ip()))
        .collect();
    if addrs.is_empty() {
        return None;
    }
    reqwest::Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .https_only(true)
        .timeout(timeout)
        .resolve_to_addrs(host, &addrs)
        .build()
        .ok()
}

async fn post_telegraph_json(client: &reqwest::Client, url: Url, body: &Value) -> Option<Value> {
    let mut response = client.post(url).json(body).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > TELEGRAPH_MAX_RESPONSE_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    let parsed: Value = serde_json::from_slice(&bytes).ok()?;
    (parsed.get("ok").and_then(Value::as_bool) == Some(true)).then_some(parsed)
}

async fn ensure_telegraph_token(client: &reqwest::Client, base: &Url) -> Option<String> {
    if let Some(token) = TELEGRAPH_TOKEN.lock().await.clone() {
        return Some(token);
    }
    let create_url = base.join("createAccount").ok()?;
    let body = json!({
        "short_name": "maksut",
        "author_name": "Maksut"
    });
    let resp = post_telegraph_json(client, create_url, &body).await?;
    let token = resp
        .pointer("/result/access_token")
        .and_then(Value::as_str)?
        .trim()
        .to_owned();
    if token.is_empty() {
        return None;
    }
    *TELEGRAPH_TOKEN.lock().await = Some(token.clone());
    Some(token)
}

/// Article text leaves the process only when it carries no high-confidence
/// credential. `detect_flags` flags `FLAG_SECRET` on 40+ ASCII alphanumerics
/// counted across the whole text plus one digit, so it marks ordinary technical
/// prose; publication uses `credential_like` instead.
pub(crate) fn article_is_publishable(text: &str) -> bool {
    !crate::memory_guard::credential_like(text)
}

pub(crate) async fn publish_telegraph_with_base(base: &Url, text: &str) -> Option<String> {
    if !article_is_publishable(text) {
        return None;
    }
    let client = telegraph_client(base).await?;
    let token = ensure_telegraph_token(&client, base).await?;
    let (title, nodes) = telegraph_document(text);
    let create_page = base.join("createPage").ok()?;
    let payload = json!({
        "access_token": token,
        "title": title,
        "author_name": "Maksut",
        "content": nodes,
        "return_content": false
    });
    let resp = post_telegraph_json(&client, create_page, &payload).await?;
    let url = resp.pointer("/result/url").and_then(Value::as_str)?;
    valid_telegraph_url(url)
}

/// Publishes `text` to `https://api.telegra.ph` when `text` exceeds
/// `config().chat.reply_max_chars` (or `>= 900` chars when `prompt` explicitly
/// asks for an article/detailed breakdown, or `>= 1800` chars across multiple
/// paragraphs/code blocks), returning the formatted short chat message with the
/// `telegra.ph` link. The page title comes from the reply itself.
pub(crate) async fn maybe_publish_reply(prompt: &str, text: &str) -> Option<String> {
    if !should_publish_telegraph(prompt, text) {
        return None;
    }
    let base = Url::parse(&format!("https://{TELEGRAPH_HOST}/")).ok()?;
    let page_url = publish_telegraph_with_base(&base, text).await?;
    Some(format_telegraph_chat_message(text, &page_url))
}

pub(crate) fn should_publish_telegraph(prompt: &str, text: &str) -> bool {
    let count = text.chars().count();
    let over_limit = count > config().chat.reply_max_chars;
    let long_form = (explicit_article_request(prompt) && count >= 900)
        || (text.contains('\n') && count >= 1800);
    over_limit || long_form
}

pub(crate) fn explicit_article_request(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    [
        "стать",
        "телеграф",
        "telegra.ph",
        "подробн",
        "развёрн",
        "разверн",
        "лонгрид",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}
