//! Publishes long-form or explicitly requested article/code replies to `telegra.ph`
//! (`>= 200` chars on explicit `telegra.ph` requests, `>= 500` chars on explicit
//! article/detail requests, `>= 1800` chars across multiple lines/paragraphs/code
//! blocks, or `> chat.reply_max_chars`), returning a concise chat message with a
//! brief preview and the verified `https://telegra.ph/...` article link; falls back
//! to `split_message` when Telegraph is unreachable or the text carries a credential.

use crate::chat_text::{fence_open_ticks, is_fence_close, shorten};
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

static TELEGRAPH_TOKEN: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Drops the backslash escapes Telegram's MarkdownV2 dialect puts before
/// punctuation (`\-`, `\.`), which the model emits for chat rendering.
fn unescape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                if next.is_ascii_punctuation() {
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
        }
        out.push(ch);
    }
    out
}

/// Removes emphasis markers and escapes from a line of prose.
fn strip_marks(text: &str) -> String {
    unescape_markdown(text).replace("**", "").replace("__", "")
}

/// `Some((hashes, text))` for a `#`-prefixed heading line.
fn heading_line(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim();
    let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
    let rest = trimmed.trim_start_matches('#').trim_start();
    ((1..=6).contains(&hashes) && !rest.is_empty()).then(|| (hashes, strip_marks(rest)))
}

/// `Some(text)` when the whole line is one `**bold**` run; the model uses that
/// shape for section titles.
fn bold_heading(line: &str) -> Option<String> {
    let inner = line.trim().strip_prefix("**")?.strip_suffix("**")?;
    (!inner.contains("**") && !inner.trim().is_empty()).then(|| unescape_markdown(inner.trim()))
}

fn plain_line(line: &str) -> String {
    bold_heading(line).unwrap_or_else(|| strip_marks(line.trim_start_matches('#').trim_start()))
}
/// Lines that are prose, i.e. outside fenced code blocks, trimmed of `\r`.
fn prose_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut fence_ticks = 0usize;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
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
        out.push(line);
    }
    out
}

fn explicit_title_line(line: &str) -> Option<String> {
    let raw = heading_line(line)
        .map(|(_, t)| t)
        .or_else(|| bold_heading(line))?;
    let title: String = raw.chars().take(80).collect();
    let title = title.trim().to_owned();
    (!title.is_empty()).then_some(title)
}

/// Clean title from the user's prompt when the reply itself opens with prose
/// banter instead of an explicit `#` or `**bold**` heading.
pub(crate) fn prompt_title(prompt: &str) -> Option<String> {
    let mut cleaned = prompt.trim().to_owned();
    if cleaned.is_empty() {
        return None;
    }
    for name in crate::addressing::BOT_NAME_FORMS {
        if let (rest, true) = crate::addressing::strip_name(&cleaned, name) {
            cleaned = rest;
        }
    }
    let mut s = crate::addressing::trim_name_edges(&cleaned);
    const PREFIXES: &[&str] = &[
        "напиши подробную статью про ",
        "напиши статью про ",
        "напиши статью о ",
        "напиши статью ",
        "сделай статью про ",
        "сделай статью ",
        "дай инструкцию ",
        "напиши инструкцию ",
        "расскажи подробно про ",
        "расскажи про ",
        "объясни подробно ",
        "объясни ",
        "разбери подробно ",
        "разбери ",
    ];
    for p in PREFIXES {
        if let Some(rest) = s
            .get(..p.len())
            .filter(|h| h.eq_ignore_ascii_case(p))
            .and_then(|_| s.get(p.len()..))
        {
            s = rest.trim();
            break;
        }
    }
    let c: String = plain_line(s).chars().take(80).collect();
    let c = c.trim().to_owned();
    (c.chars().count() >= 4 && !c.contains("://")).then_some(c)
}

/// First prose line of the reply when it is an explicit heading or short title;
/// falls back to `Maksut` when the opening line is conversational prose.
pub(crate) fn article_title(text: &str) -> String {
    let line = prose_lines(text)
        .into_iter()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    if let Some(explicit) = explicit_title_line(line) {
        return explicit;
    }
    let plain = plain_line(line);
    let t = plain.trim();
    if !t.is_empty() && t.chars().count() <= 72 && !t.contains("://") && !is_banter_opening(t) {
        return t.to_owned();
    }
    "Maksut".to_owned()
}

