use crate::test_support::*;
use crate::*;
use serde_json::json;

/// Per-chat ids are unique per test: the resolution cache is process-wide and
/// keyed by chat id, exactly like production.
#[tokio::test]
async fn set_resolve_and_reset_roundtrip_through_the_cache() {
    let (_dir, pool) = test_pool().await;
    let chat = ChatId(-910001);
    let stored = crate::chat_settings::set(
        &pool,
        chat,
        RouterSettingsUpdate {
            mode: Some(RouterMode::Off),
            min_confidence: Some(0.25),
        },
    )
    .await
    .unwrap();
    assert_eq!(stored.mode, RouterMode::Off);
    assert_eq!(stored.min_confidence, 0.25);
    assert_eq!(
        crate::chat_settings::router_settings(&pool, chat).await,
        stored
    );

    // The write path refreshed the cache, so a row edited behind the bot's back
    // is invisible until the TTL expires or the entry is invalidated.
    sqlx::query("UPDATE chat_settings SET router_min_confidence = 0.75 WHERE chat_id = ?")
        .bind(chat.0)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        crate::chat_settings::router_settings(&pool, chat)
            .await
            .min_confidence,
        0.25,
        "a cached chat must not pay for a SQLite read per message"
    );

    assert!(crate::chat_settings::reset(&pool, chat).await.unwrap());
    assert_eq!(
        crate::chat_settings::router_settings(&pool, chat).await,
        ChatRouterSettings::global(),
        "reset must drop the cached override at once"
    );
    assert!(
        !crate::chat_settings::reset(&pool, chat).await.unwrap(),
        "a second reset reports that no override existed"
    );
}

#[tokio::test]
async fn a_garbage_row_falls_back_to_the_global_section() {
    let (_dir, pool) = test_pool().await;
    let chat = ChatId(-910002);
    sqlx::query(
        "INSERT INTO chat_settings (chat_id, router_mode, router_min_confidence) VALUES (?, ?, ?)",
    )
    .bind(chat.0)
    .bind("wat")
    .bind(7.5)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        crate::chat_settings::router_settings(&pool, chat).await,
        ChatRouterSettings::global(),
        "an unknown mode and an out-of-range threshold must not disable the chat"
    );
}

#[test]
fn validation_refuses_jev_without_a_client_and_bad_thresholds() {
    let jev = RouterSettingsUpdate {
        mode: Some(RouterMode::Jev),
        min_confidence: None,
    };
    assert!(jev.validate(false).is_err());
    assert!(jev.validate(true).is_ok());

    for bad in [1.5, -0.1, f64::NAN, f64::INFINITY] {
        let update = RouterSettingsUpdate {
            mode: None,
            min_confidence: Some(bad),
        };
        assert!(update.validate(true).is_err(), "{bad}");
    }
    assert!(RouterSettingsUpdate::default().validate(true).is_ok());
}

#[tokio::test]
async fn set_refuses_invalid_values_and_an_empty_update() {
    let (_dir, pool) = test_pool().await;
    let chat = ChatId(-910003);
    let out_of_range = crate::chat_settings::set(
        &pool,
        chat,
        RouterSettingsUpdate {
            mode: None,
            min_confidence: Some(2.0),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(out_of_range, SettingsError::Refused(_)));

    let empty = crate::chat_settings::set(&pool, chat, RouterSettingsUpdate::default())
        .await
        .unwrap_err();
    assert!(matches!(empty, SettingsError::Refused(_)));
    assert!(crate::chat_router_row(&pool, chat).await.unwrap().is_none());
}

#[tokio::test]
async fn owner_tool_gets_sets_and_resets_the_current_chat() {
    let (_dir, pool) = test_pool().await;
    let chat = ChatId(-910004);
    let url = Url::parse("http://127.0.0.1:1/v1").unwrap();
    let chat_cfg = ChatConfig::new(
        url.clone(),
        "key".into(),
        "model".into(),
        url,
        SearchProvider::DuckDuckGo,
        None,
    );
    let stickers = StickerReplies::for_tests();
    let catalog = StickerCatalog::for_tests(&[]);
    let ctx = ToolContext {
        chat: &chat_cfg,
        pool: &pool,
        stickers: &stickers,
        catalog: &catalog,
        proposal: None,
        owner: true,
        chat_id: chat,
        user_id: UserId(1),
        sender_name: "owner",
        username: None,
        message_id: 1,
        reply_user_id: None,
        bot: None,
        read_url_targets: &[],
    };

    let read = crate::chat_settings::run_tool(&json!({"action": "get"}), &ctx).await;
    assert!(read.contains("-910004"), "{read}");
    assert!(read.contains("\"per_chat\":false"), "{read}");

    let written = crate::chat_settings::run_tool(
        &json!({"action": "set", "router_mode": "off", "router_min_confidence": 0.3}),
        &ctx,
    )
    .await;
    assert!(written.contains("порог 0.3"), "{written}");

    let read = crate::chat_settings::run_tool(&json!({"action": "get"}), &ctx).await;
    assert!(read.contains("\"per_chat\":true"), "{read}");
    assert!(read.contains("\"router_min_confidence\":0.3"), "{read}");

    let reset = crate::chat_settings::run_tool(&json!({"action": "reset"}), &ctx).await;
    assert!(reset.contains("сбросил"), "{reset}");
    let again = crate::chat_settings::run_tool(&json!({"action": "reset"}), &ctx).await;
    assert!(again.contains("не было"), "{again}");

    let bad_action = crate::chat_settings::run_tool(&json!({"action": "wat"}), &ctx).await;
    assert!(bad_action.contains("get, set или reset"), "{bad_action}");
}

#[tokio::test]
async fn list_reports_stored_overrides() {
    let (_dir, pool) = test_pool().await;
    for (chat, confidence) in [(ChatId(-910005), 0.2), (ChatId(-910006), 0.4)] {
        crate::chat_settings::set(
            &pool,
            chat,
            RouterSettingsUpdate {
                mode: Some(RouterMode::Off),
                min_confidence: Some(confidence),
            },
        )
        .await
        .unwrap();
    }
    let rows = crate::chat_settings::list(&pool).await.unwrap();
    let seen: Vec<(i64, f64)> = rows
        .iter()
        .map(|row| (row.chat_id, row.effective.min_confidence))
        .collect();
    assert!(seen.contains(&(-910005, 0.2)), "{seen:?}");
    assert!(seen.contains(&(-910006, 0.4)), "{seen:?}");
}

#[tokio::test]
async fn a_stored_row_overrides_the_global_mode_and_threshold() {
    let (_dir, pool) = test_pool().await;
    let chat = ChatId(-910008);
    sqlx::query(
        "INSERT INTO chat_settings (chat_id, router_mode, router_min_confidence) VALUES (?, ?, ?)",
    )
    .bind(chat.0)
    .bind("jev")
    .bind(0.5)
    .execute(&pool)
    .await
    .unwrap();

    let settings = crate::chat_settings::router_settings(&pool, chat).await;
    assert_eq!(settings.mode, RouterMode::Jev);
    assert_eq!(settings.min_confidence, 0.5);
    assert_ne!(
        settings,
        ChatRouterSettings::global(),
        "a stored row must be able to differ from the global [router] section"
    );
}
