use crate::prelude::*;
use crate::search::SearchHit;
use serde_json::Value;

pub(crate) fn parse_mcp_jsonrpc_result(bytes: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.starts_with("event:") || trimmed.contains("\ndata:") || trimmed.starts_with("data:")
    {
        for line in trimmed.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(data) {
                if let Some(result) = extract_rpc_result(&value) {
                    return Some(result);
                }
            }
        }
        return None;
    }
    let value: Value = serde_json::from_slice(bytes).ok()?;
    extract_rpc_result(&value)
}

fn extract_rpc_result(value: &Value) -> Option<Value> {
    if let Some(items) = value.as_array() {
        for item in items {
            if let Some(result) = extract_rpc_result(item) {
                return Some(result);
            }
        }
        return None;
    }
    if value.get("error").is_some() {
        return None;
    }
    let result = value.get("result")?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    Some(result.clone())
}

pub(crate) fn parse_parallel_mcp_hits(result: &Value, limit: usize) -> Vec<SearchHit> {
    if let Some(structured) = result.get("structuredContent") {
        let hits = parse_parallel_payload(structured, limit);
        if !hits.is_empty() {
            return hits;
        }
    }
    if let Some(items) = result.get("content").and_then(Value::as_array) {
        for item in items {
            let Some(text) = item.get("text").and_then(Value::as_str) else {
                continue;
            };
            if let Ok(payload) = serde_json::from_str::<Value>(text) {
                let hits = parse_parallel_payload(&payload, limit);
                if !hits.is_empty() {
                    return hits;
                }
            }
        }
    }
    parse_parallel_payload(result, limit)
}

fn parse_parallel_payload(payload: &Value, limit: usize) -> Vec<SearchHit> {
    payload
        .get("results")
        .and_then(Value::as_array)
        .map(|items| parse_json_hits(items, limit))
        .unwrap_or_default()
}

pub(crate) fn parse_exa_mcp_hits(result: &Value, limit: usize) -> Vec<SearchHit> {
    for candidate in [
        result.get("structuredContent"),
        result.get("data"),
        result.get("result"),
        Some(result),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(items) = candidate.get("results").and_then(Value::as_array) {
            let hits = parse_json_hits(items, limit);
            if !hits.is_empty() {
                return hits;
            }
        }
    }
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    for item in content {
        let Some(text) = item.get("text").and_then(Value::as_str) else {
            continue;
        };
        if let Ok(parsed) = serde_json::from_str::<Value>(text) {
            if let Some(items) = parsed.get("results").and_then(Value::as_array) {
                let hits = parse_json_hits(items, limit);
                if !hits.is_empty() {
                    return hits;
                }
            }
        }
        if !text.trim().is_empty() {
            blocks.push(text.replace("\r\n", "\n"));
        }
    }
    parse_exa_text_blocks(&blocks.join("\n\n"), limit)
}

fn parse_json_hits(items: &[Value], limit: usize) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    for item in items.iter().take(limit) {
        let Some(url) = item
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| !u.trim().is_empty())
        else {
            continue;
        };
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(url)
            .to_owned();
        let snippet = item
            .get("summary")
            .or_else(|| item.get("text"))
            .or_else(|| item.get("snippet"))
            .or_else(|| item.get("description"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                item.get("excerpts")
                    .or_else(|| item.get("highlights"))
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
            })
            .unwrap_or_default();
        hits.push(SearchHit {
            title,
            url: url.to_owned(),
            snippet,
        });
    }
    hits
}

fn parse_exa_text_blocks(raw: &str, limit: usize) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    for section in raw.split("Title:") {
        if hits.len() >= limit {
            break;
        }
        let section = section.trim();
        if section.is_empty() {
            continue;
        }
        let mut lines = section.lines();
        let title = lines.next().unwrap_or("").trim().to_owned();
        let mut url = String::new();
        let mut text = String::new();
        let mut in_text = false;
        for line in lines {
            if in_text {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(line.trim());
            } else if let Some(rest) = line.strip_prefix("URL:") {
                url = rest.trim().to_owned();
            } else if let Some(rest) = line.strip_prefix("Text:") {
                in_text = true;
                text.push_str(rest.trim());
            }
        }
        if !url.is_empty() {
            hits.push(SearchHit {
                title: if title.is_empty() { url.clone() } else { title },
                url,
                snippet: text,
            });
        }
    }
    hits
}

