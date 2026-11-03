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
    assert!(!named_content_left("почему максвелл молчит", "MyBot"));
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
