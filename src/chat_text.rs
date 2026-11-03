//! Text primitives and reply shaping shared by reply styling: fence-aware
//! chunking to Telegram's limit, bounded truncation, scheme/control-character
//! hygiene, prose/code flattening, and HTML code-block rendering.

use crate::config::config;

/// Plain-text chunks within Telegram's message limit, split on line
/// boundaries where possible. When a fenced code block spans a chunk boundary,
/// the first chunk is closed with a matching fence and the next chunk reopens
/// the fence header so every chunk renders balanced `<pre><code>` tags.
pub(crate) fn split_message(text: &str) -> Vec<String> {
    let max_chars = config().chat.reply_max_chars;
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut in_code = false;
    let mut fence_ticks = 0;
    let mut fence_header = String::new();
    let flush =
        |chunks: &mut Vec<String>, cur: &mut String, in_code: bool, ticks: usize, hdr: &str| {
            if !cur.trim().is_empty() {
                let mut chunk = std::mem::take(cur);
                if in_code {
                    chunk.push('\n');
                    chunk.extend(std::iter::repeat_n('`', ticks));
                    cur.push_str(hdr);
                }
                chunks.push(chunk);
            }
        };
    for line in text.lines() {
        let reserve = if in_code { fence_ticks + 1 } else { 0 };
        let budget = max_chars.saturating_sub(reserve).max(1);
        if line.chars().count() > budget {
            flush(
                &mut chunks,
                &mut current,
                in_code,
                fence_ticks,
                &fence_header,
            );
            let mut rest = line;
            while rest.chars().count() > budget {
                let prefix = if current.is_empty() {
                    0
                } else {
                    current.chars().count() + 1
                };
                let step = budget.saturating_sub(prefix).max(1);
                let split = rest.char_indices().nth(step).map_or(rest.len(), |(i, _)| i);
                let Some((head, tail)) = rest.split_at_checked(split) else {
                    break;
                };
                push_line(&mut current, head);
                flush(
                    &mut chunks,
                    &mut current,
                    in_code,
                    fence_ticks,
                    &fence_header,
                );
                rest = tail;
            }
            push_line(&mut current, rest);
            continue;
        }
        if current.chars().count() + usize::from(!current.is_empty()) + line.chars().count()
            > budget
        {
            flush(
                &mut chunks,
                &mut current,
                in_code,
                fence_ticks,
                &fence_header,
            );
        }
        push_line(&mut current, line);
        if in_code {
            if is_fence_close(line, fence_ticks) {
                in_code = false;
                fence_ticks = 0;
                fence_header.clear();
            }
        } else if let Some(ticks) = fence_open_ticks(line) {
            in_code = true;
            fence_ticks = ticks;
            let lang = fence_language(line, ticks);
            fence_header = format!("{}{lang}", "`".repeat(ticks));
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Normalizes reply prose and code fences: preserves intentional line breaks
/// (collapsing runs of blank lines to a single blank line), keeps fenced code
/// blocks verbatim, replaces em/en dashes in prose, and expands bare repository
/// file paths into full GitHub blob URLs.
pub(crate) fn flatten_reply_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    let mut fence_ticks = 0;
    let mut pending_blank = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if in_code {
            push_line(&mut out, line.trim_end());
            in_code = !is_fence_close(line, fence_ticks);
        } else if let Some(ticks) = fence_open_ticks(line) {
            if pending_blank && !out.is_empty() {
                out.push('\n');
                pending_blank = false;
            }
            push_line(&mut out, line.trim_end());
            in_code = true;
            fence_ticks = ticks;
        } else {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                if !out.is_empty() {
                    pending_blank = true;
                }
            } else {
                if pending_blank && !out.is_empty() {
                    out.push('\n');
                    pending_blank = false;
                }
                let prose = if trimmed.contains(['—', '–']) {
                    std::borrow::Cow::Owned(trimmed.replace(['—', '–'], "-"))
                } else {
                    std::borrow::Cow::Borrowed(trimmed)
                };
                push_line(&mut out, &expand_repo_blob_links(&prose));
            }
        }
    }
    out
}

