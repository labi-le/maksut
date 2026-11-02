use crate::prelude::*;

#[derive(Clone)]
pub(crate) struct ChatImage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) mime: String,
}

/// The image or sticker frame that best represents a message: the largest
/// photo, a static sticker, or a thumbnail for animated and video media.
pub(crate) fn message_image(message: &Message) -> Option<(FileId, String)> {
    if let Some(sizes) = message.photo() {
        let best = sizes
            .iter()
            .max_by_key(|photo| u64::from(photo.width) * u64::from(photo.height))?;
        return Some((best.file.id.clone(), "image/jpeg".into()));
    }
    if let Some(sticker) = message.sticker() {
        if sticker.is_animated() || sticker.is_video() {
            return sticker
                .thumbnail
                .as_ref()
                .map(|thumbnail| (thumbnail.file.id.clone(), "image/jpeg".into()));
        }
        return Some((sticker.file.id.clone(), "image/webp".into()));
    }
    if let Some(animation) = message.animation() {
        return animation
            .thumbnail
            .as_ref()
            .map(|thumbnail| (thumbnail.file.id.clone(), "image/jpeg".into()));
    }
    if let Some(video) = message.video() {
        return video
            .thumbnail
            .as_ref()
            .map(|thumbnail| (thumbnail.file.id.clone(), "image/jpeg".into()));
    }
    if let Some(note) = message.video_note() {
        return note
            .thumbnail
            .as_ref()
            .map(|thumbnail| (thumbnail.file.id.clone(), "image/jpeg".into()));
    }
    if let Some(document) = message.document() {
        let mime = document.mime_type.as_ref()?.to_string();
        if mime.starts_with("image/") {
            return Some((document.file.id.clone(), mime));
        }
    }
    None
}

static IMAGE_SEMAPHORE: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(config().chat.image_concurrency));

/// One client for every attachment download: building a fresh one per photo
/// message threw away the connection pool and its TLS setup.
static IMAGE_CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(config().chat.image_timeout_secs))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

pub(crate) async fn download_pending_image(
    bot: &Bot,
    pending: Option<&(FileId, String)>,
) -> (
    Option<ChatImage>,
    Option<tokio::sync::SemaphorePermit<'static>>,
) {
    let Some((file_id, mime)) = pending else {
        return (None, None);
    };
    let Ok(permit) = IMAGE_SEMAPHORE.acquire().await else {
        return (None, None);
    };
    let img = download_image(bot, file_id, mime).await;
    let held = img.is_some().then_some(permit);
    (img, held)
}

enum DownloadImageError {
    Transient,
    Permanent,
}

pub(crate) async fn download_image(bot: &Bot, file_id: &FileId, mime: &str) -> Option<ChatImage> {
    for attempt in 0..2u8 {
        match download_image_once(bot, file_id, mime).await {
            Ok(image) => return Some(image),
            Err(DownloadImageError::Permanent) => return None,
            Err(DownloadImageError::Transient) if attempt == 0 => {
                time::sleep(Duration::from_millis(150)).await;
            }
            Err(DownloadImageError::Transient) => {}
        }
    }
    tracing::warn!("Chat image download failed after 2 attempts");
    None
}

async fn download_image_once(
    bot: &Bot,
    file_id: &FileId,
    mime: &str,
) -> Result<ChatImage, DownloadImageError> {
    let file = bot
        .get_file(file_id.clone())
        .await
        .map_err(|err| match err {
            teloxide::RequestError::Network(_) | teloxide::RequestError::RetryAfter(_) => {
                DownloadImageError::Transient
            }
            teloxide::RequestError::Api(teloxide::ApiError::Unknown(ref msg))
                if msg.contains("500") || msg.contains("502") || msg.contains("503") =>
            {
                DownloadImageError::Transient
            }
            teloxide::RequestError::Api(_)
            | teloxide::RequestError::MigrateToChatId(_)
            | teloxide::RequestError::InvalidJson { .. }
            | teloxide::RequestError::Io(_) => DownloadImageError::Permanent,
        })?;
    let url = bot
        .api_url()
        .join(&format!("file/bot{}/{}", bot.token(), file.path))
        .map_err(|_| DownloadImageError::Permanent)?;
    let response = IMAGE_CLIENT
        .get(url)
        .send()
        .await
        .map_err(|_| DownloadImageError::Transient)?;
    let status = response.status();
    if !status.is_success() {
        return if status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429 {
            Err(DownloadImageError::Transient)
        } else {
            Err(DownloadImageError::Permanent)
        };
    }
    if response
        .content_length()
        .is_some_and(|length| length > config().chat.image_max_bytes)
    {
        return Err(DownloadImageError::Permanent);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| DownloadImageError::Transient)?
        .to_vec();
    if bytes.is_empty() || bytes.len() as u64 > config().chat.image_max_bytes {
        return Err(DownloadImageError::Permanent);
    }
    Ok(ChatImage {
        bytes,
        mime: mime.to_owned(),
    })
}

