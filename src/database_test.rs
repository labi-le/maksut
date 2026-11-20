use crate::test_support::*;
use crate::*;

#[tokio::test]
async fn group_toggle_survives_reopening_database() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("state.db").display());
    let pool = open_database(&url).await.unwrap();
    let chat = ChatId(-100123);
    assert!(!group_enabled(&pool, chat).await.unwrap());
    set_group_enabled(&pool, chat, true).await.unwrap();
    pool.close().await;
    let pool = open_database(&url).await.unwrap();
    assert!(group_enabled(&pool, chat).await.unwrap());
    set_group_enabled(&pool, chat, false).await.unwrap();
    assert!(!group_enabled(&pool, chat).await.unwrap());
}

#[tokio::test]
async fn chat_router_rows_roundtrip_and_delete() {
    let (_dir, pool) = test_pool().await;
    let chat = ChatId(-100456);
    assert!(chat_router_row(&pool, chat).await.unwrap().is_none());

    set_chat_router_row(&pool, chat, "jev", 0.42).await.unwrap();
    assert_eq!(
        chat_router_row(&pool, chat).await.unwrap(),
        Some(("jev".to_string(), 0.42))
    );
    assert_eq!(
        list_chat_router_rows(&pool, 10).await.unwrap(),
        vec![(chat.0, "jev".to_string(), 0.42)]
    );

    assert!(delete_chat_router_row(&pool, chat).await.unwrap());
    assert!(!delete_chat_router_row(&pool, chat).await.unwrap());
    assert!(chat_router_row(&pool, chat).await.unwrap().is_none());
}
