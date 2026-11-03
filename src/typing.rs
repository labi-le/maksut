use crate::prelude::*;
use teloxide::types::ChatAction;

pub(crate) struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Refreshes the typing action until dropped; the interval is
/// `chat.typing_keepalive_secs`, so a long tool loop still looks alive.
pub(crate) fn spawn_typing_keepalive(bot: Bot, chat_id: ChatId) -> AbortOnDrop {
    AbortOnDrop(tokio::spawn(async move {
        let _ = bot.send_chat_action(chat_id, ChatAction::Typing).await;
        let interval = Duration::from_secs(config().chat.typing_keepalive_secs);
        while {
            time::sleep(interval).await;
            bot.send_chat_action(chat_id, ChatAction::Typing)
                .await
                .is_ok()
        } {}
    }))
}
