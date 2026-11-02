use crate::*;

/// Marks the message a search pass is working on, so the sender sees the
/// request was picked up before the answer arrives.
pub(crate) async fn mark_search(bot: &Bot, chat_id: ChatId, message_id: MessageId) {
    // Best effort: a group can disable reactions and the triggering message can
    // be gone, neither of which may fail the answer.
    let _ = bot
        .set_message_reaction(chat_id, message_id)
        .reaction(vec![ReactionType::Emoji {
            emoji: "👀".to_owned(),
        }])
        .await;
}
