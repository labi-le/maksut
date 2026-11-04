//! Reply text delivery: Bot API 10.1 `sendRichMessage` first, then the
//! `telegra.ph` teaser and `SendMessage` chunking fallback.
//!
//! teloxide 0.17 ships typed methods only up to Bot API 9.1, so the rich method
//! is sent as a raw JSON request through the same public `JsonRequest`
//! primitive teloxide uses for its typed methods. Rich delivery is opt-in
//! through `chat.rich_messages`; any API failure returns `None`, letting the
//! caller fall back to `SendMessage` and the plain-text split.

use crate::*;

/// Payload of Bot API `sendRichMessage`.
#[derive(serde::Serialize)]
struct SendRichMessage {
    chat_id: ChatId,
    rich_message: InputRichMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_parameters: Option<ReplyParameters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<teloxide::types::InlineKeyboardMarkup>,
    disable_notification: bool,
}

/// HTML flavour of `InputRichMessage`; rich messages accept exactly one of
/// `html`, `markdown`, or `blocks`, and replies are already rendered to
/// Telegram HTML.
#[derive(serde::Serialize)]
struct InputRichMessage {
    html: String,
}

impl teloxide::requests::Payload for SendRichMessage {
    type Output = Message;
    const NAME: &'static str = "sendRichMessage";
}

/// `true` when `text` takes the rich path: enabled by config, non-empty, within
/// `chat.rich_max_chars`, and not an explicit article request (those keep going
/// to `telegra.ph`).
pub(crate) fn prefer_rich(prompt: &str, text: &str) -> bool {
    let chat = &crate::config::config().chat;
    chat.rich_messages
        && !text.trim().is_empty()
        && text.chars().count() <= chat.rich_max_chars
        && !crate::telegraph::explicit_article_request(prompt)
}

/// `true` when a Telegram Bot API error is a transient transport or rate-limit
/// failure worth one short retry on a fresh connection.
pub(crate) fn retryable_telegram_error(error: &teloxide::RequestError) -> Option<Duration> {
    match error {
        teloxide::RequestError::Network(_) => Some(Duration::from_millis(200)),
        teloxide::RequestError::RetryAfter(wait) => {
            Some(wait.duration().min(Duration::from_secs(5)))
        }
        teloxide::RequestError::Api(teloxide::ApiError::Unknown(ref msg))
            if msg.contains("500")
                || msg.contains("502")
                || msg.contains("503")
                || msg.contains("504") =>
        {
            Some(Duration::from_millis(200))
        }
        teloxide::RequestError::Api(_)
        | teloxide::RequestError::MigrateToChatId(_)
        | teloxide::RequestError::InvalidJson { .. }
        | teloxide::RequestError::Io(_) => None,
    }
}

/// Sends `text` as one rich message whose HTML comes from
/// `render_telegram_html`, or from an escaped copy of `text` when it carries no
/// Telegram markup. Returns the message id, or `None` on any API error so the
/// caller can fall back to `SendMessage`.
pub(crate) async fn send_rich_reply(
    bot: &Bot,
    chat_id: ChatId,
    reply_to: MessageId,
    text: &str,
    markup: Option<teloxide::types::InlineKeyboardMarkup>,
) -> Option<i32> {
    let html = crate::chat_text::render_telegram_html(text)
        .unwrap_or_else(|| crate::chat_text::escape_html(text));
    for attempt in 0..2u8 {
        let payload = SendRichMessage {
            chat_id,
            rich_message: InputRichMessage { html: html.clone() },
            reply_parameters: Some(ReplyParameters::new(reply_to).allow_sending_without_reply()),
            reply_markup: markup.clone(),
            disable_notification: true,
        };
        match teloxide::requests::JsonRequest::new(bot.clone(), payload).await {
            Ok(message) => return Some(message.id.0),
            Err(error) => {
                if attempt == 0 {
                    if let Some(delay) = retryable_telegram_error(&error) {
                        tracing::warn!(
                            timeout = matches!(&error, teloxide::RequestError::Network(err) if err.is_timeout()),
                            connect = matches!(&error, teloxide::RequestError::Network(err) if err.is_connect()),
                            "sendRichMessage transient error, retrying"
                        );
                        time::sleep(delay).await;
                        continue;
                    }
                }
                tracing::warn!(
                    timeout =
                        matches!(&error, teloxide::RequestError::Network(err) if err.is_timeout()),
                    connect =
                        matches!(&error, teloxide::RequestError::Network(err) if err.is_connect()),
                    "sendRichMessage failed, falling back to sendMessage"
                );
                return None;
            }
        }
    }
    None
}

