use crate::*;

async fn record_assistant_reply(pool: &SqlitePool, chat_id: ChatId, text: &str, message_id: i32) {
    let _ = crate::memory::record_message(
        pool,
        RecordMessage::new(chat_id, 0, "максут", "assistant", text)
            .with_message_id(Some(message_id)),
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
        if let Ok(s) = bot
            .send_sticker(message.chat.id, InputFile::file_id(id.clone()))
            .reply_parameters(ReplyParameters::new(message.id).allow_sending_without_reply())
            .disable_notification(true)
            .await
        {
            delivered = true;
            record_assistant_reply(&cfg.database, message.chat.id, "[стикер]", s.id.0).await;
        }
    }
    let markup = reply
        .decision
        .as_ref()
        .and_then(crate::self_improve::decision_markup);
    let p = message.text().or_else(|| message.caption()).unwrap_or("");
    let skip_text = followup && is_pass_reply(&reply.text);
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
        record_assistant_reply(&cfg.database, message.chat.id, rec, id).await;
    }
    if is_stop_followup(p) {
        chat.clear_followup(message.chat.id).await;
    } else if delivered {
        chat.note_reply(message.chat.id).await;
    }
    chat_reply_delivered(&reply, delivered, delivered_text_present);
}
