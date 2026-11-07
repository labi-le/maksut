use crate::prelude::*;

const DOC_MAX_BYTES: u64 = 512 * 1024;
const DOC_MAX_CHARS: usize = 12_000;

const TEXT_EXTENSIONS: &[&str] = &[
    "txt",
    "csv",
    "tsv",
    "log",
    "json",
    "jsonl",
    "ndjson",
    "md",
    "markdown",
    "rst",
    "toml",
    "yaml",
    "yml",
    "xml",
    "html",
    "htm",
    "css",
    "js",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "jsx",
    "py",
    "rs",
    "go",
    "c",
    "h",
    "cpp",
    "hpp",
    "cc",
    "java",
    "kt",
    "swift",
    "rb",
    "php",
    "sh",
    "bash",
    "zsh",
    "fish",
    "sql",
    "ini",
    "cfg",
    "conf",
    "env",
    "nix",
    "dockerfile",
    "makefile",
    "patch",
    "diff",
];

#[derive(Clone, Debug)]
pub(crate) struct DocCandidate {
    pub(crate) file_id: FileId,
    pub(crate) name: String,
    pub(crate) from_reply: bool,
    pub(crate) author: Option<String>,
}

pub(crate) struct DocTurn {
    pub(crate) text: Option<String>,
    pub(crate) quoted: bool,
}

static RECENT_DOCS: std::sync::LazyLock<
    tokio::sync::Mutex<std::collections::HashMap<ChatId, (time::Instant, DocCandidate)>>,
> = std::sync::LazyLock::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

async fn note_recent_doc(chat_id: ChatId, cand: &DocCandidate) {
    let mut guard = RECENT_DOCS.lock().await;
    let now = time::Instant::now();
    let ttl = Duration::from_secs(300);
    if guard.len() >= 256 && !guard.contains_key(&chat_id) {
        guard.retain(|_, (at, _)| now.duration_since(*at) < ttl);
        while guard.len() >= 256 {
            let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(chat, _)| *chat)
            else {
                break;
            };
            guard.remove(&oldest);
        }
    }
    guard.insert(chat_id, (now, cand.clone()));
}

pub(crate) async fn clear_recent_doc(chat_id: ChatId) {
    RECENT_DOCS.lock().await.remove(&chat_id);
}

async fn take_recent_doc(chat_id: ChatId) -> Option<DocCandidate> {
    let mut guard = RECENT_DOCS.lock().await;
    let now = time::Instant::now();
    match guard.remove(&chat_id) {
        Some((at, mut cand)) if now.duration_since(at) < Duration::from_secs(300) => {
            cand.from_reply = true;
            Some(cand)
        }
        _ => None,
    }
}

/// Checks whether `doc` is a text/code/log/CSV document within `DOC_MAX_BYTES`.
pub(crate) fn is_text_document(doc: &teloxide::types::Document) -> bool {
    if u64::from(doc.file.size) > DOC_MAX_BYTES {
        return false;
    }
    let mime = doc
        .mime_type
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let essence = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if essence.starts_with("image/")
        || essence.starts_with("video/")
        || essence.starts_with("audio/")
    {
        return false;
    }
    if let Some(name) = doc.file_name.as_deref() {
        let lower = name.trim().to_ascii_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or_default();
        if TEXT_EXTENSIONS.contains(&ext) {
            return true;
        }
    }
    essence.starts_with("text/")
        || matches!(
            essence.as_str(),
            "application/json"
                | "application/ld+json"
                | "application/x-ndjson"
                | "application/xml"
                | "application/toml"
                | "application/x-yaml"
                | "application/yaml"
                | "application/javascript"
                | "application/x-sh"
                | "application/sql"
                | "application/csv"
        )
}

pub(crate) fn candidate(message: &Message, reply_ignored: bool) -> Option<DocCandidate> {
    if let Some(doc) = message.document().filter(|d| is_text_document(d)) {
        let name = sanitize_doc_name(doc.file_name.as_deref());
        return Some(DocCandidate {
            file_id: doc.file.id.clone(),
            name,
            from_reply: false,
            author: None,
        });
    }
    if reply_ignored {
        return None;
    }
    let reply = message.reply_to_message()?;
    let doc = reply.document().filter(|d| is_text_document(d))?;
    let name = sanitize_doc_name(doc.file_name.as_deref());
    let author = reply
        .from
        .as_ref()
        .map(crate::chat_style::display_author_name);
    Some(DocCandidate {
        file_id: doc.file.id.clone(),
        name,
        from_reply: true,
        author,
    })
}

pub(crate) fn pending(message: &Message) -> bool {
    candidate(message, false).is_some()
}

pub(crate) fn reply_label(doc: &teloxide::types::Document) -> String {
    format!("[файл {}]", sanitize_doc_name(doc.file_name.as_deref()))
}

