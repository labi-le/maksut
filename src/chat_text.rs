//! Text primitives shared by reply styling: chunking to Telegram's limit,
//! bounded truncation, and scheme/control-character hygiene.

pub(crate) const TELEGRAM_MESSAGE_CHARS: usize = 3900;

/// Plain-text chunks within Telegram's message limit, split on line
/// boundaries where possible.
pub(crate) fn split_message(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.chars().count() > TELEGRAM_MESSAGE_CHARS {
            if !current.trim().is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut rest = line;
            while rest.chars().count() > TELEGRAM_MESSAGE_CHARS {
                let split = rest
                    .char_indices()
                    .nth(TELEGRAM_MESSAGE_CHARS)
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
        if projected > TELEGRAM_MESSAGE_CHARS {
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

/// Lowercases the whole reply, except for URLs whose path may be case-sensitive.
pub(crate) fn lowercase_reply(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|token| {
            let trimmed = token.trim_start_matches(|ch: char| !ch.is_alphanumeric());
            if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                token.to_owned()
            } else {
                token.to_lowercase()
            }
        })
        .collect()
}

/// Cuts at the last whitespace before `max_chars`: a shortened reply never
/// loses a whole sentence and never ends mid-word. Splitting on sentence ends
/// deleted the final clause of measured replies, which read as a broken
/// thought rather than a short one.
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
