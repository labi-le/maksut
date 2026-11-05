use crate::*;

async fn record_assistant_reply(
    pool: &SqlitePool,
    chat_id: ChatId,
    user_id: UserId,
    text: &str,
    message_id: i32,
    flags: i64,
) {
    let _ = crate::memory::record_message(
        pool,
        RecordMessage::new(chat_id, user_id.0 as i64, "максут", "assistant", text)
            .with_message_id(Some(message_id))
            .with_flags(flags),
    )
    .await;
}

pub(crate) async fn deliver_chat_reply(
    bot: &Bot,
    message: &Message,
    cfg: &Settings,
    chat: &ChatConfig,
    followup: bool,
    reply: ChatReply,
) {
    if followup && is_pass_reply(&reply.text) && reply.sticker.is_none() && reply.decision.is_none()
    {
        tracing::debug!("followup PASS, nothing delivered");
        return;
    }
    let mut delivered = false;
    if let Some(id) = &reply.sticker {
        for attempt in 0..2u8 {
            match bot
                .send_sticker(message.chat.id, InputFile::file_id(id.clone()))
                .reply_parameters(ReplyParameters::new(message.id).allow_sending_without_reply())
                .disable_notification(true)
                .await
            {
                Ok(s) => {
                    delivered = true;
                    let uid = message.from.as_ref().map_or(UserId(0), |u| u.id);
                    record_assistant_reply(
                        &cfg.database,
                        message.chat.id,
                        uid,
                        "[стикер]",
                        s.id.0,
                        crate::memory::FLAG_JUNK,
                    )
                    .await;
                    break;
                }
                Err(err) => {
                    if attempt == 0 {
                        if let Some(delay) = crate::rich_message::retryable_telegram_error(&err) {
                            time::sleep(delay).await;
                            continue;
                        }
                    }
                    break;
                }
            }
        }
    }
    let markup = reply
        .decision
        .as_ref()
        .and_then(crate::self_improve::decision_markup);
    let p = message.text().or_else(|| message.caption()).unwrap_or("");
    let skip_text = is_pass_reply(&reply.text);
    let delivered_text = crate::rich_message::deliver_reply_text(
        bot,
        message.chat.id,
        message.id,
        p,
        skip_text,
        markup,
        &reply,
    )
    .await;
    let delivered_text_present = delivered_text.is_some();
    delivered |= delivered_text.is_some();
    if let Some((id, out)) = delivered_text {
        let rec = match out.as_deref() {
            Some(published) => published,
            None if reply.text.trim().is_empty() => "жми кнопку",
            None => &reply.text,
        };
        let uid = message.from.as_ref().map_or(UserId(0), |u| u.id);
        record_assistant_reply(&cfg.database, message.chat.id, uid, rec, id, 0).await;
    }
    if is_stop_followup(p) && (followup || p.split_whitespace().count() <= 4) {
        chat.clear_followup(message.chat.id).await;
    } else if delivered {
        chat.note_reply(message.chat.id).await;
    }
    chat_reply_delivered(&reply, delivered, delivered_text_present);
}

pub(crate) async fn send_turn_error_reply(
    bot: &Bot,
    message: &Message,
    followup: bool,
    reason: &str,
) {
    if !message.chat.is_private() && followup {
        return;
    }
    let msg = match (message.chat.is_private(), reason.strip_prefix("http_")) {
        (true, _) => "Не получилось ответить, попробуй ещё раз.".to_owned(),
        (false, Some(code)) => format!("лпмка вернула ошибку {code}, повтори ещё раз"),
        (false, None) if reason == "timeout" => {
            "лпмка отвалилась по таймауту, повтори ещё раз".to_owned()
        }
        (false, None) => "лпмка временно отвалилась, повтори ещё раз".to_owned(),
    };
    let _ =
        crate::rich_message::send_chat_chunk(bot, message.chat.id, message.id, &msg, true, None)
            .await;
}
