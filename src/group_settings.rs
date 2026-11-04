use crate::*;

/// `/maksut status|on|off`. The command is group-scoped and owner-only; a
/// private-chat owner gets an explanation instead of a silent drop.
pub(crate) async fn handle_command(bot: &Bot, message: &Message, cfg: &Settings, command: &str) {
    if !message.chat.is_group() && !message.chat.is_supergroup() {
        if message.chat.is_private()
            && message
                .from
                .as_ref()
                .is_some_and(|user| user.id == cfg.owner_id)
        {
            let _ = bot.send_message(message.chat.id, "Команды /maksut status|on|off работают только в группе; в личном чате ссылки доступны только владельцу бота.").disable_notification(true).await;
        }
        return;
    }
    if !message
        .from
        .as_ref()
        .is_some_and(|user| user.id == cfg.owner_id)
    {
        return;
    }
    let result = match command {
        "status" => group_enabled(&cfg.database, message.chat.id).await,
        "on" => set_group_enabled(&cfg.database, message.chat.id, true)
            .await
            .map(|()| true),
        "off" => set_group_enabled(&cfg.database, message.chat.id, false)
            .await
            .map(|()| false),
        _ => {
            let _ = bot
                .send_message(
                    message.chat.id,
                    "Используйте /maksut status, /maksut on или /maksut off.",
                )
                .disable_notification(true)
                .await;
            return;
        }
    };
    let reply = match result {
        Ok(true) => "Максут включён в этой группе.",
        Ok(false) => "Максут выключен в этой группе.",
        Err(_) => "Не удалось прочитать или сохранить настройки группы.",
    };
    tracing::info!(command, ok = result.is_ok(), "group command handled");
    let _ = bot
        .send_message(message.chat.id, reply)
        .disable_notification(true)
        .await;
}

pub(crate) fn maksut_command<'a>(text: &'a str, username: &str) -> Option<&'a str> {
    let mut parts = text.split_whitespace();
    let name = parts.next()?;
    let addressed = name == "/maksut"
        || name
            .strip_prefix("/maksut@")
            .is_some_and(|target| !target.is_empty() && target.eq_ignore_ascii_case(username));
    addressed.then(|| parts.next().unwrap_or(""))
}