fn is_banter_opening(line: &str) -> bool {
    let l = line.to_lowercase();
    const P: &[&str] = &[
        "сладкий,",
        "повелитель,",
        "император,",
        "держи ",
        "давай,",
        "слушай,",
        "смотри,",
        "короче,",
        "вот ",
        "по факту,",
        "согласен,",
    ];
    P.iter().any(|p| l.starts_with(p))
        || l.contains(", сладкий")
        || l.contains(", повелитель")
        || l.ends_with('.')
}

/// Drops the title line so the published page does not repeat it above the body.
/// Only a prose line can be the title, so a reply that opens with fenced code
/// still loses the first text line rather than a code line.
pub(crate) fn body_after_title(text: &str, title: &str) -> String {
    let mut body = String::with_capacity(text.len());
    let mut fence_ticks = 0usize;
    let mut first = true;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let prose = fence_ticks == 0 && fence_open_ticks(line).is_none();
        if fence_ticks > 0 {
            if is_fence_close(line, fence_ticks) {
                fence_ticks = 0;
            }
        } else if let Some(ticks) = fence_open_ticks(line) {
            fence_ticks = ticks;
        }
        if first && prose {
            if line.trim().is_empty() {
                continue;
            }
            first = false;
            let candidate: String = plain_line(line).chars().take(80).collect();
            if candidate.trim() == title {
                continue;
            }
        }
        body.push_str(line);
        body.push('\n');
    }
    body
}

/// Paragraph children: paired inline `code` spans become `code` nodes,
/// everything else is prose with emphasis markers and Backslash escapes
/// removed. An odd backtick opens nothing, so it stays literal text.
fn inline_nodes(text: &str) -> Vec<Value> {
    let mut nodes = Vec::new();
    let mut buf = String::new();
    let parts: Vec<&str> = text.split('`').collect();
    let paired = parts.len() % 2 == 1;
    let last = parts.len().saturating_sub(1);
    for (index, part) in parts.iter().enumerate() {
        if !paired && index == last {
            buf.push('`');
            buf.push_str(&strip_marks(part));
            continue;
        }
        if index % 2 == 1 {
            if !buf.is_empty() {
                nodes.push(Value::String(std::mem::take(&mut buf)));
            }
            if !part.is_empty() {
                nodes.push(json!({"tag": "code", "children": [part]}));
            }
            continue;
        }
        buf.push_str(&strip_marks(part));
    }
    if !buf.is_empty() {
        nodes.push(Value::String(buf));
    }
    if nodes.is_empty() {
        nodes.push(Value::String(String::new()));
    }
    nodes
}

/// Converts markdown-like reply text (paragraphs, headings, fenced code blocks,
/// and `источники:` lines) into Telegraph Node JSON (`p`, `h3`/`h4`, `pre`,
/// inline `code`).
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
    let push_heading = |nodes: &mut Vec<Value>, tag: &str, text: String, used: &mut usize| {
        if *used < TELEGRAPH_MAX_NODES_BYTES {
            *used += text.len() + 32;
            nodes.push(json!({"tag": tag, "children": [text]}));
        }
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
            let tag = if level <= 2 { "h3" } else { "h4" };
            push_heading(&mut nodes, tag, text, &mut used_bytes);
            continue;
        }
        if let Some(text) = bold_heading(line) {
            flush_para(&mut nodes, &mut para_buf, &mut used_bytes);
            push_heading(&mut nodes, "h3", text, &mut used_bytes);
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
        nodes.push(json!({"tag": "p", "children": [shorten(text, 2000)]}));
    }
    nodes
}

/// Formats the short chat summary message pointing to the published Telegraph
/// page: the first prose line after the article title, freed of markdown
/// markers, plus the article URL.
pub(crate) fn format_telegraph_chat_message(text: &str, page_url: &str) -> String {
    let title = article_title(text);
    let preview = prose_lines(text)
        .into_iter()
        .map(|line| plain_line(line.trim()))
        .find(|line| !line.is_empty() && *line != title)
        .unwrap_or_else(|| title.clone());
    let preview = shorten(&preview, TELEGRAPH_PREVIEW_CHARS);
    let preview = preview.trim_end_matches(['.', '…']);
    format!("{preview}\n\nполный текст и код: {page_url}")
}

