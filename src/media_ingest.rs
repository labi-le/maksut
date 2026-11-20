use crate::garage::GarageClient;
use crate::*;

/// One attachment chosen for storage. Only message metadata is read here, so the
/// dispatch path pays no network cost for the decision.
pub(crate) struct MediaCandidate {
    pub(crate) chat_id: i64,
    pub(crate) message_id: i64,
    pub(crate) kind: &'static str,
    pub(crate) file_id: FileId,
    pub(crate) file_unique_id: String,
    pub(crate) content_type: String,
    pub(crate) is_group: bool,
}

static INGEST_SEMAPHORE: std::sync::LazyLock<Semaphore> =
    std::sync::LazyLock::new(|| Semaphore::new(config().garage.concurrency));

/// Picks the attachment of a message that fits the storage bounds. Messages with
/// several photo sizes store the largest one; animations and videos are gated by
/// duration before any download happens.
pub(crate) fn candidate(message: &Message, settings: &GarageSettings) -> Option<MediaCandidate> {
    let (file_id, file_unique_id, kind, content_type) = if let Some(photo) = message.photo() {
        let best = photo
            .iter()
            .max_by_key(|size| u64::from(size.width) * u64::from(size.height))?;
        if !fits_size(best.file.size, settings) {
            return None;
        }
        (
            best.file.id.clone(),
            best.file.unique_id.0.clone(),
            "photo",
            "image/jpeg".to_owned(),
        )
    } else if let Some(video) = message.video() {
        if !fits_size(video.file.size, settings)
            || !fits_duration(video.duration.seconds(), settings)
        {
            return None;
        }
        (
            video.file.id.clone(),
            video.file.unique_id.0.clone(),
            "video",
            mime_or(video.mime_type.as_ref(), "video/mp4"),
        )
    } else if let Some(animation) = message.animation() {
        if !fits_size(animation.file.size, settings)
            || !fits_duration(animation.duration.seconds(), settings)
        {
            return None;
        }
        (
            animation.file.id.clone(),
            animation.file.unique_id.0.clone(),
            "animation",
            mime_or(animation.mime_type.as_ref(), "video/mp4"),
        )
    } else if let Some(document) = message.document() {
        if !fits_size(document.file.size, settings) {
            return None;
        }
        (
            document.file.id.clone(),
            document.file.unique_id.0.clone(),
            "document",
            mime_or(document.mime_type.as_ref(), "application/octet-stream"),
        )
    } else if let Some(audio) = message.audio() {
        if !fits_size(audio.file.size, settings) {
            return None;
        }
        (
            audio.file.id.clone(),
            audio.file.unique_id.0.clone(),
            "audio",
            mime_or(audio.mime_type.as_ref(), "audio/mpeg"),
        )
    } else if let Some(voice) = message.voice() {
        if !fits_size(voice.file.size, settings) {
            return None;
        }
        (
            voice.file.id.clone(),
            voice.file.unique_id.0.clone(),
            "voice",
            mime_or(voice.mime_type.as_ref(), "audio/ogg"),
        )
    } else {
        let note = message.video_note()?;
        if !fits_size(note.file.size, settings) {
            return None;
        }
        (
            note.file.id.clone(),
            note.file.unique_id.0.clone(),
            "video_note",
            "video/mp4".to_owned(),
        )
    };
    Some(MediaCandidate {
        chat_id: message.chat.id.0,
        message_id: i64::from(message.id.0),
        kind,
        file_id,
        file_unique_id,
        content_type,
        is_group: message.chat.is_group() || message.chat.is_supergroup(),
    })
}

fn fits_size(size: u32, settings: &GarageSettings) -> bool {
    size != 0 && u64::from(size) <= settings.max_bytes
}

fn fits_duration(seconds: u32, settings: &GarageSettings) -> bool {
    u64::from(seconds) <= settings.max_video_secs
}

