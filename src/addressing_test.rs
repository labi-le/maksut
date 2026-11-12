use crate::addressing::{address_kind, explicit_mention, named_content_left, AddressKind};
use teloxide::types::Message;

fn message(chat_type: &str, text: &str) -> Message {
    serde_json::from_value(serde_json::json!({
        "message_id": 1,
        "date": 1700000000,
        "chat": { "id": 7, "type": chat_type },
        "text": text
    }))
    .unwrap()
}
#[test]
fn bare_name_has_no_content() {
    assert!(!named_content_left("максут", "MyBot"));
    assert!(!named_content_left("  Максут! ", "MyBot"));
    assert!(!named_content_left("maksut", "MyBot"));
    assert!(!named_content_left("максут ?", "MyBot"));
    assert!(!named_content_left("максут, ...", "MyBot"));
}

#[test]
fn name_with_text_counts_as_addressed() {
    assert!(named_content_left("максут, что нового?", "MyBot"));
    assert!(named_content_left("Maksut привет", "MyBot"));
    assert!(!named_content_left("почему игорёк молчит", "MyBot"));
}

#[test]
fn mention_must_be_the_whole_word() {
    assert!(explicit_mention("@MyBot привет", "MyBot"));
    assert!(explicit_mention("привет @mybot!", "MyBot"));
    assert!(!explicit_mention("@MyBot_extra привет", "MyBot"));
    assert!(!explicit_mention("x@MyBot.ru", "MyBot"));
    assert!(!explicit_mention("привет", "MyBot"));
}

fn kind(chat_type: &str, text: &str) -> AddressKind {
    address_kind(&message(chat_type, text), text, "MyBot")
}

#[test]
fn classifier_matches_prompt_addressing() {
    assert_eq!(kind("supergroup", "максут"), AddressKind::None);
    assert_eq!(kind("supergroup", "максут ?"), AddressKind::None);
    assert_eq!(
        kind("supergroup", "максут, что нового?"),
        AddressKind::Named
    );
    assert_eq!(kind("supergroup", "@MyBot привет"), AddressKind::Explicit);
    assert_eq!(kind("supergroup", "@MyBot"), AddressKind::None);
    assert_eq!(kind("private", "привет"), AddressKind::Explicit);
}

#[test]
fn bare_mention_is_not_explicit() {
    assert!(!explicit_mention("@MyBot", "MyBot"));
    assert!(explicit_mention("@MyBot что нового", "MyBot"));
}

#[test]
fn reply_to_bot_media_post_is_not_addressed_to_bot() {
    let mut reply_msg =
        crate::feature_support::feature_message(-1002045081420, 1, Some("video"), "v1");
    reply_msg["from"] = serde_json::json!({
        "id": 8399828471_i64,
        "is_bot": true,
        "first_name": "Maksut",
        "username": "MyBot"
    });
    reply_msg["caption"] = serde_json::json!("Делаем это вместе\nОт Андрей\nСсылка");
    let reply_to_video: Message = serde_json::from_value(serde_json::json!({
        "message_id": 218897,
        "date": 1700000100,
        "chat": { "id": -1002045081420_i64, "type": "supergroup" },
        "from": { "id": 280584660, "is_bot": false, "first_name": "Вадик" },
        "text": "почему у него МакБук изогнутый",
        "reply_to_message": reply_msg
    }))
    .unwrap();
    assert!(!crate::addressing::reply_to_bot(&reply_to_video, "MyBot"));
    assert_eq!(
        address_kind(&reply_to_video, "почему у него МакБук изогнутый", "MyBot"),
        AddressKind::None
    );
    assert_eq!(
        address_kind(
            &reply_to_video,
            "максут почему у него МакБук изогнутый",
            "MyBot"
        ),
        AddressKind::Named
    );
    assert_eq!(
        address_kind(&reply_to_video, "это Максут", "MyBot"),
        AddressKind::None,
        "third-person 'это Максут' reply to another user must not address the bot"
    );
    assert_eq!(
        address_kind(&reply_to_video, "опять максут акции обрушил", "MyBot"),
        AddressKind::None,
        "third-person mid-sentence mention in reply to another user must not address the bot"
    );
    assert_eq!(
        address_kind(&reply_to_video, "у максута спроси", "MyBot"),
        AddressKind::None,
        "oblique case mention in reply to another user must not address the bot"
    );
    assert!(crate::chat_style::resolve_addressed_chat(
        &reply_to_video,
        "это Максут",
        "MyBot",
        false
    )
    .0
    .is_none());
    assert_eq!(
        crate::chat_style::resolve_addressed_chat(
            &reply_to_video,
            "максут почему у него МакБук изогнутый",
            "MyBot",
            false
        )
        .0,
        Some(("почему у него МакБук изогнутый".to_owned(), false))
    );
    let dm_reply_to_media: Message = serde_json::from_value(serde_json::json!({
        "message_id": 218900,
        "date": 1700000180,
        "chat": { "id": 395448554_i64, "type": "private" },
        "from": { "id": 395448554_i64, "is_bot": false, "first_name": "Денис" },
        "text": "а где звук?",
        "reply_to_message": reply_msg
    }))
    .unwrap();
    assert_eq!(
        crate::chat_style::resolve_addressed_chat(&dm_reply_to_media, "а где звук?", "MyBot", true)
            .0,
        Some(("а где звук?".to_owned(), false)),
        "private chat replies to non-bot/media messages must stay addressed with their full text"
    );

    let reply_to_text: Message = serde_json::from_value(serde_json::json!({
        "message_id": 218899,
        "date": 1700000200,
        "chat": { "id": -1002045081420_i64, "type": "supergroup" },
        "from": { "id": 280584660, "is_bot": false, "first_name": "Вадик" },
        "text": "а щас",
        "reply_to_message": {
            "message_id": 218898,
            "date": 1700000150,
            "chat": { "id": -1002045081420_i64, "type": "supergroup" },
            "from": { "id": 8399828471_i64, "is_bot": true, "first_name": "Maksut", "username": "MyBot" },
            "text": "обычный ответ бота"
        }
    }))
    .unwrap();
    assert!(crate::addressing::reply_to_bot(&reply_to_text, "MyBot"));
    assert_eq!(
        address_kind(&reply_to_text, "а щас", "MyBot"),
        AddressKind::Explicit
    );
}
