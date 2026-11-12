use crate::prelude::*;
use crate::RouterMode;

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
    let cfg = config();
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(cfg.telegram.connect_timeout_secs))
        .timeout(Duration::from_secs(cfg.chat.image_timeout_secs))
        .pool_idle_timeout(Duration::from_secs(cfg.telegram.pool_idle_timeout_secs))
        .tcp_keepalive(Duration::from_secs(cfg.telegram.tcp_keepalive_secs))
        .tcp_nodelay(true);
    crate::apply_telegram_dns_fallback(builder)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});
type PendingImage = (FileId, String);

static RECENT_IMAGES: std::sync::LazyLock<
    tokio::sync::Mutex<std::collections::HashMap<ChatId, (time::Instant, PendingImage)>>,
> = std::sync::LazyLock::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

async fn note_recent_image(chat_id: ChatId, img: &PendingImage) {
    let mut guard = RECENT_IMAGES.lock().await;
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
    guard.insert(chat_id, (now, img.clone()));
}

pub(crate) async fn take_recent_image(chat_id: ChatId) -> Option<PendingImage> {
    let mut guard = RECENT_IMAGES.lock().await;
    let now = time::Instant::now();
    if guard
        .get(&chat_id)
        .is_some_and(|(at, _)| now.duration_since(*at) >= Duration::from_secs(300))
    {
        guard.remove(&chat_id);
        return None;
    }
    guard.remove(&chat_id).map(|(_, img)| img)
}

async fn peek_recent_image(chat_id: ChatId) -> Option<PendingImage> {
    let mut guard = RECENT_IMAGES.lock().await;
    let now = time::Instant::now();
    match guard.get(&chat_id) {
        Some((at, img)) if now.duration_since(*at) < Duration::from_secs(300) => Some(img.clone()),
        Some(_) => {
            guard.remove(&chat_id);
            None
        }
        None => None,
    }
}

fn references_recent_image(text: &str) -> bool {
    let lower = text.to_lowercase();
    const STEMS: &[&str] = &["картинк", "фотк", "изображени", "пикч", "скриншот"];
    const WORDS: &[&str] = &[
        "скрин",
        "скрине",
        "скрина",
        "скрину",
        "скрином",
        "фото",
        "мем",
        "меме",
        "мема",
    ];
    const SHORT: &[&str] = &[
        "че это",
        "чё это",
        "что это",
        "а это",
        "а тут",
        "а здесь",
        "держи еще раз",
        "держи ещё раз",
    ];
    let word_hit = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(|w| WORDS.contains(&w) || STEMS.iter().any(|s| w.starts_with(s)));
    word_hit || (lower.chars().count() <= 40 && SHORT.iter().any(|k| lower.contains(k)))
}

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
    let bytes = download_file_bytes(bot, file_id, config().chat.image_max_bytes).await?;
    Some(ChatImage {
        bytes,
        mime: mime.to_owned(),
    })
}

pub(crate) async fn download_raw_file(
    bot: &Bot,
    file_id: &FileId,
    max_bytes: u64,
) -> Option<Vec<u8>> {
    let Ok(_permit) = IMAGE_SEMAPHORE.acquire().await else {
        return None;
    };
    download_file_bytes(bot, file_id, max_bytes).await
}

async fn download_file_bytes(bot: &Bot, file_id: &FileId, max_bytes: u64) -> Option<Vec<u8>> {
    for attempt in 0..2u8 {
        match download_bytes_once(bot, file_id, max_bytes).await {
            Ok(bytes) => return Some(bytes),
            Err(DownloadImageError::Permanent) => return None,
            Err(DownloadImageError::Transient) if attempt == 0 => {
                time::sleep(Duration::from_millis(150)).await;
            }
            Err(DownloadImageError::Transient) => {}
        }
    }
    tracing::warn!("Chat attachment download failed after 2 attempts");
    None
}

async fn download_bytes_once(
    bot: &Bot,
    file_id: &FileId,
    max_bytes: u64,
) -> Result<Vec<u8>, DownloadImageError> {
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
    let mut response = IMAGE_CLIENT
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
    if response.content_length().is_some_and(|len| len > max_bytes) {
        return Err(DownloadImageError::Permanent);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| DownloadImageError::Transient)?
    {
        if bytes.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(DownloadImageError::Permanent);
        }
        bytes.extend_from_slice(&chunk);
    }
    (!bytes.is_empty())
        .then_some(bytes)
        .ok_or(DownloadImageError::Permanent)
}

pub(crate) struct ResolvedChatInput {
    pub(crate) prompt: String,
    pub(crate) followup: bool,
    pub(crate) address: crate::addressing::AddressKind,
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
    let own_text = message.text().or_else(|| message.caption()).unwrap_or("");
    let current_media = message_image(message);
    let has_current_media = current_media.is_some();
    if !message.chat.is_private() && own_text.trim().is_empty() && message_has_own_photo(message) {
        if let Some(ref img) = current_media {
            note_recent_image(message.chat.id, img).await;
        }
    }
    let has_reply_photo = !reply_ignored
        && message
            .reply_to_message()
            .is_some_and(message_has_own_photo);
    let media = current_media.or_else(|| {
        (!reply_ignored)
            .then(|| message.reply_to_message().and_then(message_image))
            .flatten()
    });
    // The classifier repeats the name-form scan `resolve_addressed_chat` does
    // next; mode `off` never reads the result, so do not pay for it.
    let address = if cfg.router.mode() == RouterMode::Off {
        crate::addressing::AddressKind::None
    } else {
        crate::addressing::address_kind(message, text, &cfg.bot_username)
    };
    let (addressed, bare_sticker_reply) = crate::chat_style::resolve_addressed_chat(
        message,
        text,
        &cfg.bot_username,
        media.is_some(),
    );
    let is_addressed = addressed.is_some();
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
            if active {
                (trimmed.to_owned(), true)
            } else if cfg.router.mode() != RouterMode::Off && !trimmed.is_empty() {
                // A router mode may wake the bot on an unaddressed message;
                // the decision happens before any model or media work.
                (trimmed.to_owned(), false)
            } else {
                return None;
            }
        }
        None => return None,
    };
    let reply_context = message
        .reply_to_message()
        .and_then(|reply| crate::chat_style::format_reply_context(reply, reply_ignored));
    let direct_media = media.filter(|_| !followup || has_current_media || has_reply_photo);
    let pending_media = match direct_media {
        Some(img) => {
            if has_current_media && !message.chat.is_private() && !own_text.trim().is_empty() {
                let _ = take_recent_image(message.chat.id).await;
            }
            Some(img)
        }
        None if !message.chat.is_private()
            && !reply_ignored
            && (is_addressed || followup)
            && references_recent_image(prompt.split("[файл ").next().unwrap_or(&prompt)) =>
        {
            peek_recent_image(message.chat.id).await
        }
        None => None,
    };
    let warn_missing_image = pending_media.is_some() && !bare_sticker_reply;
    Some(ResolvedChatInput {
        prompt,
        followup,
        address,
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
