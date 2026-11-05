use crate::prelude::*;
use crate::read_url::{html_is_too_complex, prompt_urls, truncate_chars, validate_url, ReadPage};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UrlSelector {
    None,
    Raw,
    Range {
        start: usize,
        end: Option<usize>,
        raw: bool,
    },
    Tail {
        count: usize,
        raw: bool,
    },
}

pub(crate) fn split_url_selector(raw: &str) -> (&str, UrlSelector) {
    let trimmed = raw.trim();
    if let Some(base) = trimmed.strip_suffix(":raw") {
        if let Some((inner, sel)) = parse_line_suffix(base, true) {
            return (inner, sel);
        }
        if base.contains("://") && !base.ends_with("://") {
            return (base, UrlSelector::Raw);
        }
    }
    if let Some((base, sel)) = parse_line_suffix(trimmed, false) {
        return (base, sel);
    }
    (trimmed, UrlSelector::None)
}

fn parse_line_suffix(raw: &str, mut is_raw: bool) -> Option<(&str, UrlSelector)> {
    let mut base = raw;
    if let Some(stripped) = base.strip_suffix(":raw") {
        is_raw = true;
        base = stripped;
    }
    let (candidate_base, suffix) = base.rsplit_once(':')?;
    if !candidate_base.contains("://") || candidate_base.ends_with("://") {
        return None;
    }
    let after_scheme = candidate_base
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or("");
    // Do not mistake `http://example.com:80` for a line selector unless there is a path `/`.
    if !after_scheme.contains('/') && suffix.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let suffix = suffix.strip_prefix('L').unwrap_or(suffix);
    if let Some(tail_str) = suffix.strip_prefix('-') {
        let count = tail_str.parse::<usize>().ok().filter(|&n| n >= 1)?;
        return Some((candidate_base, UrlSelector::Tail { count, raw: is_raw }));
    }
    if let Some((start_s, count_s)) = suffix.split_once('+') {
        let start = start_s.parse::<usize>().ok().filter(|&n| n >= 1)?;
        let count = count_s.parse::<usize>().ok().filter(|&n| n >= 1)?;
        let end = Some(start.saturating_add(count).saturating_sub(1));
        return Some((
            candidate_base,
            UrlSelector::Range {
                start,
                end,
                raw: is_raw,
            },
        ));
    }
    if let Some((start_s, end_s)) = suffix.split_once("..").or_else(|| suffix.split_once('-')) {
        let start = start_s.parse::<usize>().ok().filter(|&n| n >= 1)?;
        let end = (!end_s.is_empty())
            .then(|| end_s.parse::<usize>().ok().filter(|&e| e >= start))
            .flatten();
        if !end_s.is_empty() && end.is_none() {
            return None;
        }
        return Some((
            candidate_base,
            UrlSelector::Range {
                start,
                end,
                raw: is_raw,
            },
        ));
    }
    let start = suffix.parse::<usize>().ok().filter(|&n| n >= 1)?;
    Some((
        candidate_base,
        UrlSelector::Range {
            start,
            end: is_raw.then_some(start),
            raw: is_raw,
        },
    ))
}

pub(crate) fn apply_url_selector(page: ReadPage, selector: &UrlSelector) -> ReadPage {
    if matches!(selector, UrlSelector::None) {
        return page;
    }
    let (header, body) = page
        .text
        .split_once("\n\n---\n\n")
        .map(|(h, b)| (Some(h), b))
        .unwrap_or((None, page.text.as_str()));
    let lines: Vec<&str> = body.lines().collect();
    let total = lines.len().max(1);
    let (start_idx, end_idx, raw) = match *selector {
        UrlSelector::None => return page,
        UrlSelector::Raw => {
            return ReadPage {
                text: body.to_owned(),
                links: page.links,
            }
        }
        UrlSelector::Tail { count, raw } => {
            let start = total.saturating_sub(count);
            (start, total, raw)
        }
        UrlSelector::Range { start, end, raw } => {
            let s = start.saturating_sub(1).min(total);
            let e = end.unwrap_or(total).min(total).max(s);
            (s, e, raw)
        }
    };
    let slice = lines.get(start_idx..end_idx).unwrap_or(&[]);
    if raw {
        return ReadPage {
            text: slice.join("\n"),
            links: page.links,
        };
    }
    let mut numbered = String::new();
    if let Some(header) = header {
        numbered.push_str(header);
        numbered.push_str(&format!(
            "\nShowing lines {}-{} of {total}\n\n---\n\n",
            start_idx + 1,
            end_idx
        ));
    }
    for (offset, line) in slice.iter().enumerate() {
        numbered.push_str(&format!("{}: {}\n", start_idx + offset + 1, line));
    }
    ReadPage {
        text: numbered.trim_end().to_owned(),
        links: page.links,
    }
}