/// Renders a styled plain-text chunk into Telegram HTML (`ParseMode::Html`)
/// when it contains a fenced code block, inline backtick span, or markdown
/// link `[label](url)`, or `None` when plain-text delivery suffices.
pub(crate) fn render_telegram_html(text: &str) -> Option<String> {
    if !text.contains('`') && !text.contains("](") {
        return None;
    }
    let mut out = String::with_capacity(text.len() + 32);
    let mut code = String::new();
    let mut lang = String::new();
    let mut in_code = false;
    let mut fence_ticks = 0;
    let mut formatted = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if in_code {
            if is_fence_close(line, fence_ticks) {
                push_html_pre(&mut out, &lang, &code);
                code.clear();
                lang.clear();
                in_code = false;
                formatted = true;
            } else {
                if !code.is_empty() {
                    code.push('\n');
                }
                code.push_str(&escape_html(line));
            }
        } else if let Some(ticks) = fence_open_ticks(line) {
            in_code = true;
            fence_ticks = ticks;
            lang = fence_language(line, ticks);
        } else {
            let (rendered, line_has_code) = render_inline_code_html(line);
            formatted |= line_has_code;
            push_line(&mut out, &rendered);
        }
    }
    if in_code {
        push_html_pre(&mut out, &lang, &code);
        formatted = true;
    }
    formatted.then_some(out)
}

fn fence_language(line: &str, ticks: usize) -> String {
    let trimmed = line.trim_start();
    let rest = trimmed.get(ticks..).unwrap_or("").trim();
    let token = rest.split_whitespace().next().unwrap_or("");
    token
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+'))
        .take(32)
        .collect()
}

fn push_html_pre(out: &mut String, lang: &str, escaped_code: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    if lang.is_empty() {
        out.push_str("<pre><code>");
        out.push_str(escaped_code);
        out.push_str("</code></pre>");
    } else {
        out.push_str("<pre><code class=\"language-");
        out.push_str(lang);
        out.push_str("\">");
        out.push_str(escaped_code);
        out.push_str("</code></pre>");
    }
}

fn render_inline_code_html(line: &str) -> (String, bool) {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut formatted = false;
    while let Some(open) = rest.find('`') {
        formatted |= push_prose_with_links_html(&mut out, rest.get(..open).unwrap_or(""));
        let after_prefix = rest.get(open..).unwrap_or("");
        let ticks = after_prefix.len() - after_prefix.trim_start_matches('`').len();
        let after_open = after_prefix.get(ticks..).unwrap_or("");
        if let Some(close) = find_closing_ticks(after_open, ticks) {
            let span = after_open.get(..close).unwrap_or("");
            let inner =
                if ticks > 1 && span.starts_with(' ') && span.ends_with(' ') && span.len() > 2 {
                    span.get(1..span.len() - 1).unwrap_or(span)
                } else {
                    span
                };
            if !inner.trim().is_empty() {
                out.push_str("<code>");
                push_escaped_html(&mut out, inner);
                out.push_str("</code>");
                formatted = true;
            } else {
                out.extend(std::iter::repeat_n('`', ticks * 2));
            }
            rest = after_open.get(close + ticks..).unwrap_or("");
        } else {
            out.extend(std::iter::repeat_n('`', ticks));
            rest = after_open;
        }
    }
    formatted |= push_prose_with_links_html(&mut out, rest);
    (out, formatted)
}

fn push_prose_with_links_html(out: &mut String, text: &str) -> bool {
    let mut rest = text;
    let mut formatted = false;
    while let Some(open) = rest.find('[') {
        push_escaped_html(out, rest.get(..open).unwrap_or(""));
        let after_open = rest.get(open + 1..).unwrap_or("");
        let Some(mid) = after_open.find("](") else {
            out.push('[');
            push_escaped_html(out, after_open);
            return formatted;
        };
        let label = after_open.get(..mid).unwrap_or("").trim();
        let after_mid = after_open.get(mid + 2..).unwrap_or("");
        if let Some(close) = after_mid.find(')') {
            let raw_url = after_mid.get(..close).unwrap_or("").trim();
            let resolved = match crate::chat_urls::resolve_repo_target(raw_url) {
                Some(rel) => std::borrow::Cow::Owned(format!(
                    "{}/{rel}",
                    crate::code_inspect::GITHUB_BLOB_BASE
                )),
                None => std::borrow::Cow::Borrowed(raw_url),
            };
            if !label.is_empty()
                && !label.contains('\n')
                && (resolved.starts_with("https://") || resolved.starts_with("http://"))
                && !resolved.chars().any(char::is_whitespace)
            {
                out.push_str("<a href=\"");
                push_escaped_html_attr(out, &resolved);
                out.push_str("\">");
                push_escaped_html(out, label);
                out.push_str("</a>");
                formatted = true;
                rest = after_mid.get(close + 1..).unwrap_or("");
                continue;
            }
        }
        out.push('[');
        rest = after_open;
    }
    push_escaped_html(out, rest);
    formatted
}

