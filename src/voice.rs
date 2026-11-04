use crate::*;
use serde_json::json;
use teloxide::types::Audio;

/// System prompt for the speech-to-text call: verbatim transcription in the
/// original language, no commentary, a sentinel when the audio has no speech.
/// The untrusted transcript is labelled by `format_block`.
const TRANSCRIBE_PROMPT: &str = "Дословно расшифруй голосовое сообщение на языке оригинала без комментариев и кавычек. Если речи нет — верни [неразборчиво].";

/// Substituted when the attachment cannot be downloaded or transcribed.
pub(crate) const UNRECOGNISED: &str = "[не удалось распознать голосовое]";

/// A voice note or audio file that can be sent to the transcription model.
#[derive(Clone, Debug)]
pub(crate) struct VoiceAttachment {
    pub(crate) file_id: FileId,
    pub(crate) duration_secs: u32,
    /// OpenAI `input_audio.format`: ogg, mp3, wav, flac, m4a, webm.
    pub(crate) format: &'static str,
    pub(crate) label: &'static str,
    /// Set only for a replied attachment, so the block never attributes the
    /// quoted speech to the person quoting it.
    pub(crate) author: Option<String>,
}

/// The attachment chosen for this message plus where it came from.
pub(crate) struct VoiceCandidate {
    pub(crate) attachment: VoiceAttachment,
    pub(crate) from_reply: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum VoiceError {
    Disabled,
    TooLong,
    Timeout,
    Download,
    Transcription,
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Disabled => "disabled",
            Self::TooLong => "too_long",
            Self::Timeout => "timeout",
            Self::Download => "download",
            Self::Transcription => "transcription",
        })
    }
}

static VOICE_SEMAPHORE: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(config().voice.concurrency));

/// One client for attachment download and transcription; both are bounded by
/// `voice.timeout_secs` instead of the longer model timeout.
static VOICE_CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(config().voice.timeout_secs))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

pub(crate) fn enabled() -> bool {
    let voice = &config().voice;
    voice.enabled && !voice.model.trim().is_empty()
}

/// The current message's voice note or audio, else the voice note it replies
/// to. Audio with an unknown container is not offered a provider format.
pub(crate) fn candidate(message: &Message) -> Option<VoiceCandidate> {
    if let Some(attachment) = attachment(message) {
        return Some(VoiceCandidate {
            attachment,
            from_reply: false,
        });
    }
    let reply = message.reply_to_message()?;
    let mut attachment = attachment(reply)?;
    attachment.author = reply
        .from
        .as_ref()
        .map(crate::chat_style::display_author_name);
    Some(VoiceCandidate {
        attachment,
        from_reply: true,
    })
}

fn attachment(message: &Message) -> Option<VoiceAttachment> {
    if let Some(voice) = message.voice() {
        return Some(VoiceAttachment {
            file_id: voice.file.id.clone(),
            duration_secs: voice.duration.seconds(),
            format: "ogg",
            label: "голосовое",
            author: None,
        });
    }
    let audio = message.audio()?;
    Some(VoiceAttachment {
        file_id: audio.file.id.clone(),
        duration_secs: audio.duration.seconds(),
        format: audio_format(audio)?,
        label: "аудио",
        author: None,
    })
}

fn audio_format(audio: &Audio) -> Option<&'static str> {
    if let Some(name) = audio.file_name.as_deref() {
        let extension = name.rsplit('.').next().unwrap_or_default();
        if let Some(format) = known_format(extension) {
            return Some(format);
        }
    }
    let mime = audio.mime_type.as_ref()?.to_string();
    let essence = mime.split(';').next().unwrap_or_default().trim();
    match essence {
        "audio/ogg" | "audio/opus" | "audio/x-ogg" => Some("ogg"),
        "audio/mpeg" | "audio/mp3" | "audio/mpeg3" => Some("mp3"),
        "audio/wav" | "audio/wave" | "audio/x-wav" => Some("wav"),
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" => Some("m4a"),
        "audio/webm" => Some("webm"),
        _ => None,
    }
}

fn known_format(extension: &str) -> Option<&'static str> {
    match extension
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "ogg" | "oga" | "opus" => Some("ogg"),
        "mp3" | "mpga" => Some("mp3"),
        "wav" | "wave" => Some("wav"),
        "flac" => Some("flac"),
        "m4a" | "mp4" | "aac" => Some("m4a"),
        "mpeg" => Some("mp3"),
        "webm" => Some("webm"),
        _ => None,
    }
}

