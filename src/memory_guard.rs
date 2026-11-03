use crate::*;

const SECRET_MARKERS: &[&str] = &[
    "sk-",
    "ghp_",
    "xoxb",
    "akia",
    "-----begin",
    "password=",
    "password:",
    "token=",
    "token:",
    "cookie:",
    "api_key",
    "api key",
];

pub(crate) fn detect_flags(text: &str) -> i64 {
    let trimmed = text.trim();
    let mut flags = 0;
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.chars().count() < 4
        || (!trimmed.contains(char::is_whitespace)
            && (trimmed.starts_with("http://") || trimmed.starts_with("https://")))
        || !trimmed.chars().any(|ch| ch.is_alphanumeric())
    {
        flags |= FLAG_JUNK;
    }
    let lowered = trimmed.to_lowercase();
    const INJECTIONS: &[&str] = &[
        "ignore previous",
        "ignore all previous",
        "system prompt",
        "you are now",
        "act as",
        "developer mode",
        "jailbreak",
        "prompt injection",
        "ты теперь",
        "забудь",
        "новые инструкции",
        "выполни команду",
        "служебное сообщение",
        "ignore the above",
        "disregard previous",
        "repeat the following",
        "твоя новая роль",
    ];
    if INJECTIONS.iter().any(|marker| lowered.contains(marker)) {
        flags |= FLAG_INJECTION;
    }
    if SECRET_MARKERS.iter().any(|marker| lowered.contains(marker)) {
        flags |= FLAG_SECRET;
    }
    // Deliberately coarse: this counter keeps long technical text out of the
    // memory-hygiene paths (load_memory, compaction, reply context), not out of
    // the process. Text that leaves for `telegra.ph` is checked by the narrower
    // `credential_like` instead.
    let compact: String = trimmed
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if compact.len() >= 40 && compact.chars().any(|ch| ch.is_ascii_digit()) {
        flags |= FLAG_SECRET;
    }
    if injection_attempt(trimmed) {
        flags |= FLAG_INJECTION;
    }
    flags
}

/// Credential check for text leaving the process, used before publishing an
/// article. `detect_flags` sets `FLAG_SECRET` when the trimmed text holds 40+
/// ASCII alphanumerics in total (whitespace and punctuation discarded) and at
/// least one digit, which is why every technical answer trips it; that
/// whole-text rule still governs memory hygiene, while publication uses this
/// narrower predicate.
///
/// A key is caught by an assignment marker or by one contiguous run of 32+
/// `[A-Za-z0-9_-]` characters carrying a digit, so JWT segments, base64
/// padding, `id:secret` Bot API tokens, and quoted literals stay visible
/// without punctuation hiding them.
pub(crate) fn credential_like(text: &str) -> bool {
    let lowered = text.to_lowercase();
    let assigns_value = |name: &str| {
        lowered
            .split(name)
            .skip(1)
            .any(|rest| matches!(rest.trim_start().chars().next(), Some('=') | Some(':')))
    };
    let marked = SECRET_MARKERS
        .iter()
        .copied()
        .filter(|name| !matches!(*name, "api_key" | "api key"))
        .any(|name| lowered.contains(name));
    if marked || assigns_value("api_key") || assigns_value("api key") {
        return true;
    }
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
        .any(|run| run.chars().count() >= 32 && run.chars().any(|ch| ch.is_ascii_digit()))
}

/// High-confidence prompt-injection markers that justify a 24-hour ignore.
/// The broader `detect_flags` list is intentionally not reused here: weak
/// markers occur in ordinary technical conversation.
const INJECTION_MARKERS: &[&str] = &[
    "ignore previous",
    "ignore all previous",
    "ignore your previous",
    "ignore the above",
    "disregard previous",
    "disregard all previous",
    "forget previous",
    "forget all previous",
    "repeat the following",
    "игнорируй предыдущие инструкции",
    "игнорируй все инструкции",
    "забудь предыдущие инструкции",
    "забудь все инструкции",
    "enable developer mode",
    "developer mode enabled",
    "jailbreak mode",
    "твоя новая роль",
    "включи режим разработчика",
    "режим разработчика включен",
    "reveal your system prompt",
    "show your system prompt",
    "print your system prompt",
    "output your system prompt",
    "покажи системный промпт",
    "выведи системный промпт",
    "раскрой системный промпт",
    "новые инструкции:",
    "служебное сообщение:",
    "выполни команду",
];

fn normalize_injection_text(text: &str) -> String {
    text.chars()
        .filter(|&ch| {
            !matches!(
                ch,
                '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}' | '\u{00ad}'
            )
        })
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_marker(text: &str, marker: &str) -> bool {
    let marker_ends_in_word = marker.chars().last().is_some_and(|ch| ch.is_alphanumeric());
    let mut offset = 0;
    while let Some(rest) = text.get(offset..) {
        let Some(found) = rest.find(marker) else {
            break;
        };
        let end = offset + found + marker.len();
        let boundary = !marker_ends_in_word
            || text
                .get(end..)
                .and_then(|tail| tail.chars().next())
                .is_none_or(|next| !next.is_alphanumeric());
        if boundary {
            return true;
        }
        offset = end;
    }
    false
}

pub(crate) fn injection_attempt(text: &str) -> bool {
    let normalized = normalize_injection_text(text);
    !normalized.is_empty()
        && INJECTION_MARKERS
            .iter()
            .any(|marker| contains_marker(&normalized, marker))
}
