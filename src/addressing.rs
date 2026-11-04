pub(crate) const BOT_NAME_FORMS: &[&str] = &[
    "максут",
    "максута",
    "максуту",
    "максутом",
    "максуте",
    "максутка",
    "максутки",
    "максутке",
    "максутку",
    "максуткой",
    "максуток",
    "максутик",
    "максутика",
    "максутику",
    "максутиком",
    "максутике",
    "максутики",
    "maksut",
];

/// How the message relates to the bot, as far as addressing goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AddressKind {
    /// Nobody addressed the bot; only a router may wake it up.
    None,
    /// One of the bot name forms appears and some content remains.
    Named,
    /// Private chat, a reply to the bot, or an explicit `@username` mention.
    Explicit,
}

/// Media uploads posted by the bot always end with an attribution line
/// (`От <name>`) and a trailing `Ссылка` line (`src/caption.rs`). A reply to
/// such a post is a comment to the human who shared the link, not a reply to
/// the bot unless the message also mentions the bot by name or `@username`.
pub(crate) fn is_bot_media_post(reply: &teloxide::types::Message) -> bool {
    let Some(caption) = reply.caption().or_else(|| {
        crate::chat_media::message_has_attachment(reply)
            .then(|| reply.text())
            .flatten()
    }) else {
        return false;
    };
    let mut lines = caption.lines().rev().map(str::trim);
    lines.next() == Some("Ссылка")
        && lines
            .next()
            .is_some_and(|line| line.starts_with("От ") && line.len() > "От ".len())
}

pub(crate) fn reply_to_bot(message: &teloxide::types::Message, bot_username: &str) -> bool {
    message.reply_to_message().is_some_and(|reply| {
        !is_bot_media_post(reply)
            && reply.from.as_ref().is_some_and(|user| {
                user.is_bot
                    && user
                        .username
                        .as_deref()
                        .is_some_and(|name| name.eq_ignore_ascii_case(bot_username))
            })
    })
}

/// True when a word is exactly `@<bot_username>` and some other content
/// remains; `@bot_extra`, `mail@host` domains, and a bare mention do not count.
pub(crate) fn explicit_mention(text: &str, bot_username: &str) -> bool {
    let bot_name = bot_username.trim().to_lowercase();
    if bot_name.is_empty() {
        return false;
    }
    let mention = format!("@{bot_name}");
    let mut found = false;
    let mut rest = String::new();
    for word in text.split_whitespace() {
        let core = word.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '@' && ch != '_');
        if core.eq_ignore_ascii_case(&mention) {
            found = true;
        } else {
            rest.push_str(word);
            rest.push(' ');
        }
    }
    found && !trim_name_edges(&rest).is_empty()
}

pub(crate) fn address_kind(
    message: &teloxide::types::Message,
    text: &str,
    bot_username: &str,
) -> AddressKind {
    let private = message.chat.is_private() || reply_to_bot(message, bot_username);
    let kind = if private || explicit_mention(text, bot_username) {
        AddressKind::Explicit
    } else if named_content_left(text, bot_username) {
        AddressKind::Named
    } else {
        AddressKind::None
    };
    tracing::trace!(kind = ?kind, text_len = text.chars().count(), "address kind");
    kind
}

/// True when at least one bot name form is present and stripping it leaves
/// real content. A message that is only `"максут"` stays unaddressed, exactly as
/// `chat_prompt` treats it.
pub(crate) fn named_content_left(text: &str, bot_username: &str) -> bool {
    let bot_name = (!bot_username.is_empty()).then(|| bot_username.to_lowercase());
    let mut cleaned = text.to_owned();
    let mut named = false;
    for name in BOT_NAME_FORMS.iter().copied().chain(bot_name.as_deref()) {
        let (rest, found) = strip_name(&cleaned, name);
        if found {
            named = true;
            cleaned = rest;
        }
    }
    named && !trim_name_edges(&cleaned).is_empty()
}

/// The same edge punctuation `chat_prompt` removes before deciding that a
/// prompt is empty.
pub(crate) fn trim_name_edges(text: &str) -> &str {
    text.trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, ',' | '.' | ':' | ';' | '!' | '?' | '-')
    })
}

pub(crate) fn strip_name(text: &str, name: &str) -> (String, bool) {
    let words: Vec<&str> = text.split_whitespace().collect();
    let matches: Vec<usize> = words
        .iter()
        .enumerate()
        .filter_map(|(index, word)| {
            let core = word.trim_matches(|ch: char| !ch.is_alphanumeric());
            (!core.is_empty() && core.to_lowercase() == name).then_some(index)
        })
        .collect();
    if matches.is_empty() {
        return (text.to_owned(), false);
    }
    let last = words.len().saturating_sub(1);
    let edge_hit = matches.contains(&0) || matches.contains(&last);
    if !edge_hit {
        return (text.to_owned(), true);
    }
    let kept: Vec<&str> = words
        .into_iter()
        .enumerate()
        .filter_map(|(index, word)| {
            let strip = (index == 0 || index == last) && matches.contains(&index);
            (!strip).then_some(word)
        })
        .collect();
    (kept.join(" "), true)
}
