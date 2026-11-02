use crate::*;

/// Marks the message a lookup is working on, so the sender sees the request was
/// picked up before the answer arrives.
pub(crate) fn mark_lookup(bot: &Bot, chat_id: ChatId, message_id: MessageId) {
    // Best effort and non-blocking: fire-and-forget so a slow or rate-limited
    // Telegram reaction call never delays the search or code lookup itself.
    let bot = bot.clone();
    tokio::spawn(async move {
        let _ = bot
            .set_message_reaction(chat_id, message_id)
            .reaction(vec![ReactionType::Emoji {
                emoji: "👀".to_owned(),
            }])
            .await;
    });
}
