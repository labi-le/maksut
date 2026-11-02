//! Text primitives and reply shaping shared by reply styling: chunking to
//! Telegram's limit, bounded truncation, scheme/control-character hygiene, and
//! prose/code flattening for plain-text delivery.

use crate::config::config;

/// Plain-text chunks within Telegram's message limit, split on line
/// boundaries where possible.
pub(crate) fn split_message(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.chars().count() > config().chat.reply_max_chars {
            if !current.trim().is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut rest = line;
            while rest.chars().count() > config().chat.reply_max_chars {
                let split = rest
                    .char_indices()
                    .nth(config().chat.reply_max_chars)
                    .map(|(index, _)| index)
                    .unwrap_or(rest.len());
                let Some((head, tail)) = rest.split_at_checked(split) else {
                    break;
                };
                chunks.push(head.to_owned());
                rest = tail;
            }
            current.push_str(rest);
            continue;
        }
        let projected =
            current.chars().count() + usize::from(!current.is_empty()) + line.chars().count();
        if projected > config().chat.reply_max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Flattens prose runs to single lines while fenced code blocks keep their
/// line breaks, indentation and backticks: a code answer is unreadable once
/// every line is glued together. Prose loses em dashes and gains repository
/// blob links; code is left verbatim.
pub(crate) fn flatten_reply_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prose = String::new();
    let mut in_code = false;
    let mut fence_ticks = 0;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if in_code {
            push_line(&mut out, line.trim_end());
            in_code = !is_fence_close(line, fence_ticks);
        } else if let Some(ticks) = fence_open_ticks(line) {
            flush_prose(&mut out, &mut prose);
            push_line(&mut out, line.trim_end());
            in_code = true;
            fence_ticks = ticks;
        } else {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                if !prose.is_empty() {
                    prose.push(' ');
                }
                prose.push_str(trimmed);
            }
        }
    }
    flush_prose(&mut out, &mut prose);
    out
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
fn fence_open_ticks(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let ticks = trimmed.len() - trimmed.trim_start_matches('`').len();
    (ticks >= 3).then_some(ticks)
}

/// `true` for a line that closes a fence opened with `open_ticks` backticks:
/// a run of at least that many backticks and nothing else.
fn is_fence_close(line: &str, open_ticks: usize) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= open_ticks && trimmed.bytes().all(|byte| byte == b'`')
}

fn push_line(out: &mut String, line: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}

fn flush_prose(out: &mut String, prose: &mut String) {
    if prose.is_empty() {
        return;
    }
    if prose.contains(['—', '–']) {
        *prose = prose.replace(['—', '–'], "-");
    }
    let expanded = expand_repo_blob_links(prose);
    push_line(out, &expanded);
    prose.clear();
}

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

/// Expands bare repository paths (`src/foo.rs#L10`, `/docs/adr/012-free-chat.md:8`)
/// into full GitHub blob URLs while leaving existing `http(s)://` URLs untouched.
pub(crate) fn expand_repo_blob_links(text: &str) -> String {
    text.split_whitespace()
        .map(expand_repo_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn expand_repo_token(word: &str) -> String {
    if word.contains("://") {
        return word.to_owned();
    }
    let start = word
        .char_indices()
        .find(|(_, ch)| !matches!(ch, '(' | '[' | '<' | '"' | '\'' | '«'))
        .map_or(word.len(), |(idx, _)| idx);
    let tail = word.get(start..).unwrap_or("");
    let end = tail
        .char_indices()
        .rev()
        .find(|(_, ch)| {
            !matches!(
                ch,
                ')' | ']' | '>' | '"' | '\'' | '»' | ',' | '.' | ';' | ':' | '!' | '?'
            )
        })
        .map_or(start, |(idx, ch)| start + idx + ch.len_utf8());
    let core = word.get(start..end).unwrap_or("");
    let trimmed = core.strip_prefix('/').unwrap_or(core);
    let is_repo = trimmed.starts_with("src/")
        || trimmed.starts_with("docs/")
        || trimmed.starts_with("tools/")
        || trimmed == "Cargo.toml"
        || trimmed.starts_with("Cargo.toml#")
        || trimmed.starts_with("Cargo.toml:");
    if !is_repo {
        return word.to_owned();
    }
    let normalized = if !trimmed.contains('#') {
        if let Some((path, digits)) = trimmed.rsplit_once(':') {
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                format!("{path}#L{digits}")
            } else {
                trimmed.to_owned()
            }
        } else {
            trimmed.to_owned()
        }
    } else {
        trimmed.to_owned()
    };
    format!(
        "{}{}/{normalized}{}",
        word.get(..start).unwrap_or(""),
        crate::code_inspect::GITHUB_BLOB_BASE,
        word.get(end..).unwrap_or("")
    )
}
