use crate::garage::GarageClient;
use crate::media_store::{lookup_binding, refresh_binding_file_id, StoredChatMedia};
use crate::*;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SendMediaOutcome {
    CachedFileId,
    GarageFallback,
}

fn extension_for_kind(kind: &str) -> &'static str {
    match kind {
        "photo" => "jpg",
        "video" | "animation" | "video_note" => "mp4",
        "audio" => "mp3",
        "voice" => "ogg",
        "sticker" => "webp",
        _ => "bin",
    }
}

pub(crate) fn parse_send_args(args: &Value) -> Result<(Option<i64>, Option<String>), String> {
    let message_id = args
        .get("message_id")
        .and_then(Value::as_i64)
        .filter(|id| *id > 0);
    let storage_key = args
        .get("storage_key")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    if let Some(key) = &storage_key {
        if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("storage_key должен быть 64-символьным sha256".into());
        }
    }
    if message_id.is_none() && storage_key.is_none() {
        return Err("укажи message_id или storage_key из chat_search".into());
    }
    Ok((message_id, storage_key))
}

pub(crate) async fn run_tool(args: &Value, ctx: &ToolContext<'_>) -> String {
    let (message_id, storage_key) = match parse_send_args(args) {
        Ok(parsed) => parsed,
        Err(reason) => return reason,
    };
    let Some(bot) = ctx.bot else {
        return "отправка медиа недоступна в этом контексте".into();
    };
    let stored =
        match lookup_binding(ctx.pool, ctx.chat_id.0, message_id, storage_key.as_deref()).await {
            Ok(Some(row)) => row,
            Ok(None) => return "медиа для этого сообщения не найдено в архиве чата".into(),
            Err(_) => return "ошибка чтения архива медиа".into(),
        };
    let garage = GarageClient::from_config();
    match deliver_stored_media(bot, ctx.pool, ctx.chat_id, &stored, garage.as_ref()).await {
        Ok(_) => format!(
            "отправлено {} из архива чата (message_id={}, storage_key={})",
            stored.kind, stored.message_id, stored.storage_key
        ),
        Err(reason) => reason,
    }
}

pub(crate) async fn deliver_stored_media(
    bot: &Bot,
    pool: &SqlitePool,
    chat_id: ChatId,
    stored: &StoredChatMedia,
    garage: Option<&GarageClient>,
) -> Result<SendMediaOutcome, String> {
    if !stored.file_id.trim().is_empty() {
        let input = InputFile::file_id(FileId(stored.file_id.clone()));
        if send_by_kind(bot, chat_id, &stored.kind, input)
            .await
            .is_ok()
        {
            return Ok(SendMediaOutcome::CachedFileId);
        }
    }
    let Some(client) = garage else {
        return Err("file_id устарел, а хранилище Garage не настроено".into());
    };
    let bytes = client
        .get(&stored.storage_key)
        .await
        .map_err(|_| "не удалось загрузить блоб из Garage".to_string())?;
    let prefix = stored.storage_key.get(..12).unwrap_or(&stored.storage_key);
    let name = format!("media-{prefix}.{}", extension_for_kind(&stored.kind));
    let input = InputFile::memory(bytes).file_name(name);
    let sent = send_by_kind(bot, chat_id, &stored.kind, input)
        .await
        .map_err(|_| "не удалось отправить медиа в Telegram".to_string())?;
    if let Some(fresh_id) = extract_sent_file_id(&sent) {
        if fresh_id != stored.file_id {
            let _ =
                refresh_binding_file_id(pool, stored.chat_id, stored.message_id, &fresh_id).await;
        }
    }
    Ok(SendMediaOutcome::GarageFallback)
}

async fn send_by_kind(
    bot: &Bot,
    chat_id: ChatId,
    kind: &str,
    input: InputFile,
) -> Result<Message, teloxide::RequestError> {
    match kind {
        "photo" => {
            bot.send_photo(chat_id, input)
                .disable_notification(true)
                .await
        }
        "video" => {
            bot.send_video(chat_id, input)
                .supports_streaming(true)
                .disable_notification(true)
                .await
        }
        "animation" => {
            bot.send_animation(chat_id, input)
                .disable_notification(true)
                .await
        }
        "audio" => {
            bot.send_audio(chat_id, input)
                .disable_notification(true)
                .await
        }
        "voice" => {
            bot.send_voice(chat_id, input)
                .disable_notification(true)
                .await
        }
        "video_note" => {
            bot.send_video_note(chat_id, input)
                .disable_notification(true)
                .await
        }
        "sticker" => {
            bot.send_sticker(chat_id, input)
                .disable_notification(true)
                .await
        }
        _ => {
            bot.send_document(chat_id, input)
                .disable_notification(true)
                .await
        }
    }
}

fn extract_sent_file_id(message: &Message) -> Option<String> {
    if let Some(sizes) = message.photo() {
        return sizes
            .iter()
            .max_by_key(|size| u64::from(size.width) * u64::from(size.height))
            .map(|size| size.file.id.0.clone());
    }
    if let Some(video) = message.video() {
        return Some(video.file.id.0.clone());
    }
    if let Some(animation) = message.animation() {
        return Some(animation.file.id.0.clone());
    }
    if let Some(audio) = message.audio() {
        return Some(audio.file.id.0.clone());
    }
    if let Some(voice) = message.voice() {
        return Some(voice.file.id.0.clone());
    }
    if let Some(note) = message.video_note() {
        return Some(note.file.id.0.clone());
    }
    if let Some(sticker) = message.sticker() {
        return Some(sticker.file.id.0.clone());
    }
    message.document().map(|doc| doc.file.id.0.clone())
}