pub(crate) fn select_injection_text<'a>(
    typed: &'a str,
    voice_text: Option<&'a str>,
    voice_quoted: bool,
    doc_text: Option<&'a str>,
    doc_quoted: bool,
    merged: &'a str,
) -> std::borrow::Cow<'a, str> {
    match (voice_quoted, doc_quoted) {
        (false, false) => std::borrow::Cow::Borrowed(merged),
        (false, true) => std::borrow::Cow::Borrowed(voice_text.unwrap_or(typed)),
        (true, false) => match (voice_text, doc_text) {
            (Some(vt), Some(dt)) => {
                let doc_only = dt
                    .strip_prefix(vt)
                    .and_then(|s| s.strip_prefix('\n'))
                    .unwrap_or("");
                if typed.trim().is_empty() {
                    std::borrow::Cow::Borrowed(doc_only)
                } else if doc_only.is_empty() {
                    std::borrow::Cow::Borrowed(typed)
                } else {
                    std::borrow::Cow::Owned(format!("{typed}\n{doc_only}"))
                }
            }
            (None, Some(dt)) => std::borrow::Cow::Borrowed(dt),
            _ => std::borrow::Cow::Borrowed(typed),
        },
        (true, true) => std::borrow::Cow::Borrowed(typed),
    }
}
fn sanitize_doc_name(raw: Option<&str>) -> String {
    let cleaned: String = raw
        .unwrap_or("document.txt")
        .chars()
        .filter(|ch| !ch.is_control() && !matches!(ch, '\n' | '\r' | ']' | '['))
        .take(80)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "document.txt".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Downloads and decodes a text document when the message may reach the chat
/// model. Binary files with NUL bytes are skipped; ignored senders, non-owner
/// private chats, and disabled groups never pay for a download.
pub(crate) async fn turn(
    bot: &Bot,
    message: &Message,
    cfg: &crate::Settings,
    typed: &str,
    owner: bool,
    sender_ignored: bool,
    reply_ignored: bool,
) -> DocTurn {
    if sender_ignored || (!owner && message.chat.is_private()) || cfg.chat.is_none() {
        return DocTurn {
            text: None,
            quoted: false,
        };
    }
    let direct_cand = candidate(message, reply_ignored);
    if let Some(mut cand) = direct_cand.clone() {
        if !cand.from_reply && !message.chat.is_private() {
            if crate::group_enabled(&cfg.database, message.chat.id)
                .await
                .is_ok_and(|enabled| enabled)
            {
                cand.author = message
                    .from
                    .as_ref()
                    .map(crate::chat_style::display_author_name);
                note_recent_doc(message.chat.id, &cand).await;
            } else {
                return DocTurn {
                    text: None,
                    quoted: false,
                };
            }
        }
    }
    if !message.chat.is_private()
        && !crate::group_enabled(&cfg.database, message.chat.id)
            .await
            .is_ok_and(|enabled| enabled)
    {
        return DocTurn {
            text: None,
            quoted: false,
        };
    }
    if !message.chat.is_private()
        && crate::addressing::address_kind(message, typed.trim(), &cfg.bot_username)
            == crate::addressing::AddressKind::None
    {
        let active = match cfg.chat.as_ref() {
            Some(chat) => chat.followup_active(message.chat.id).await,
            None => false,
        };
        if !active {
            return DocTurn {
                text: None,
                quoted: false,
            };
        }
    }
    let cand = match direct_cand {
        Some(cand) => cand,
        None if !reply_ignored && !typed.trim().is_empty() => {
            let Some(cand) = take_recent_doc(message.chat.id).await else {
                return DocTurn {
                    text: None,
                    quoted: false,
                };
            };
            cand
        }
        None => {
            return DocTurn {
                text: None,
                quoted: false,
            };
        }
    };
    let Some(bytes) = crate::chat_media::download_raw_file(bot, &cand.file_id, DOC_MAX_BYTES).await
    else {
        return DocTurn {
            text: None,
            quoted: false,
        };
    };
    let Some(block) = format_doc_block(&cand, &bytes) else {
        return DocTurn {
            text: None,
            quoted: false,
        };
    };
    let merged = if typed.trim().is_empty() {
        block
    } else {
        format!("{typed}\n{block}")
    };
    DocTurn {
        text: Some(merged),
        quoted: cand.from_reply,
    }
}

pub(crate) fn format_doc_block(cand: &DocCandidate, bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let cleaned = text.replace('\r', "");
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    let total_chars = trimmed.chars().count();
    let body = match trimmed.char_indices().nth(DOC_MAX_CHARS) {
        Some((idx, _)) => format!(
            "{}\n...[обрезано до {DOC_MAX_CHARS} из {total_chars} симв.]",
            trimmed.get(..idx).unwrap_or(trimmed)
        ),
        None => trimmed.to_owned(),
    };
    let head = match cand.author.as_deref() {
        Some(author) => format!("[файл {}, {author}]:\n", cand.name),
        None => format!("[файл {}]:\n", cand.name),
    };
    Some(format!("{head}{body}"))
}
