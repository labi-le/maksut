use crate::media_store::{blob_known, insert_binding, insert_blob, BlobRecord, MediaBinding};
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