pub(crate) fn extract_search_results(html: &str, limit: usize) -> Vec<(String, String, String)> {
    let mut results = Vec::new();
    let mut cursor = 0;
    while results.len() < limit {
        let Some(relative) = html
            .get(cursor..)
            .and_then(|tail| tail.find("class=\"result__a\""))
        else {
            break;
        };
        let marker = cursor + relative;
        let Some(tag_relative) = html.get(..marker).and_then(|head| head.rfind("<a ")) else {
            break;
        };
        let tag_end = html
            .get(marker..)
            .and_then(|tail| tail.find('>'))
            .map(|index| marker + index + 1)
            .unwrap_or(html.len());
        let tag = html.get(tag_relative..tag_end).unwrap_or_default();
        let href = tag
            .find("href=\"")
            .and_then(|start| {
                tag.get(start + 6..)
                    .and_then(|tail| tail.find('"'))
                    .and_then(|end| tag.get(start + 6..start + 6 + end))
            })
            .map(decode_link)
            .unwrap_or_default();
        let title_end = html
            .get(tag_end..)
            .and_then(|tail| tail.find("</a>"))
            .map(|index| tag_end + index)
            .unwrap_or(html.len());
        let title = clean_text(html.get(tag_end..title_end).unwrap_or_default());
        let next_result = html
            .get(tag_end..)
            .and_then(|t| t.find("class=\"result__a\""))
            .map(|i| tag_end + i)
            .unwrap_or(html.len());
        let snippet = html
            .get(title_end..next_result)
            .and_then(extract_ddg_snippet)
            .unwrap_or_default();
        if !href.is_empty() {
            results.push((title, href, snippet));
        }
        cursor = tag_end.max(marker + 1);
    }
    results
}

fn extract_ddg_snippet(window: &str) -> Option<String> {
    let marker = window.find("class=\"result__snippet\"")?;
    let text_start = marker + window.get(marker..)?.find('>')? + 1;
    let tail = window.get(text_start..)?;
    let end_rel = tail
        .find("</a>")
        .or_else(|| tail.find("</div>"))
        .or_else(|| tail.find("</span>"))?;
    Some(clean_text(tail.get(..end_rel)?))
}

pub(crate) fn extract_startpage_results(html: &str, limit: usize) -> Vec<SearchHit> {
    extract_dom_results(
        html,
        limit,
        "div.result, div.w-gl__result",
        "a.result-link, a.w-gl__result-title",
        Some("h2, h3"),
        "p.description, p.w-gl__description",
        "startpage.com",
    )
}

pub(crate) fn extract_mojeek_results(html: &str, limit: usize) -> Vec<SearchHit> {
    extract_dom_results(
        html,
        limit,
        "ul.results-standard > li",
        "h2 a.title, a.title",
        None,
        "p.s",
        "mojeek.",
    )
}

pub(crate) fn extract_ecosia_results(html: &str, limit: usize) -> Vec<SearchHit> {
    extract_dom_results(
        html,
        limit,
        "article[data-test-id='organic-result']",
        "a[href]",
        Some("[data-test-id='result-title']"),
        "[data-test-id='web-result-description'], [data-test-id='result-description']",
        "ecosia.org",
    )
}