pub(crate) struct ResolvedChatInput {
    pub(crate) prompt: String,
    pub(crate) followup: bool,
    pub(crate) pending_media: Option<(FileId, String)>,
    pub(crate) warn_missing_image: bool,
    pub(crate) reply_context: Option<String>,
}

pub(crate) fn message_has_own_photo(message: &Message) -> bool {
    message.photo().is_some()
        || message
            .document()
            .and_then(|doc| doc.mime_type.as_ref())
            .is_some_and(|mime| mime.as_ref().starts_with("image/"))
}

pub(crate) fn message_has_attachment(message: &Message) -> bool {
    message.photo().is_some()
        || message.video().is_some()
        || message.animation().is_some()
        || message.document().is_some()
        || message.audio().is_some()
        || message.voice().is_some()
        || message.video_note().is_some()
}

pub(crate) async fn resolve_chat_input(
    message: &Message,
    text: &str,
    cfg: &crate::Settings,
    reply_ignored: bool,
) -> Option<ResolvedChatInput> {
    let chat = cfg.chat.as_ref()?;
    let current_media = message_image(message);
    let has_current_media = current_media.is_some();
    let has_reply_photo = !reply_ignored
        && message
            .reply_to_message()
            .is_some_and(message_has_own_photo);
    let media = current_media.or_else(|| {
        (!reply_ignored)
            .then(|| message.reply_to_message().and_then(message_image))
            .flatten()
    });
    let (addressed, bare_sticker_reply) = crate::chat_style::resolve_addressed_chat(
        message,
        text,
        &cfg.bot_username,
        media.is_some(),
    );
    let (prompt, followup) = match addressed {
        Some(pair) => pair,
        None if !message.chat.is_private() => {
            let trimmed = text.trim();
            let active =
                if !trimmed.is_empty() && (!has_current_media || message_has_own_photo(message)) {
                    chat.followup_active(message.chat.id).await
                } else {
                    false
                };
            active.then(|| (trimmed.to_owned(), true))?
        }
        None => return None,
    };
    let reply_context = message
        .reply_to_message()
        .and_then(|reply| crate::chat_style::format_reply_context(reply, reply_ignored));
    let pending_media = media.filter(|_| !followup || has_current_media || has_reply_photo);
    let warn_missing_image = pending_media.is_some() && !bare_sticker_reply;
    Some(ResolvedChatInput {
        prompt,
        followup,
        pending_media,
        warn_missing_image,
        reply_context,
    })
}

pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let Some((first, rest)) = chunk.split_first() else {
            continue;
        };
        let second = rest.first().copied().unwrap_or(0);
        let third = rest.get(1).copied().unwrap_or(0);
        let quad = [
            usize::from(first >> 2),
            usize::from((first & 0x03) << 4 | second >> 4),
            usize::from((second & 0x0f) << 2 | third >> 6),
            usize::from(third & 0x3f),
        ];
        for (index, padded) in quad
            .into_iter()
            .zip([false, false, rest.is_empty(), rest.len() < 2])
        {
            if padded {
                encoded.push('=');
            } else {
                encoded.extend(TABLE.get(index).copied().map(char::from));
            }
        }
    }
    encoded
}