fn push_escaped_html_attr(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

pub(crate) fn find_closing_ticks(text: &str, ticks: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes.get(i) == Some(&b'`') {
            let mut run = 1;
            while bytes.get(i + run) == Some(&b'`') {
                run += 1;
            }
            if run == ticks {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    push_escaped_html(&mut out, text);
    out
}

fn push_escaped_html(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

/// `true` when `text` ends inside an unclosed fenced code block, so trailing
/// prose punctuation must not be stripped from code.
pub(crate) fn ends_inside_fence(text: &str) -> bool {
    let mut in_code = false;
    let mut fence_ticks = 0;
    for line in text.lines() {
        if in_code {
            in_code = !is_fence_close(line, fence_ticks);
        } else if let Some(ticks) = fence_open_ticks(line) {
            in_code = true;
            fence_ticks = ticks;
        }
    }
    in_code
}

/// Backtick count of an opening fence line (at least three), or `None` for
/// prose. Four-backtick fences wrap examples that contain three-backtick ones,
/// so the closing line must match this length.
pub(crate) fn fence_open_ticks(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let ticks = trimmed.len() - trimmed.trim_start_matches('`').len();
    (3..=8).contains(&ticks).then_some(ticks)
}

/// `true` for a line that closes a fence opened with `open_ticks` backticks:
/// a run of at least that many backticks and nothing else.
pub(crate) fn is_fence_close(line: &str, open_ticks: usize) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= open_ticks && trimmed.bytes().all(|byte| byte == b'`')
}

fn push_line(out: &mut String, line: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}

pub(crate) use crate::chat_urls::expand_repo_blob_links;
/// Cuts at the last whitespace before `max_chars` so a shortened reply never
/// ends mid-word.
pub(crate) fn shorten(text: &str, max_chars: usize) -> &str {
    let Some((limit, _)) = text.char_indices().nth(max_chars) else {
        return text;
    };
    let Some(head) = text.get(..limit) else {
        return text;
    };
    head.rfind(char::is_whitespace)
        .and_then(|index| head.get(..index))
        .map(str::trim_end)
        .unwrap_or(head)
}

pub(crate) fn truncate_text(text: &str, max_chars: usize) -> String {
    let Some((limit, _)) = text.char_indices().nth(max_chars) else {
        return text.to_owned();
    };
    let Some(head) = text.get(..limit) else {
        return text.to_owned();
    };
    let cut = head.rfind(char::is_whitespace).unwrap_or(head.len());
    let head = if cut == 0 {
        head
    } else {
        head.get(..cut).unwrap_or(head)
    };
    head.trim_end().to_owned()
}

pub(crate) fn is_bidi_or_control(ch: char) -> bool {
    ch.is_control()
        || matches!(ch as u32,
            0x00AD
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2066..=0x2069
            | 0xFEFF)
}

pub(crate) fn strip_url_schemes(text: &str) -> String {
    const SCHEMES: [&str; 7] = [
        "https://",
        "http://",
        "tg://",
        "javascript:",
        "data:",
        "file:",
        "ftp://",
    ];
    let lower = text.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while let Some(rest) = text.get(index..) {
        let boundary = text
            .get(..index)
            .and_then(|head| head.chars().next_back())
            .is_none_or(|ch| !ch.is_alphanumeric());
        let matched = if boundary {
            SCHEMES.into_iter().find(|scheme| {
                lower
                    .get(index..)
                    .is_some_and(|tail| tail.starts_with(*scheme))
            })
        } else {
            None
        };
        if let Some(scheme) = matched {
            index += scheme.len();
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}

pub(crate) fn format_moscow_time(unix_secs: i64) -> String {
    let msk = unix_secs.saturating_add(3 * 3600).max(0);
    let days = msk.div_euclid(86_400);
    let rem = msk.rem_euclid(86_400);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    let weekday = match (days + 4).rem_euclid(7) {
        0 => "вс",
        1 => "пн",
        2 => "вт",
        3 => "ср",
        4 => "чт",
        5 => "пт",
        _ => "сб",
    };
    format!("{year:04}-{month:02}-{day:02} ({weekday}) {hour:02}:{minute:02} МСК (UTC+3)")
}