/// Downloads the attachment, transcribes it, and returns the prompt block with
/// its duration, the untrusted-input label, and the provider-reported usage.
/// Every failure is the caller's cue to substitute [`UNRECOGNISED`].
pub(crate) async fn transcribe(
    bot: &Bot,
    attachment: &VoiceAttachment,
    api_key: &str,
    base_url: &Url,
) -> Result<(String, Option<Usage>), VoiceError> {
    let settings = &config().voice;
    if !enabled() {
        return Err(VoiceError::Disabled);
    }
    if u64::from(attachment.duration_secs) > settings.max_duration_secs {
        return Err(VoiceError::TooLong);
    }
    // `bot.get_file` uses the Telegram client's much longer timeout, so only
    // this outer deadline bounds the whole acquire + download + STT chain.
    let bounded = async {
        let Ok(_permit) = VOICE_SEMAPHORE.acquire().await else {
            return Err(VoiceError::Download);
        };
        let bytes = download(bot, attachment).await?;
        transcribe_bytes(&bytes, attachment.format, api_key, base_url).await
    };
    let (transcript, usage) = time::timeout(Duration::from_secs(settings.timeout_secs), bounded)
        .await
        .map_err(|_| {
            tracing::warn!("Voice transcription exceeded {}s", settings.timeout_secs);
            VoiceError::Timeout
        })??;
    Ok((format_block(attachment, &transcript), usage))
}

pub(crate) fn format_block(attachment: &VoiceAttachment, transcript: &str) -> String {
    let max = config().voice.max_transcript_chars;
    let head = match attachment.author.as_deref() {
        Some(author) => format!(
            "[{} {}с, {author}]: ",
            attachment.label, attachment.duration_secs
        ),
        None => format!("[{} {}с]: ", attachment.label, attachment.duration_secs),
    };
    format!("{head}{}", truncate_chars(transcript, max))
}

/// The transcript block plus whether it quotes a replied message rather than
/// the sender's own attachment.
pub(crate) struct VoiceBlock {
    pub(crate) text: String,
    pub(crate) from_reply: bool,
}

/// Cheap predicate for `handle_message`'s early return.
pub(crate) fn pending(message: &Message) -> bool {
    enabled() && candidate(message).is_some()
}

/// Transcribes the message's own attachment, or the one it replies to, when
/// the turn may reach the model. Ignored senders, non-owner private chats and
/// disabled groups never pay for a download or an STT call.
pub(crate) async fn transcription(
    bot: &Bot,
    message: &Message,
    cfg: &Settings,
    owner: bool,
    sender_ignored: bool,
    reply_ignored: bool,
) -> Option<VoiceBlock> {
    let candidate = candidate(message)?;
    if !enabled()
        || sender_ignored
        || (!owner && message.chat.is_private())
        || (candidate.from_reply && reply_ignored)
    {
        return None;
    }
    if !message.chat.is_private()
        && !group_enabled(&cfg.database, message.chat.id)
            .await
            .is_ok_and(|enabled| enabled)
    {
        return None;
    }
    let chat = cfg.chat.as_ref()?;
    let (text, usage) =
        match transcribe(bot, &candidate.attachment, &chat.api_key, &chat.base_url).await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!("Voice transcription failed: {error}");
                (UNRECOGNISED.to_owned(), None)
            }
        };
    if let Some(sender) = message.from.as_ref() {
        UsageSite {
            chat_id: message.chat.id,
            user_id: sender.id,
            author_name: display_author_name(sender),
        }
        .record(&cfg.database, &config().voice.model, KIND_VOICE_STT, usage)
        .await;
    }
    Some(VoiceBlock {
        text,
        from_reply: candidate.from_reply,
    })
}

/// A transcribed turn: the text to append plus whether it quotes someone
/// else, which decides if injection markers may be blamed on this sender.
pub(crate) struct VoiceTurn {
    pub(crate) text: Option<String>,
    pub(crate) quoted: bool,
}

/// Prepares the turn text: the sender's own transcript, or the quoted one,
/// appended to whatever they typed.
pub(crate) async fn turn(
    bot: &Bot,
    message: &Message,
    cfg: &Settings,
    typed: &str,
    owner: bool,
    sender_ignored: bool,
    reply_ignored: bool,
) -> VoiceTurn {
    let Some(block) = transcription(bot, message, cfg, owner, sender_ignored, reply_ignored).await
    else {
        return VoiceTurn {
            text: None,
            quoted: false,
        };
    };
    let quoted = block.from_reply;
    VoiceTurn {
        text: append_transcript(typed, Some(block)),
        quoted,
    }
}

/// Original typed text plus the transcript, typed text first.
pub(crate) fn append_transcript(text: &str, block: Option<VoiceBlock>) -> Option<String> {
    let block = block?;
    Some(if text.trim().is_empty() {
        block.text
    } else {
        format!("{text}\n{}", block.text)
    })
}

