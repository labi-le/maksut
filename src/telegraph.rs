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

/// Converts markdown-like reply text (paragraphs, fenced code blocks, and
/// `источники:` lines) into Telegraph Node JSON (`[{"tag": "p"|"pre", ...}]`).
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
            nodes.push(json!({"tag": "p", "children": [trimmed]}));
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
/// page, keeping the opening summary sentence/paragraph and the article URL.
pub(crate) fn format_telegraph_chat_message(text: &str, page_url: &str) -> String {
    let first_prose = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("```"))
        .unwrap_or("ответ");
    let preview = shorten(first_prose, TELEGRAPH_PREVIEW_CHARS).trim_end_matches(['.', '…']);
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
/// credential (`credential_like`).
pub(crate) fn article_is_publishable(text: &str) -> bool {
    !crate::memory_guard::credential_like(text)
}

pub(crate) async fn publish_telegraph_with_base(
    base: &Url,
    title: &str,
    text: &str,
) -> Option<String> {
    if !article_is_publishable(text) {
        return None;
    }
    let client = telegraph_client(base).await?;
    let token = ensure_telegraph_token(&client, base).await?;
    let nodes = markdown_to_telegraph_nodes(text);
    let clean_title: String = title
        .lines()
        .next()
        .unwrap_or("Maksut")
        .trim()
        .chars()
        .take(80)
        .collect();
    let title = if clean_title.is_empty() {
        "Maksut"
    } else {
        clean_title.as_str()
    };
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
/// `config().chat.reply_max_chars` (or `>= 200` chars on an explicit `telegra.ph`
/// request, `>= 500` chars when `prompt` asks for an article/detailed breakdown,
/// or `>= 1800` chars across multiple paragraphs/code blocks), returning the
/// formatted short chat message with the `telegra.ph` link.
pub(crate) async fn maybe_publish_reply(title: &str, prompt: &str, text: &str) -> Option<String> {
    if !should_publish_telegraph(prompt, text) {
        return None;
    }
    let base = Url::parse(&format!("https://{TELEGRAPH_HOST}/")).ok()?;
    let page_url = publish_telegraph_with_base(&base, title, text).await?;
    Some(format_telegraph_chat_message(text, &page_url))
}

pub(crate) fn should_publish_telegraph(prompt: &str, text: &str) -> bool {
    let count = text.chars().count();
    let over_limit = count > config().chat.reply_max_chars;
    let direct_telegraph = explicit_telegraph_request(prompt) && count >= 200;
    let long_form = (explicit_article_request(prompt) && count >= 500)
        || (text.contains('\n') && count >= 1800);
    over_limit || direct_telegraph || long_form
}

pub(crate) fn explicit_telegraph_request(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    ["телеграф", "telegra.ph", "telegraph"]
        .iter()
        .any(|kw| lower.contains(kw))
}

pub(crate) fn explicit_article_request(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    explicit_telegraph_request(prompt)
        || ["стать", "подробн", "развёрн", "разверн", "лонгрид"]
            .iter()
            .any(|kw| lower.contains(kw))
}
