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

const BOT_VOCATIVE_FORMS: &[&str] = &["максут", "максутка", "максутик", "maksut"];

const THIRD_PERSON_LEAD_WORDS: &[&str] = &[
    "это",
    "вот",
    "опять",
    "снова",
    "просто",
    "же",
    "уже",
    "там",
    "тут",
    "тоже",
    "даже",
    "как",
    "зато",
    "хотя",
    "ведь",
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
    } else if named_content_left(text, bot_username)
        && (message.reply_to_message().is_none() || vocative_bot_address(text, bot_username))
    {
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

/// When a group message replies to another human, a casual third-person mention
/// (`"это Максут"`, `"опять максут акции обрушил"`, `"у максута спроси"`) is
/// directed at the replied-to human, not the bot; only an edge vocative
/// (`"максут, ..."`, `"... максут?"`) that is not a bare `"это максут"` clause
/// addresses the bot directly.
pub(crate) fn vocative_bot_address(text: &str, bot_username: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .filter_map(|w| {
            let core = w.trim_matches(|ch: char| !ch.is_alphanumeric());
            (!core.is_empty()).then(|| core.to_lowercase())
        })
        .collect();
    if words.len() < 2 {
        return false;
    }
    let bot_name = (!bot_username.is_empty()).then(|| bot_username.to_lowercase());
    let is_vocative =
        |w: &str| BOT_VOCATIVE_FORMS.contains(&w) || bot_name.as_deref().is_some_and(|b| w == b);
    let first_voc = words.first().is_some_and(|w| is_vocative(w));
    let last_voc = words.last().is_some_and(|w| is_vocative(w));
    if !first_voc && !last_voc {
        return false;
    }
    if !first_voc
        && words
            .first()
            .is_some_and(|w| THIRD_PERSON_LEAD_WORDS.contains(&w.as_str()))
    {
        return false;
    }
    true
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

pub(crate) const OTHER_PARTICIPANT_VOCATIVES: &[&str] = &[
    "денис",
    "дениска",
    "вадик",
    "даня",
    "дань",
    "ваня",
    "вань",
    "ваньок",
    "ванёк",
    "влад",
    "владулик",
    "владик",
    "влудик",
    "кирилл",
    "кирюх",
    "андрей",
    "андрюх",
    "андрюха",
    "анрюх",
    "олег",
    "олежа",
    "иван",
    "лейла",
    "павел",
    "паша",
    "евгений",
    "женя",
    "виталий",
    "виталик",
    "марина",
    "даниил",
    "алиса",
    "алису",
    "алисе",
    "алисой",
];

const THIRD_PERSON_LEAD_PRONOUNS: &[&str] = &["он", "она", "оно", "они", "ему", "ей", "им"];

pub(crate) fn addresses_other_participant(text: &str, has_question: bool) -> bool {
    if text
        .split_whitespace()
        .any(|w| w.starts_with('@') && w.len() > 2)
    {
        return true;
    }
    let words: Vec<String> = text
        .to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect();
    let Some(first) = words.first() else {
        return false;
    };
    if OTHER_PARTICIPANT_VOCATIVES.contains(&first.as_str()) {
        return true;
    }
    if THIRD_PERSON_LEAD_PRONOUNS.contains(&first.as_str())
        && !words.iter().any(|w| BOT_NAME_FORMS.contains(&w.as_str()))
        && !has_question
    {
        return true;
    }
    words.len() >= 2
        && words
            .last()
            .is_some_and(|last| OTHER_PARTICIPANT_VOCATIVES.contains(&last.as_str()))
}

pub(crate) fn explicit_sticker_request(text: &str) -> bool {
    const VERBS: &[&str] = &[
        "скинь",
        "покажи",
        "добавь",
        "кинь",
        "перестал",
        "не шл",
        "найди",
        "отправь",
        "пришли",
        "давай",
    ];
    const NOUNS: &[&str] = &["стикер", "пак", "девоч", "девуш", "гачи", "пися", "котик"];
    let t = text.to_lowercase();
    VERBS.iter().any(|v| t.contains(v)) && NOUNS.iter().any(|k| t.contains(k))
}

pub(crate) fn explicit_feature_request(text: &str) -> bool {
    const VERBS: &[&str] = &["добавь", "измени", "убери", "запили", "почини"];
    const NOUNS: &[&str] = &["бот", "фич", "функци", "поведени", "возможност"];
    let t = text.to_lowercase();
    VERBS.iter().any(|v| t.contains(v)) && NOUNS.iter().any(|k| t.contains(k))
}

pub(crate) fn sticker_request_query(text: &str) -> &'static str {
    let t = text.to_lowercase();
    if t.contains("девоч") || t.contains("девуш") {
        "девушка"
    } else if t.contains("гачи") {
        "гачимучи"
    } else if t.contains("пися") || t.contains("член") {
        "пися"
    } else if t.contains("кот") {
        "кот"
    } else if t.contains("собак") {
        "собака"
    } else if t.contains("груст") {
        "грустный"
    } else {
        "стикер"
    }
}