/// Validates that a URL returned by the Telegraph API is a safe `https://telegra.ph/<slug>` link.
pub(crate) fn valid_telegraph_url(raw: &str) -> Option<String> {
    let u = Url::parse(raw.trim()).ok()?;
    let ok = u.scheme() == "https"
        && u.host_str() == Some("telegra.ph")
        && u.username().is_empty()
        && u.password().is_none()
        && u.port().is_none()
        && u.query().is_none()
        && u.path().len() > 1;
    ok.then(|| u.into())
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
    let body = json!({"short_name": "maksut", "author_name": "Maksut"});
    let resp = post_telegraph_json(client, base.join("createAccount").ok()?, &body).await?;
    let token = resp
        .pointer("/result/access_token")
        .and_then(Value::as_str)?
        .trim()
        .to_owned();
    (!token.is_empty()).then(|| token.clone())?;
    *TELEGRAPH_TOKEN.lock().await = Some(token.clone());
    Some(token)
}

/// Article text leaves the process only when it carries no high-confidence
/// credential (`credential_like`).
pub(crate) fn article_is_publishable(text: &str) -> bool {
    !crate::memory_guard::credential_like(text)
}

/// Page title and body nodes for one reply, the single composition point for
/// the published article: the title is the reply's first line and leaves the body.
pub(crate) fn telegraph_document(text: &str) -> (String, Vec<Value>) {
    let title = article_title(text);
    let body = body_after_title(text, &title);
    (title, markdown_to_telegraph_nodes(&body))
}

pub(crate) async fn publish_telegraph_with_prompt(
    base: &Url,
    prompt: &str,
    text: &str,
) -> Option<String> {
    if !article_is_publishable(text) {
        return None;
    }
    let client = telegraph_client(base).await?;
    let token = ensure_telegraph_token(&client, base).await?;
    let (mut t, nodes) = telegraph_document(text);
    if t == "Maksut" {
        if let Some(p) = prompt_title(prompt) {
            t = p;
        }
    }
    let title = t.lines().next().unwrap_or("Maksut").trim();
    let title = if title.is_empty() { "Maksut" } else { title };
    let payload = json!({
        "access_token": token,
        "title": title,
        "author_name": "Maksut",
        "content": nodes,
        "return_content": false
    });
    let resp = post_telegraph_json(&client, base.join("createPage").ok()?, &payload).await?;
    valid_telegraph_url(resp.pointer("/result/url").and_then(Value::as_str)?)
}

/// Publishes `text` to `https://api.telegra.ph` when `text` exceeds
/// `config().chat.reply_max_chars` (or `>= 200` chars on an explicit `telegra.ph`
/// request, `>= 500` chars when `prompt` asks for an article/detailed breakdown,
/// or `>= 1800` chars across multiple paragraphs/code blocks), returning the
/// formatted short chat message with the `telegra.ph` link.
pub(crate) async fn maybe_publish_reply(prompt: &str, text: &str) -> Option<String> {
    if !should_publish_telegraph(prompt, text) {
        return None;
    }
    let base = Url::parse(&format!("https://{TELEGRAPH_HOST}/")).ok()?;
    let page_url = publish_telegraph_with_prompt(&base, prompt, text).await?;
    Some(format_telegraph_chat_message(text, &page_url))
}

pub(crate) fn should_publish_telegraph(prompt: &str, text: &str) -> bool {
    let count = text.chars().count();
    let over_limit = count > config().chat.reply_max_chars;
    let direct_telegraph = explicit_telegraph_request(prompt) && count >= 200;
    let long_form = explicit_article_request(prompt) && count >= 500;
    over_limit || direct_telegraph || long_form
}

pub(crate) fn explicit_telegraph_request(prompt: &str) -> bool {
    let l = prompt.to_lowercase();
    ["телеграф", "telegra.ph", "telegraph"]
        .iter()
        .any(|k| l.contains(k))
}

pub(crate) fn explicit_article_request(prompt: &str) -> bool {
    let l = prompt.to_lowercase();
    const PROP: &[&str] = &["пропоз", "хотелк", "бэклог", "ишью", "issue", "задач"];
    const ART: &[&str] = &["стать", "подробн", "развёрн", "разверн", "лонгрид"];
    explicit_telegraph_request(prompt)
        || (!PROP.iter().any(|k| l.contains(k)) && ART.iter().any(|k| l.contains(k)))
}