/// Sends the reply text rich-first, then falls back to `telegra.ph` publishing
/// and line-aware chunking within `chat.reply_max_chars`. Returns the first
/// message id plus the `telegra.ph` teaser when publishing happened, so the
/// caller records exactly what the chat saw.
pub(crate) async fn deliver_reply_text(
    bot: &Bot,
    chat_id: ChatId,
    reply_to: MessageId,
    prompt: &str,
    skip_text: bool,
    markup: Option<teloxide::types::InlineKeyboardMarkup>,
    reply: &ChatReply,
) -> Option<(i32, Option<String>)> {
    if !skip_text && prefer_rich(prompt, &reply.text) {
        if let Some(id) = send_rich_reply(bot, chat_id, reply_to, &reply.text, markup.clone()).await
        {
            return Some((id, None));
        }
    }
    let mut out = None;
    let mut chunks = if skip_text {
        Vec::new()
    } else {
        // Feature replies never leave for `telegra.ph`; they stay in the chat.
        if !reply.proposed_feature {
            out = crate::telegraph::maybe_publish_reply(prompt, &reply.text).await;
        }
        split_message(out.as_deref().unwrap_or(&reply.text))
    };
    if chunks.is_empty() && markup.is_some() {
        chunks.push("жми кнопку".to_owned());
    }
    let mut first_id = None;
    for (i, chunk) in chunks.iter().enumerate() {
        let m = (i == 0).then(|| markup.clone()).flatten();
        if let Some(id) = send_chat_chunk(bot, chat_id, reply_to, chunk, i == 0, m).await {
            first_id.get_or_insert(id);
        }
    }
    first_id.map(|id| (id, out))
}

/// Sends one chunk, preferring Telegram HTML and retrying without `parse_mode`
/// when Telegram rejects the entities.
pub(crate) async fn send_chat_chunk(
    bot: &Bot,
    chat_id: ChatId,
    reply_to: MessageId,
    chunk: &str,
    first: bool,
    markup: Option<teloxide::types::InlineKeyboardMarkup>,
) -> Option<i32> {
    let apply = |mut req: teloxide::requests::JsonRequest<teloxide::payloads::SendMessage>| {
        if first {
            req =
                req.reply_parameters(ReplyParameters::new(reply_to).allow_sending_without_reply());
            if let Some(m) = markup.clone() {
                req = req.reply_markup(m);
            }
        }
        req.disable_notification(true)
    };
    if let Some(html) = render_telegram_html(chunk) {
        for attempt in 0..2u8 {
            match apply(
                bot.send_message(chat_id, html.clone())
                    .parse_mode(ParseMode::Html),
            )
            .await
            {
                Ok(s) => return Some(s.id.0),
                Err(teloxide::RequestError::Api(teloxide::ApiError::CantParseEntities(_))) => break,
                Err(teloxide::RequestError::Api(teloxide::ApiError::Unknown(ref m)))
                    if m.contains("can't parse entities") =>
                {
                    break;
                }
                Err(err) => {
                    if attempt == 0 {
                        if let Some(delay) = retryable_telegram_error(&err) {
                            time::sleep(delay).await;
                            continue;
                        }
                    }
                    tracing::warn!(
                        timeout =
                            matches!(&err, teloxide::RequestError::Network(e) if e.is_timeout()),
                        connect =
                            matches!(&err, teloxide::RequestError::Network(e) if e.is_connect()),
                        "sendMessage HTML failed"
                    );
                    return None;
                }
            }
        }
    }
    for attempt in 0..2u8 {
        match apply(bot.send_message(chat_id, chunk.to_owned())).await {
            Ok(s) => return Some(s.id.0),
            Err(err) => {
                if attempt == 0 {
                    if let Some(delay) = retryable_telegram_error(&err) {
                        time::sleep(delay).await;
                        continue;
                    }
                }
                tracing::warn!(
                    timeout = matches!(&err, teloxide::RequestError::Network(e) if e.is_timeout()),
                    connect = matches!(&err, teloxide::RequestError::Network(e) if e.is_connect()),
                    "sendMessage plain failed"
                );
                return None;
            }
        }
    }
    None
}