fn mime_or<M: AsRef<str>>(mime: Option<&M>, fallback: &str) -> String {
    mime.map_or_else(|| fallback.to_owned(), |mime| mime.as_ref().to_owned())
}

pub(crate) fn ingest_allowed(garage_enabled: bool, flags: i64) -> bool {
    garage_enabled && flags & (crate::memory::FLAG_SECRET | crate::memory::FLAG_INJECTION) == 0
}

/// Starts storage for one message without delaying its reply. Secret or
/// injection-flagged messages are skipped: a screenshot with a password must not
/// live in the object store forever.
pub(crate) fn spawn(bot: &Bot, cfg: &Arc<Settings>, message: &Message, flags: i64) {
    if !ingest_allowed(cfg.garage.is_some(), flags) {
        return;
    }
    let Some(candidate) = candidate(message, &config().garage) else {
        return;
    };
    let bot = bot.clone();
    let cfg = Arc::clone(cfg);
    tokio::spawn(async move {
        ingest(bot, cfg, candidate).await;
    });
}

async fn ingest(bot: Bot, cfg: Arc<Settings>, candidate: MediaCandidate) {
    let Some(client) = cfg.garage.as_deref() else {
        return;
    };
    let Ok(_permit) = INGEST_SEMAPHORE.acquire().await else {
        return;
    };
    if candidate.is_group
        && !group_enabled(&cfg.database, ChatId(candidate.chat_id))
            .await
            .is_ok_and(|enabled| enabled)
    {
        return;
    }
    let max_bytes = config().garage.max_bytes;
    let Some(bytes) =
        crate::chat_media::download_raw_file(&bot, &candidate.file_id, max_bytes).await
    else {
        tracing::warn!(
            stage = "download",
            kind = candidate.kind,
            "chat media ingest skipped"
        );
        return;
    };
    match persist(&cfg.database, client, &candidate, bytes).await {
        Ok(()) => {}
        Err(error) => tracing::warn!(
            stage = error.stage(),
            kind = candidate.kind,
            "chat media ingest failed"
        ),
    }
}

/// Stores one downloaded blob and its message binding. Blobs are content
/// addressed, so a repeated byte sequence skips the upload.
pub(crate) async fn persist(
    pool: &SqlitePool,
    client: &GarageClient,
    candidate: &MediaCandidate,
    bytes: Vec<u8>,
) -> Result<(), IngestError> {
    let now = crate::memory::unix_now();
    let storage_key = sha256_hex(&bytes);
    let stored_bytes = bytes.len() as i64;
    if !crate::media_store::blob_known(pool, &storage_key)
        .await
        .map_err(|_| IngestError::Store)?
    {
        client
            .put(&storage_key, bytes, &candidate.content_type)
            .await
            .map_err(|_| IngestError::Upload)?;
        crate::media_store::insert_blob(
            pool,
            &crate::media_store::BlobRecord {
                sha256: &storage_key,
                kind: candidate.kind,
                bytes: stored_bytes,
                created_at: now,
            },
        )
        .await
        .map_err(|_| IngestError::Store)?;
    }
    crate::media_store::insert_binding(
        pool,
        &crate::media_store::MediaBinding {
            chat_id: candidate.chat_id,
            message_id: candidate.message_id,
            kind: candidate.kind,
            file_unique_id: &candidate.file_unique_id,
            file_id: candidate.file_id.0.as_str(),
            storage_key: &storage_key,
        },
        now,
    )
    .await
    .map_err(|_| IngestError::Store)
}

/// Failure categories only: raw storage errors stay out of logs and chat, the
/// same way download stderr does.
#[derive(Debug)]
pub(crate) enum IngestError {
    Store,
    Upload,
}

impl IngestError {
    fn stage(&self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::Upload => "upload",
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut key = String::with_capacity(64);
    for byte in digest {
        let _ = write!(key, "{byte:02x}");
    }
    key
}