fn extract_dom_results(
    html: &str,
    limit: usize,
    item_sel: &str,
    link_sel: &str,
    title_sel: Option<&str>,
    snippet_sel: &str,
    self_domain: &str,
) -> Vec<SearchHit> {
    if crate::read_url::html_is_too_complex(html) {
        return Vec::new();
    }
    let document = dom_query::Document::from(html);
    let mut hits = Vec::new();
    for item in document.select(item_sel).iter() {
        if hits.len() >= limit {
            break;
        }
        let anchor = item.select_single(link_sel);
        let Some(href) = anchor.attr("href") else {
            continue;
        };
        let Ok(url) = Url::parse(href.trim()) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https")
            || url
                .host_str()
                .is_some_and(|host| host.contains(self_domain))
        {
            continue;
        }
        let raw_title = title_sel
            .map(|sel| item.select_single(sel).text())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| anchor.text());
        let title = clean_text(&raw_title);
        if title.is_empty() {
            continue;
        }
        let snippet = clean_text(&item.select_single(snippet_sel).text());
        hits.push(SearchHit {
            title,
            url: url.into(),
            snippet,
        });
    }
    hits
}
fn decode_link(raw: &str) -> String {
    let href = raw.replace("&amp;", "&");
    if let Some((_, query)) = href.split_once("uddg=") {
        let encoded = query.split('&').next().unwrap_or(query);
        let decoded: String = url::form_urlencoded::parse(format!("u={encoded}").as_bytes())
            .map(|(_, value)| value.into_owned())
            .collect();
        if decoded.starts_with("http://") || decoded.starts_with("https://") {
            return decoded;
        }
    }
    href.strip_prefix("//")
        .map(|rest| format!("https://{rest}"))
        .unwrap_or(href)
}

pub(crate) fn clean_text(raw: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    let text = decode_entities(&text);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find('&') {
        let (Some(head), Some(after)) = (rest.get(..position), rest.get(position..)) else {
            break;
        };
        output.push_str(head);
        let entity_end = after
            .get(1..)
            .and_then(|tail| tail.find(';'))
            .filter(|offset| *offset <= 32)
            .map(|offset| offset + 1);
        let Some(end) = entity_end else {
            output.push('&');
            let Some(next) = after.get(1..) else { break };
            rest = next;
            continue;
        };
        if let Some(ch) = after.get(1..end).and_then(decode_entity) {
            output.push(ch);
            let Some(next) = after.get(end + 1..) else {
                break;
            };
            rest = next;
        } else {
            output.push('&');
            let Some(next) = after.get(1..) else { break };
            rest = next;
        }
    }
    output.push_str(rest);
    output
}

fn decode_entity(entity: &str) -> Option<char> {
    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        return u32::from_str_radix(hex, 16)
            .ok()
            .and_then(char::from_u32)
            .filter(|ch| !ch.is_control());
    }
    if let Some(decimal) = entity.strip_prefix('#') {
        return decimal
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .filter(|ch| !ch.is_control());
    }
    match entity.to_ascii_lowercase().as_str() {
        "amp" => Some('&'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "nbsp" | "ensp" | "emsp" | "thinsp" => Some(' '),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        "hellip" => Some('…'),
        "lsquo" => Some('‘'),
        "rsquo" => Some('’'),
        "ldquo" => Some('“'),
        "rdquo" => Some('”'),
        "laquo" => Some('«'),
        "raquo" => Some('»'),
        "copy" => Some('©'),
        "reg" => Some('®'),
        "trade" => Some('™'),
        "times" => Some('×'),
        "divide" => Some('÷'),
        "euro" => Some('€'),
        "pound" => Some('£'),
        "yen" => Some('¥'),
        "cent" => Some('¢'),
        "deg" => Some('°'),
        "middot" => Some('·'),
        "bull" => Some('•'),
        "dagger" => Some('†'),
        "prime" => Some('′'),
        "plusmn" => Some('±'),
        "micro" => Some('µ'),
        "para" => Some('¶'),
        "sect" => Some('§'),
        "frac12" => Some('½'),
        "frac14" => Some('¼'),
        "frac34" => Some('¾'),
        "sup2" => Some('²'),
        "sup3" => Some('³'),
        "szlig" => Some('ß'),
        "shy" => Some('\u{00AD}'),
        "zwnj" => Some('\u{200C}'),
        "zwj" => Some('\u{200D}'),
        "lrm" => Some('\u{200E}'),
        "rlm" => Some('\u{200F}'),
        _ => None,
    }
}
