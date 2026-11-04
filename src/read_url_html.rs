use crate::prelude::*;
use crate::read_url::{truncate_chars, validate_url, ReadUrlError};
use dom_smoothie::{Config, Readability, TextMode};

pub(crate) fn extract_html_with_links(
    html: &str,
    url: &Url,
    links: &mut Vec<(String, Url)>,
) -> Result<(Option<String>, String), ReadUrlError> {
    if html_is_too_complex(html) {
        tracing::debug!(method = "reject", reason = "too_complex", "html extraction");
        return Err(ReadUrlError::TooComplex);
    }
    // Readability picks the main article and strips navigation/chrome. Parse
    // synchronously: its tendril-backed Article is not Send.
    let document = dom_query::Document::from(html);
    collect_document_links(&document, url, links);
    let document = strip_chrome(document);
    let title = document.select_single("title").text().to_string();
    let title = title.trim().to_owned();
    // Readability's scoring can fall back to the body on short pages and keep
    // site chrome. Remove elements whose content is non-prose by definition
    // first; keep header/footer because they often carry article metadata.
    let cleaned = document.html().to_string();
    let config = Config {
        text_mode: TextMode::Markdown,
        char_threshold: 0,
        max_elements_to_parse: config().read_url.max_html_elements,
        ..Config::default()
    };
    if let Ok(mut readability) = Readability::new(cleaned, Some(url.as_str()), Some(config)) {
        if let Ok(article) = readability.parse() {
            let text = article.text_content.to_string();
            if !text.trim().is_empty() {
                let article_title = article.title.trim();
                let title = if article_title.is_empty() {
                    title
                } else {
                    article_title.to_owned()
                };
                let r_chars = text.chars().count();
                let chosen = if r_chars < 600 {
                    let fallback = fallback_text(&document);
                    let f_chars = fallback.chars().count();
                    if f_chars >= 600 && f_chars > r_chars.saturating_mul(2) {
                        format!("{text}\n\n{fallback}")
                    } else {
                        text
                    }
                } else {
                    text
                };
                tracing::debug!(
                    method = "readability",
                    chars = chosen.chars().count(),
                    links = links.len(),
                    "html extraction"
                );
                return Ok(((!title.is_empty()).then_some(title), chosen));
            }
        }
    }
    // A readability failure must not turn a simple page into an error. Fall
    // back to the cleaned body text; links and headings are lost but content is not.
    let fallback = fallback_text(&document);
    tracing::debug!(
        method = "fallback",
        chars = fallback.chars().count(),
        links = links.len(),
        "html extraction"
    );
    Ok(((!title.is_empty()).then_some(title), fallback))
}

pub(crate) fn strip_chrome(document: dom_query::Document) -> dom_query::Document {
    document
        .select(
            "script, style, nav, aside, form, noscript, svg, iframe, template, select, dialog, canvas",
        )
        .remove();
    document
}

pub(crate) fn fallback_text(document: &dom_query::Document) -> String {
    document.select_single("body").formatted_text().to_string()
}

fn collect_document_links(
    document: &dom_query::Document,
    base: &Url,
    links: &mut Vec<(String, Url)>,
) {
    for node in document
        .select("a[href], link[rel='alternate'][href]")
        .iter()
    {
        if links.len() >= config().read_url.max_discovered_links {
            break;
        }
        let Some(href) = node.attr("href") else {
            continue;
        };
        let Ok(mut resolved) = base.join(href.trim()) else {
            continue;
        };
        resolved.set_fragment(None);
        if resolved.as_str().len() <= config().read_url.max_link_chars
            && matches!(resolved.scheme(), "http" | "https")
            && validate_url(&resolved).is_ok()
            && !links.iter().any(|(_, existing)| existing == &resolved)
        {
            let raw_label: String = node
                .text()
                .chars()
                .filter(|ch| !ch.is_control())
                .take(1_000)
                .collect();
            let label = truncate_chars(
                &raw_label.split_whitespace().collect::<Vec<_>>().join(" "),
                80,
            )
            .0;
            let label = if label.is_empty() {
                node.attr("title")
                    .map(|title| sanitize_title(&title))
                    .unwrap_or_default()
            } else {
                label
            };
            links.push((label, resolved));
        }
    }
}

pub(crate) fn sanitize_title(raw: &str) -> String {
    let cleaned: String = raw.chars().filter(|ch| !ch.is_control()).collect();
    truncate_chars(
        &cleaned.split_whitespace().collect::<Vec<_>>().join(" "),
        config().read_url.title_chars,
    )
    .0
}

pub(crate) fn html_is_too_complex(html: &str) -> bool {
    let bytes = html.as_bytes();
    let mut i = 0usize;
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    let mut elements = 0usize;
    while let Some(&byte) = bytes.get(i) {
        if byte != b'<' {
            i += 1;
            continue;
        }
        let rest = html.get(i..).unwrap_or_default();
        if rest.starts_with("<!--") {
            i = rest
                .get(4..)
                .and_then(|tail| tail.find("-->"))
                .map_or(bytes.len(), |end| i + 4 + end + 3);
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = rest
                .get(9..)
                .and_then(|tail| tail.find("]]>"))
                .map_or(bytes.len(), |end| i + 9 + end + 3);
            continue;
        }
        if matches!(bytes.get(i + 1), Some(b'!') | Some(b'?')) {
            i = rest
                .get(2..)
                .and_then(|tail| tail.find('>'))
                .map_or(bytes.len(), |end| i + 2 + end + 1);
            continue;
        }
        i += 1;
        let closing = bytes.get(i) == Some(&b'/');
        if closing {
            i += 1;
        }
        let name_start = i;
        while let Some(&byte) = bytes.get(i) {
            if !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':' | b'_')) {
                break;
            }
            i += 1;
        }
        if name_start == i {
            continue;
        }
        let name = html
            .get(name_start..i)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut quote = None;
        let mut self_closing = false;
        while let Some(&byte) = bytes.get(i) {
            if let Some(active) = quote {
                if byte == active {
                    quote = None;
                }
            } else if matches!(byte, b'"' | b'\'') {
                quote = Some(byte);
            } else if byte == b'>' {
                let mut previous = i;
                while previous > 0 && bytes.get(previous - 1).is_some_and(u8::is_ascii_whitespace) {
                    previous -= 1;
                }
                self_closing = previous > 0 && bytes.get(previous - 1) == Some(&b'/');
                break;
            }
            i += 1;
        }
        i = i.saturating_add(1);
        if closing {
            depth = depth.saturating_sub(1);
            continue;
        }
        elements = elements.saturating_add(1);
        if elements > config().read_url.max_html_elements {
            return true;
        }
        if !self_closing && !is_void_element(&name) {
            depth += 1;
            max_depth = max_depth.max(depth);
            if max_depth > config().read_url.max_html_depth {
                return true;
            }
        }
    }
    false
}

fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}