/// Rewrites recognized site URLs (GitHub blob/issue, Reddit post/subreddit,
/// Wikipedia article, crates.io crate, PyPI package, Hacker News item, public
/// Telegram channel/post) to their public raw/JSON/web preview endpoints.
pub(crate) fn rewrite_special_target(url: &Url) -> Option<(Url, &'static str)> {
    let host = url.host_str()?.to_ascii_lowercase();
    let path = url.path();
    if host == "github.com" || host == "www.github.com" {
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if let [owner, repo, "blob", branch, rest @ ..] = segments.as_slice() {
            if !rest.is_empty() {
                let raw = format!(
                    "https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{}",
                    rest.join("/")
                );
                if let Ok(parsed) = Url::parse(&raw) {
                    return Some((parsed, "github-raw"));
                }
            }
        }
        if let [owner, repo, "issues" | "pull", number] = segments.as_slice() {
            if number.chars().all(|c| c.is_ascii_digit()) {
                let api = format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}");
                if let Ok(parsed) = Url::parse(&api) {
                    return Some((parsed, "github-issue"));
                }
            }
        }
    }
    if matches!(
        host.as_str(),
        "reddit.com" | "www.reddit.com" | "old.reddit.com"
    ) && !path.ends_with(".json")
        && path.len() > 1
    {
        let mut json_url = url.clone();
        json_url.set_path(&format!("{}.json", path.trim_end_matches('/')));
        return Some((json_url, "reddit"));
    }
    if let Some(lang) = host.strip_suffix(".wikipedia.org") {
        let lang = lang.strip_suffix(".m").unwrap_or(lang);
        if !lang.is_empty() && lang.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
            if let Some(title) = path.strip_prefix("/wiki/") {
                if !title.is_empty() && !title.contains(':') {
                    let api =
                        format!("https://{lang}.wikipedia.org/api/rest_v1/page/summary/{title}");
                    if let Ok(parsed) = Url::parse(&api) {
                        return Some((parsed, "wikipedia"));
                    }
                }
            }
        }
    }
    if host == "crates.io" || host == "www.crates.io" {
        if let Some(rest) = path.strip_prefix("/crates/") {
            let name = rest.split('/').next().unwrap_or("").trim();
            if !name.is_empty() {
                let api = format!("https://crates.io/api/v1/crates/{name}");
                if let Ok(parsed) = Url::parse(&api) {
                    return Some((parsed, "crates.io"));
                }
            }
        }
    }
    if host == "pypi.org" || host == "www.pypi.org" {
        if let Some(rest) = path.strip_prefix("/project/") {
            let name = rest.split('/').next().unwrap_or("").trim();
            if !name.is_empty() {
                let api = format!("https://pypi.org/pypi/{name}/json");
                if let Ok(parsed) = Url::parse(&api) {
                    return Some((parsed, "pypi"));
                }
            }
        }
    }
    if host == "news.ycombinator.com" && path == "/item" {
        if let Some(id) = url
            .query_pairs()
            .find(|(k, _)| k == "id")
            .map(|(_, v)| v.into_owned())
            .filter(|v| v.chars().all(|c| c.is_ascii_digit()))
        {
            let api = format!("https://hacker-news.firebaseio.com/v0/item/{id}.json");
            if let Ok(parsed) = Url::parse(&api) {
                return Some((parsed, "hackernews"));
            }
        }
    }
    if matches!(
        host.as_str(),
        "t.me" | "www.t.me" | "telegram.me" | "www.telegram.me"
    ) {
        if let Some(rewritten) = crate::read_url_telegram::rewrite_telegram_url(url, path) {
            return Some((rewritten, "telegram"));
        }
    }
    None
}

fn str_field<'a>(value: &'a Value, key: &str, default: &'a str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or(default)
}

