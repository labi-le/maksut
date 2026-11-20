use crate::*;

/// Message-to-blob binding. `storage_key` is the sha256 of the bytes and the
/// only pointer into the object store; `file_id` is kept for a cheap Telegram
/// re-send and may expire, so it never authorizes a lookup on its own.
pub(crate) struct MediaBinding<'a> {
    pub(crate) chat_id: i64,
    pub(crate) message_id: i64,
    pub(crate) kind: &'a str,
    pub(crate) file_unique_id: &'a str,
    pub(crate) file_id: &'a str,
    pub(crate) storage_key: &'a str,
}

pub(crate) struct BlobRecord<'a> {
    pub(crate) sha256: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) bytes: i64,
    pub(crate) created_at: i64,
}

pub(crate) async fn init_media_store(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS chat_media_blobs (sha256 TEXT PRIMARY KEY, kind TEXT NOT NULL, bytes INTEGER NOT NULL, created_at INTEGER NOT NULL)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS chat_media (chat_id INTEGER NOT NULL, message_id INTEGER NOT NULL, kind TEXT NOT NULL, file_unique_id TEXT NOT NULL, file_id TEXT NOT NULL, storage_key TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(chat_id, message_id))",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS chat_media_blob ON chat_media(storage_key)")
        .execute(pool)
        .await?;
    Ok(())
}

pub(crate) async fn blob_known(pool: &SqlitePool, sha256: &str) -> Result<bool, sqlx::Error> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM chat_media_blobs WHERE sha256 = ?")
        .bind(sha256)
        .fetch_optional(pool)
        .await?;
    Ok(found.is_some())
}

pub(crate) async fn insert_blob(
    pool: &SqlitePool,
    record: &BlobRecord<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO chat_media_blobs (sha256, kind, bytes, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(sha256) DO NOTHING",
    )
    .bind(record.sha256)
    .bind(record.kind)
    .bind(record.bytes)
    .bind(record.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn insert_binding(
    pool: &SqlitePool,
    binding: &MediaBinding<'_>,
    created_at: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO chat_media (chat_id, message_id, kind, file_unique_id, file_id, storage_key, created_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(chat_id, message_id) DO NOTHING",
    )
    .bind(binding.chat_id)
    .bind(binding.message_id)
    .bind(binding.kind)
    .bind(binding.file_unique_id)
    .bind(binding.file_id)
    .bind(binding.storage_key)
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(())
}
