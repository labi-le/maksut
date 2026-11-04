use crate::chat_client::ChatConfig;
use crate::prelude::*;

pub(crate) struct RecentUrls {
    updated_at: time::Instant,
    urls: Vec<Url>,
}

impl ChatConfig {
    pub(crate) async fn remember_urls(&self, chat_id: ChatId, urls: &[Url]) {
        if urls.is_empty() {
            return;
        }
        let mut recent = self.recent_urls.lock().await;
        let now = time::Instant::now();
        let ttl = Duration::from_secs(config().chat.discovered_url_ttl_secs);
        let limit = config().chat.max_active_chats;
        let per_chat = config().chat.discovered_urls_per_chat;
        if recent.len() >= limit && !recent.contains_key(&chat_id) {
            recent.retain(|_, entry| now.duration_since(entry.updated_at) < ttl);
            if recent.len() >= limit {
                if let Some(oldest) = recent
                    .iter()
                    .min_by_key(|(_, entry)| entry.updated_at)
                    .map(|(chat, _)| *chat)
                {
                    recent.remove(&oldest);
                }
            }
        }
        let entry = recent.entry(chat_id).or_insert_with(|| RecentUrls {
            updated_at: now,
            urls: Vec::with_capacity(urls.len().min(per_chat)),
        });
        if now.duration_since(entry.updated_at) >= ttl {
            entry.urls.clear();
        }
        entry.updated_at = now;
        for url in urls {
            if let Some(pos) = entry.urls.iter().position(|existing| existing == url) {
                entry.urls.remove(pos);
            }
            if entry.urls.len() >= per_chat {
                entry.urls.remove(0);
            }
            entry.urls.push(url.clone());
        }
        tracing::trace!(
            chat_id = chat_id.0,
            incoming = urls.len(),
            stored = entry.urls.len(),
            ttl_secs = ttl.as_secs(),
            "urls remembered"
        );
    }

    pub(crate) async fn recent_urls(&self, chat_id: ChatId) -> Vec<Url> {
        let mut recent = self.recent_urls.lock().await;
        let now = time::Instant::now();
        let ttl = Duration::from_secs(config().chat.discovered_url_ttl_secs);
        let urls = match recent.get(&chat_id) {
            Some(entry) if now.duration_since(entry.updated_at) < ttl => entry.urls.clone(),
            Some(_) => {
                recent.remove(&chat_id);
                Vec::new()
            }
            None => Vec::new(),
        };
        tracing::trace!(
            chat_id = chat_id.0,
            urls = urls.len(),
            ttl_secs = ttl.as_secs(),
            "recent urls read"
        );
        urls
    }
}

/// Expands repository paths (`src/foo.rs#L10`, `chat_client.rs#L275-277`,
/// `tools/omp_worker.py:12`, `/docs/adr/012-free-chat.md:8`) into full GitHub
/// blob URLs while leaving existing `http(s)://` URLs untouched.
pub(crate) fn expand_repo_blob_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let ws_len: usize = rest
            .chars()
            .take_while(|ch| ch.is_whitespace())
            .map(char::len_utf8)
            .sum();
        if ws_len > 0 {
            out.push_str(rest.get(..ws_len).unwrap_or(""));
            rest = rest.get(ws_len..).unwrap_or("");
            if rest.is_empty() {
                break;
            }
        }
        let word_len: usize = rest
            .chars()
            .take_while(|ch| !ch.is_whitespace())
            .map(char::len_utf8)
            .sum();
        let word = rest.get(..word_len).unwrap_or("");
        push_expanded_repo_token(&mut out, word);
        rest = rest.get(word_len..).unwrap_or("");
    }
    out
}