pub(crate) fn format_special_json(method: &str, value: &Value) -> Option<(Option<String>, String)> {
    tracing::debug!(method, "special json extraction");
    match method {
        "github-issue" => {
            let title = value.get("title")?.as_str()?.to_owned();
            let state = str_field(value, "state", "open");
            let user = value
                .pointer("/user/login")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let body = str_field(value, "body", "");
            Some((
                Some(title.clone()),
                format!("# {title}\nState: {state} · Author: {user}\n\n{body}"),
            ))
        }
        "wikipedia" => {
            let title = value.get("title")?.as_str()?.to_owned();
            let desc = str_field(value, "description", "");
            let extract = value.get("extract")?.as_str()?;
            let body = if desc.is_empty() {
                format!("# {title}\n\n{extract}")
            } else {
                format!("# {title}\n*{desc}*\n\n{extract}")
            };
            Some((Some(title), body))
        }
        "reddit" => format_reddit_json(value),
        "crates.io" => {
            let krate = value.get("crate")?;
            let name = krate.get("name")?.as_str()?;
            let version = krate
                .get("max_stable_version")
                .or_else(|| krate.get("newest_version"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let desc = str_field(krate, "description", "");
            let repo = str_field(krate, "repository", "");
            let docs = str_field(krate, "documentation", "");
            Some((
                Some(format!("{name} {version}")),
                format!("# {name} ({version})\n\n{desc}\n\nRepository: {repo}\nDocs: {docs}"),
            ))
        }
        "pypi" => {
            let info = value.get("info")?;
            let name = info.get("name")?.as_str()?;
            let version = str_field(info, "version", "");
            let summary = str_field(info, "summary", "");
            let desc = truncate_chars(str_field(info, "description", ""), 6000).0;
            Some((
                Some(format!("{name} {version}")),
                format!("# {name} ({version})\n{summary}\n\n{desc}"),
            ))
        }
        "hackernews" => {
            let title = str_field(value, "title", "HN Item").to_owned();
            let by = str_field(value, "by", "");
            let score = value.get("score").and_then(Value::as_i64).unwrap_or(0);
            let target = str_field(value, "url", "");
            let text = str_field(value, "text", "");
            Some((
                Some(title.clone()),
                format!("# {title}\nBy {by} · {score} points\nURL: {target}\n\n{text}"),
            ))
        }
        _ => None,
    }
}

fn format_reddit_json(value: &Value) -> Option<(Option<String>, String)> {
    if let Some(arr) = value.as_array() {
        let post = arr.first()?.pointer("/data/children/0/data")?;
        let title = post.get("title")?.as_str()?.to_owned();
        let sub = post.get("subreddit").and_then(Value::as_str).unwrap_or("");
        let author = post.get("author").and_then(Value::as_str).unwrap_or("");
        let score = post.get("score").and_then(Value::as_i64).unwrap_or(0);
        let selftext = post.get("selftext").and_then(Value::as_str).unwrap_or("");
        let link_url = post.get("url").and_then(Value::as_str).unwrap_or("");
        let mut out = format!("# {title}\nr/{sub} · u/{author} · {score} points\n");
        if !selftext.is_empty() {
            out.push_str(&format!("\n{selftext}\n"));
        } else if !link_url.is_empty() {
            out.push_str(&format!("\nLink: {link_url}\n"));
        }
        if let Some(comments) = arr
            .get(1)
            .and_then(|c| c.pointer("/data/children"))
            .and_then(Value::as_array)
        {
            out.push_str("\n## Top Comments\n");
            for item in comments.iter().take(8) {
                let data = item.get("data")?;
                let Some(body) = data.get("body").and_then(Value::as_str) else {
                    continue;
                };
                let c_author = data.get("author").and_then(Value::as_str).unwrap_or("");
                out.push_str(&format!(
                    "\n- u/{c_author}: {}\n",
                    truncate_chars(body, 400).0
                ));
            }
        }
        return Some((Some(title), out));
    }
    let children = value.pointer("/data/children")?.as_array()?;
    let mut out = String::from("# Reddit\n\n");
    for item in children.iter().take(15) {
        let Some(data) = item.get("data") else {
            continue;
        };
        let Some(title) = data.get("title").and_then(Value::as_str) else {
            continue;
        };
        let score = data.get("score").and_then(Value::as_i64).unwrap_or(0);
        let permalink = data.get("permalink").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!(
            "- {title} ({score} pts) https://www.reddit.com{permalink}\n"
        ));
    }
    Some((Some("Reddit".into()), out))
}

/// Parses RSS (`<channel><item>`) or Atom (`<feed><entry>`) XML into clean
/// markdown and extracts item links for follow-up `read_url` navigation.
pub(crate) fn render_feed_xml(
    xml: &str,
    base: &Url,
    labeled_links: &mut Vec<(String, Url)>,
) -> Option<(Option<String>, String)> {
    let trimmed = xml.trim_start();
    if (!trimmed.contains("<rss") && !trimmed.contains("<feed") && !trimmed.contains("<channel"))
        || html_is_too_complex(xml)
    {
        return None;
    }
    let doc = dom_query::Document::from(xml);
    let channel = doc.select_single("channel");
    if channel.exists() {
        let feed_title = clean_xml_field(&channel.select_single("title").text());
        let label = if feed_title.is_empty() {
            "RSS Feed"
        } else {
            &feed_title
        };
        let mut md = format!("# {label}\n\n");
        for item in doc.select("item").iter().take(15) {
            let item_title = clean_xml_field(&item.select_single("title").text());
            let pub_date = clean_xml_field(&item.select_single("pubDate, dc\\:date").text());
            let desc =
                clean_xml_field(&item.select_single("description, content\\:encoded").text());
            let item_html = item.html().to_string();
            let link = extract_xml_tag_text(&item_html, "link")
                .or_else(|| extract_xml_tag_text(&item_html, "guid"))
                .unwrap_or_default();
            append_feed_entry(
                &mut md,
                base,
                labeled_links,
                &item_title,
                &pub_date,
                &desc,
                &link,
            );
        }
        for url in prompt_urls(xml) {
            if validate_url(&url).is_ok()
                && !labeled_links.iter().any(|(_, u)| u == &url)
                && labeled_links.len() < 32
            {
                labeled_links.push((String::new(), url));
            }
        }
        return Some(((!feed_title.is_empty()).then_some(feed_title), md));
    }
    let feed = doc.select_single("feed");
    if feed.exists() {
        let feed_title = clean_xml_field(&feed.select_single("title").text());
        let label = if feed_title.is_empty() {
            "Atom Feed"
        } else {
            &feed_title
        };
        let mut md = format!("# {label}\n\n");
        for entry in doc.select("entry").iter().take(15) {
            let entry_title = clean_xml_field(&entry.select_single("title").text());
            let updated = clean_xml_field(&entry.select_single("updated, published").text());
            let summary = clean_xml_field(&entry.select_single("summary, content").text());
            let href = entry
                .select_single("link[href]")
                .attr("href")
                .map(|s| s.to_string())
                .unwrap_or_default();
            append_feed_entry(
                &mut md,
                base,
                labeled_links,
                &entry_title,
                &updated,
                &summary,
                &href,
            );
        }
        return Some(((!feed_title.is_empty()).then_some(feed_title), md));
    }
    None
}

fn append_feed_entry(
    md: &mut String,
    base: &Url,
    labeled_links: &mut Vec<(String, Url)>,
    title: &str,
    date: &str,
    summary: &str,
    href: &str,
) {
    let title_shown = if title.is_empty() { "Untitled" } else { title };
    md.push_str(&format!("## {title_shown}\n"));
    if !date.is_empty() {
        md.push_str(&format!("*{date}*\n"));
    }
    if !summary.is_empty() {
        md.push_str(&format!("{}\n", truncate_chars(summary, 500).0));
    }
    if let Ok(mut parsed) = base.join(href.trim()) {
        parsed.set_fragment(None);
        if validate_url(&parsed).is_ok() {
            md.push_str(&format!("Link: {parsed}\n"));
            if !labeled_links.iter().any(|(_, u)| u == &parsed) && labeled_links.len() < 32 {
                labeled_links.push((title_shown.to_owned(), parsed));
            }
        }
    }
    md.push('\n');
}

fn clean_xml_field(raw: &str) -> String {
    crate::search::clean_text(&raw.replace("<![CDATA[", "").replace("]]>", ""))
}

fn extract_xml_tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml.get(start..)?.find(&close)? + start;
    let content = xml
        .get(start..end)?
        .trim()
        .trim_start_matches("<![CDATA[")
        .trim_end_matches("]]>")
        .trim();
    (!content.is_empty()).then(|| content.to_owned())
}
