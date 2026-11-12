use crate::*;

/// One inbound chat message plus who sent it.
pub(crate) struct ChatTurn<'a> {
    pub(crate) chat_id: ChatId,
    pub(crate) user_id: UserId,
    pub(crate) sender_name: &'a str,
    pub(crate) username: Option<&'a str>,
    pub(crate) message_id: i32,
    pub(crate) reply_user_id: Option<UserId>,
    pub(crate) owner: bool,
    pub(crate) prompt: &'a str,
    pub(crate) image: Option<&'a ChatImage>,
    /// Tier resolved by the `auto` judge before the model slot was taken;
    /// `None` for a fixed mode, vision turns, blank prompts, and `auto` turns
    /// the judge skipped because it had no classifier or a pinned ceiling.
    pub(crate) effort: Option<Effort>,
    pub(crate) followup: bool,
    pub(crate) sticker_mode: bool,
    pub(crate) reply_context: Option<&'a str>,
    pub(crate) bot: Option<&'a Bot>,
}

#[derive(Default)]
pub(crate) struct ChatReply {
    pub(crate) text: String,
    pub(crate) sticker: Option<FileId>,
    pub(crate) decision: Option<crate::self_improve::PendingDecision>,
    pub(crate) proposed_feature: bool,
}
