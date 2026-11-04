use crate::log_fields::{
    mask_secret, sanitize_json, sanitize_json_text, sanitize_text, MAX_JSON_CHARS, MAX_TEXT_CHARS,
};
use serde_json::json;

#[test]
fn mask_secret_hides_value_and_length() {
    assert_eq!(mask_secret(""), "***");
    assert_eq!(mask_secret("abc"), "***");
    assert_eq!(mask_secret("abcdefghijk"), "***");
    assert_eq!(mask_secret("abcdefghijkl"), "abc***kl");
    assert_eq!(mask_secret("abcdefghijklmnop"), "abc***op");
}

#[test]
fn sanitize_text_masks_tokens_but_keeps_prose() {
    let secret = format!("sk-proj-{}", "1".repeat(16));
    let key = format!("ghp_{}", "a".repeat(16));
    let text = format!("ищу {secret} и {key}");
    let out = sanitize_text(&text);
    assert!(out.contains("***"), "{out}");
    assert!(out.contains("ищу"), "{out}");
    assert!(!out.contains("sk-proj"), "{out}");
    assert!(!out.contains("ghp_"), "{out}");
}

#[test]
fn sanitize_text_caps_long_values() {
    let text = "слово ".repeat(MAX_TEXT_CHARS);
    let out = sanitize_text(&text);
    assert!(out.chars().count() <= MAX_TEXT_CHARS + 1, "{}", out.len());
}

#[test]
fn sanitize_json_masks_sensitive_fields_and_nested_strings() {
    let args = json!({
        "query": "обычный запрос",
        "api_key": format!("sk-proj-{}", "1".repeat(16)),
        "access_token": "abcdefghijklmnopqrstuvwxyz",
        "client_secret": "shhhh",
        "options": {"Authorization": format!("Bearer {}", "a".repeat(16)), "limit": 5}
    });
    let out = sanitize_json(&args);
    assert_eq!(out["api_key"], "sk-***11", "{out}");
    assert_eq!(out["access_token"], "abc***yz", "{out}");
    assert_eq!(out["client_secret"], "***", "{out}");
    assert_eq!(out["options"]["Authorization"], "Bea***aa", "{out}");
    assert_eq!(out["query"], "обычный запрос", "{out}");
    assert_eq!(out["options"]["limit"], 5, "{out}");
}

#[test]
fn sanitize_json_text_stays_bounded() {
    let args = json!({
        "a": "x".repeat(500),
        "b": "y".repeat(500),
        "c": "z".repeat(500)
    });
    let out = sanitize_json_text(&args);
    assert_eq!(out.chars().count(), MAX_JSON_CHARS + 1, "{out}");
    assert!(out.ends_with('…'), "{out}");
}
