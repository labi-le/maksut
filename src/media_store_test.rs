use crate::media_store::{
    blob_description, blob_known, insert_binding, insert_blob, lookup_binding,
    refresh_binding_file_id, set_blob_description, BlobRecord, MediaBinding,
};
use crate::*;

#[tokio::test]
async fn blob_and_binding_round_trip_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("state.db").display());
    let pool = open_database(&url).await.unwrap();
    let key = "ab".repeat(32);
    assert!(!blob_known(&pool, &key).await.unwrap());

    let record = BlobRecord {
        sha256: &key,
        kind: "photo",
        bytes: 3,
        created_at: 7,
    };
    insert_blob(&pool, &record).await.unwrap();
    insert_blob(&pool, &record).await.unwrap();
    assert!(blob_known(&pool, &key).await.unwrap());
    let blobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_media_blobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(blobs, 1, "content address is the primary key");

    let binding = MediaBinding {
        chat_id: 5,
        message_id: 9,
        kind: "photo",
        file_unique_id: "unique-9",
        file_id: "file-9",
        storage_key: &key,
    };
    insert_binding(&pool, &binding, 11).await.unwrap();
    let retry = MediaBinding {
        file_id: "file-9-refreshed",
        ..binding
    };
    insert_binding(&pool, &retry, 12).await.unwrap();
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT storage_key, file_id, created_at FROM chat_media WHERE chat_id = ? AND message_id = ?",
    )
    .bind(5_i64)
    .bind(9_i64)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![(key, "file-9".to_owned(), 11)],
        "re-ingest must not rewrite an existing binding"
    );
}

#[tokio::test]
async fn blob_description_and_binding_lookup_support_telegram_and_internal_ids() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("state.db").display());
    let pool = open_database(&url).await.unwrap();
    let key = "cd".repeat(32);
    insert_blob(
        &pool,
        &BlobRecord {
            sha256: &key,
            kind: "photo",
            bytes: 128,
            created_at: 100,
        },
    )
    .await
    .unwrap();
    assert_eq!(blob_description(&pool, &key).await.unwrap(), None);
    set_blob_description(&pool, &key, "рыжий кот на клавиатуре", 105)
        .await
        .unwrap();
    assert_eq!(
        blob_description(&pool, &key).await.unwrap().as_deref(),
        Some("рыжий кот на клавиатуре")
    );

    crate::memory::record_message(
        &pool,
        RecordMessage::new(ChatId(5), 42, "аня", "user", "[вложение]")
            .with_message_id(Some(777))
            .with_flags(crate::memory::FLAG_JUNK),
    )
    .await
    .unwrap();
    let internal_id: i64 =
        sqlx::query_scalar("SELECT id FROM chat_messages WHERE chat_id = 5 AND message_id = 777")
            .fetch_one(&pool)
            .await
            .unwrap();
    insert_binding(
        &pool,
        &MediaBinding {
            chat_id: 5,
            message_id: 777,
            kind: "photo",
            file_unique_id: "u-777",
            file_id: "file-777",
            storage_key: &key,
        },
        100,
    )
    .await
    .unwrap();

    let by_internal = lookup_binding(&pool, 5, Some(internal_id), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_internal.message_id, 777);
    assert_eq!(by_internal.file_id, "file-777");
    assert_eq!(by_internal.storage_key, key);
    assert_eq!(
        by_internal.description.as_deref(),
        Some("рыжий кот на клавиатуре")
    );

    let by_tg = lookup_binding(&pool, 5, Some(777), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_tg, by_internal);

    let by_key = lookup_binding(&pool, 5, None, Some(&key))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_key, by_internal);

    assert!(
        lookup_binding(&pool, 999, None, Some(&key))
            .await
            .unwrap()
            .is_none(),
        "other chat must never resolve a binding from chat 5"
    );

    refresh_binding_file_id(&pool, 5, 777, "file-777-fresh")
        .await
        .unwrap();
    let refreshed = lookup_binding(&pool, 5, Some(777), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.file_id, "file-777-fresh");
}