fn push_expanded_repo_token(out: &mut String, word: &str) {
    if word.contains("://") {
        out.push_str(word);
        return;
    }
    if let Some((open_bracket, mid)) = word.split_once("](") {
        if open_bracket.contains('[') {
            if let Some((target, close_rest)) = mid.split_once(')') {
                if let Some(rel) = resolve_repo_target(target) {
                    out.push_str(open_bracket);
                    out.push_str("](");
                    out.push_str(crate::code_inspect::GITHUB_BLOB_BASE);
                    out.push('/');
                    out.push_str(&rel);
                    out.push(')');
                    out.push_str(close_rest);
                    return;
                }
            }
        }
    }
    let start = word
        .char_indices()
        .find(|(_, ch)| !matches!(ch, '(' | '[' | '<' | '"' | '\'' | '«' | '`'))
        .map_or(word.len(), |(idx, _)| idx);
    let tail = word.get(start..).unwrap_or("");
    let end = tail
        .char_indices()
        .rev()
        .find(|(_, ch)| {
            !matches!(
                ch,
                ')' | ']' | '>' | '"' | '\'' | '»' | '`' | ',' | '.' | ';' | ':' | '!' | '?'
            )
        })
        .map_or(start, |(idx, ch)| start + idx + ch.len_utf8());
    let core = word.get(start..end).unwrap_or("");
    let Some(normalized) = resolve_repo_target(core) else {
        out.push_str(word);
        return;
    };
    out.push_str(word.get(..start).unwrap_or("").trim_end_matches('`'));
    out.push('[');
    out.push_str(&normalized);
    out.push_str("](");
    out.push_str(crate::code_inspect::GITHUB_BLOB_BASE);
    out.push('/');
    out.push_str(&normalized);
    out.push(')');
    out.push_str(word.get(end..).unwrap_or("").trim_start_matches('`'));
}

pub(crate) fn resolve_repo_target(core: &str) -> Option<String> {
    let trimmed = core.trim_matches('`');
    let trimmed = trimmed.strip_prefix('/').unwrap_or(trimmed);
    if trimmed.is_empty() || trimmed.contains("..") || trimmed.starts_with('.') {
        return None;
    }
    let (raw_path, anchor) = match trimmed.split_once('#') {
        Some((p, frag)) => (
            p,
            normalize_line_anchor(frag).or_else(|| (!frag.is_empty()).then(|| frag.to_owned())),
        ),
        None => match trimmed.rsplit_once(':') {
            Some((p, frag)) => match normalize_line_anchor(frag) {
                Some(a) => (p, Some(a)),
                None => (trimmed, None),
            },
            None => (trimmed, None),
        },
    };
    let repo_path = if raw_path.starts_with("src/")
        || raw_path.starts_with("docs/")
        || raw_path.starts_with("tools/")
        || matches!(
            raw_path,
            "Cargo.toml"
                | "README.md"
                | "AGENTS.md"
                | "Dockerfile"
                | "compose.yaml"
                | "example.config.toml"
        ) {
        raw_path.to_owned()
    } else if !raw_path.contains('/') {
        if let Some(stem) = raw_path.strip_suffix(".rs").filter(|s| is_safe_ident(s)) {
            format!("src/{stem}.rs")
        } else {
            let stem = raw_path.strip_suffix(".py")?;
            if !is_safe_ident(stem) {
                return None;
            }
            format!("tools/{raw_path}")
        }
    } else {
        return None;
    };
    Some(match anchor {
        Some(a) => format!("{repo_path}#{a}"),
        None => repo_path,
    })
}

fn is_safe_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn normalize_line_anchor(frag: &str) -> Option<String> {
    let rest = frag.strip_prefix('L').unwrap_or(frag);
    if let Some((start, end)) = rest.split_once('-') {
        let end = end.strip_prefix('L').unwrap_or(end);
        if is_line_num(start) && is_line_num(end) {
            return Some(format!("L{start}-L{end}"));
        }
        return None;
    }
    is_line_num(rest).then(|| format!("L{rest}"))
}

fn is_line_num(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('0') && s.chars().all(|c| c.is_ascii_digit())
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
    let weekday = ["вс", "пн", "вт", "ср", "чт", "пт", "сб"]
        .get((days + 4).rem_euclid(7) as usize)
        .copied()
        .unwrap_or("сб");
    format!("{year:04}-{month:02}-{day:02} ({weekday}) {hour:02}:{minute:02} МСК (UTC+3)")
}

pub(crate) fn sticker_turn_hint(prompt: &str) -> &'static str {
    if crate::chat_style::explicit_sticker_request(prompt) {
        "пользователь просит стикер или спрашивает про стикеры — обязательно вызови send_sticker в этом ответе\n"
    } else if crate::chat_style::sticker_emotion_hint(prompt) {
        "в реплике яркая эмоция, подкол или живой диалог — если это не research и не длинный разбор кода, вызови send_sticker (с коротким query по смыслу или через find_stickers) вместе с ответом\n"
    } else {
        ""
    }
}

pub(crate) fn is_bidi_or_control(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch as u32,
            0x00AD | 0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF
        )
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