fn truncate_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((index, _)) => text.get(..index).unwrap_or(text),
        None => text,
    }
}

enum DownloadError {
    Transient,
    Permanent,
}

async fn download(bot: &Bot, attachment: &VoiceAttachment) -> Result<Vec<u8>, VoiceError> {
    for attempt in 0..2u8 {
        match download_once(bot, attachment).await {
            Ok(bytes) => return Ok(bytes),
            Err(DownloadError::Permanent) => break,
            Err(DownloadError::Transient) if attempt == 0 => {
                time::sleep(Duration::from_millis(150)).await;
            }
            Err(DownloadError::Transient) => break,
        }
    }
    tracing::warn!("Voice download failed");
    Err(VoiceError::Download)
}

async fn download_once(bot: &Bot, attachment: &VoiceAttachment) -> Result<Vec<u8>, DownloadError> {
    let file = bot
        .get_file(attachment.file_id.clone())
        .await
        .map_err(|err| match &err {
            teloxide::RequestError::Network(_) | teloxide::RequestError::RetryAfter(_) => {
                DownloadError::Transient
            }
            teloxide::RequestError::Api(teloxide::ApiError::Unknown(msg))
                if msg.contains("500") || msg.contains("502") || msg.contains("503") =>
            {
                DownloadError::Transient
            }
            teloxide::RequestError::Api(_)
            | teloxide::RequestError::MigrateToChatId(_)
            | teloxide::RequestError::InvalidJson { .. }
            | teloxide::RequestError::Io(_) => DownloadError::Permanent,
        })?;
    let url = bot
        .api_url()
        .join(&format!("file/bot{}/{}", bot.token(), file.path))
        .map_err(|_| DownloadError::Permanent)?;
    let mut response = VOICE_CLIENT
        .get(url)
        .send()
        .await
        .map_err(|_| DownloadError::Transient)?;
    let status = response.status();
    if !status.is_success() {
        return if status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429 {
            Err(DownloadError::Transient)
        } else {
            Err(DownloadError::Permanent)
        };
    }
    let max_bytes = config().voice.max_bytes;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err(DownloadError::Permanent);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| DownloadError::Transient)?
    {
        if bytes.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(DownloadError::Permanent);
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(DownloadError::Permanent);
    }
    Ok(bytes)
}

pub(crate) async fn transcribe_bytes(
    bytes: &[u8],
    format: &str,
    api_key: &str,
    base_url: &Url,
) -> Result<(String, Option<Usage>), VoiceError> {
    let mut endpoint = base_url.clone();
    endpoint.set_path(&format!(
        "{}/chat/completions",
        endpoint.path().trim_end_matches('/')
    ));
    let settings = &config().voice;
    let mut body = json!({
        "model": settings.model,
        "messages": [
            {"role": "system", "content": TRANSCRIBE_PROMPT},
            {"role": "user", "content": [{
                "type": "input_audio",
                "input_audio": {"data": "", "format": format}
            }]}
        ],
        "max_tokens": u32::try_from(settings.max_transcript_chars).unwrap_or(8000),
        "temperature": 0
    });
    // Assigning the encoded audio in place moves one allocation instead of
    // serialising the multi-megabyte string through `json!`.
    match body.pointer_mut("/messages/1/content/0/input_audio/data") {
        Some(slot) => *slot = Value::String(base64_encode(bytes)),
        None => return Err(VoiceError::Transcription),
    }
    let mut response = VOICE_CLIENT
        .post(endpoint)
        .bearer_auth(api_key)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|_| VoiceError::Transcription)?;
    if !response.status().is_success() {
        tracing::warn!("Voice transcription HTTP {}", response.status().as_u16());
        return Err(VoiceError::Transcription);
    }
    let max_response_bytes = u64::try_from(config().llm.max_response_bytes).unwrap_or(u64::MAX);
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes)
    {
        return Err(VoiceError::Transcription);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| VoiceError::Transcription)?
    {
        if bytes.len() as u64 + chunk.len() as u64 > max_response_bytes {
            return Err(VoiceError::Transcription);
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| VoiceError::Transcription)?;
    let usage = Usage::from_response(&value);
    let text = transcript_from_response(&value).ok_or(VoiceError::Transcription)?;
    let text = text.trim();
    if text.is_empty() {
        return Err(VoiceError::Transcription);
    }
    Ok((text.to_owned(), usage))
}

/// Accepts a plain string or the content-part array multimodal gateways return.
pub(crate) fn transcript_from_response(value: &Value) -> Option<String> {
    let content = value.pointer("/choices/0/message/content")?;
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text: String = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            (!text.trim().is_empty()).then_some(text)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => None,
    }
}
