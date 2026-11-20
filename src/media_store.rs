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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredChatMedia {
    pub(crate) chat_id: i64,
    pub(crate) message_id: i64,
    pub(crate) kind: String,
    pub(crate) file_id: String,
    pub(crate) storage_key: String,
    pub(crate) description: Option<String>,
}

pub(crate) async fn init_media_store(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS chat_media_blobs (sha256 TEXT PRIMARY KEY, kind TEXT NOT NULL, bytes INTEGER NOT NULL, created_at INTEGER NOT NULL, description TEXT NOT NULL DEFAULT '', described_at INTEGER NOT NULL DEFAULT 0)",
    )
    .execute(pool)
    .await?;
    let _ =
        sqlx::query("ALTER TABLE chat_media_blobs ADD COLUMN description TEXT NOT NULL DEFAULT ''")
            .execute(pool)
            .await;
    let _ = sqlx::query(
        "ALTER TABLE chat_media_blobs ADD COLUMN described_at INTEGER NOT NULL DEFAULT 0",
    )
    .execute(pool)
    .await;
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

pub(crate) async fn blob_description(
    pool: &SqlitePool,
    sha256: &str,
) -> Result<Option<String>, sqlx::Error> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT description FROM chat_media_blobs WHERE sha256 = ?")
            .bind(sha256)
            .fetch_optional(pool)
            .await?;
    Ok(found.filter(|text| !text.trim().is_empty()))
}

pub(crate) async fn set_blob_description(
    pool: &SqlitePool,
    sha256: &str,
    description: &str,
    described_at: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE chat_media_blobs SET description = ?, described_at = ? WHERE sha256 = ?")
        .bind(description)
        .bind(described_at)
        .bind(sha256)
        .execute(pool)
        .await?;
    Ok(())
}

pub(crate) async fn lookup_binding(
    pool: &SqlitePool,
    chat_id: i64,
    message_id: Option<i64>,
    storage_key: Option<&str>,
) -> Result<Option<StoredChatMedia>, sqlx::Error> {
    let row: Option<(i64, i64, String, String, String, String)> = match (message_id, storage_key) {
        (Some(msg_id), _) => {
            sqlx::query_as(
                "SELECT c.chat_id, c.message_id, c.kind, c.file_id, c.storage_key, COALESCE(b.description, '') \
                 FROM chat_media c \
                 LEFT JOIN chat_media_blobs b ON b.sha256 = c.storage_key \
                 WHERE c.chat_id = ? AND (c.message_id = ? OR c.message_id = (SELECT message_id FROM chat_messages WHERE chat_id = ? AND id = ?)) \
                 ORDER BY c.created_at DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(msg_id)
            .bind(chat_id)
            .bind(msg_id)
            .fetch_optional(pool)
            .await?
        }
        (None, Some(key)) => {
            sqlx::query_as(
                "SELECT c.chat_id, c.message_id, c.kind, c.file_id, c.storage_key, COALESCE(b.description, '') \
                 FROM chat_media c \
                 LEFT JOIN chat_media_blobs b ON b.sha256 = c.storage_key \
                 WHERE c.chat_id = ? AND c.storage_key = ? \
                 ORDER BY c.created_at DESC LIMIT 1",
            )
            .bind(chat_id)
            .bind(key)
            .fetch_optional(pool)
            .await?
        }
        (None, None) => None,
    };
    Ok(row.map(
        |(chat_id, message_id, kind, file_id, storage_key, desc)| StoredChatMedia {
            chat_id,
            message_id,
            kind,
            file_id,
            storage_key,
            description: (!desc.trim().is_empty()).then_some(desc),
        },
    ))
}

pub(crate) async fn refresh_binding_file_id(
    pool: &SqlitePool,
    chat_id: i64,
    message_id: i64,
    file_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE chat_media SET file_id = ? WHERE chat_id = ? AND message_id = ?")
        .bind(file_id)
        .bind(chat_id)
        .bind(message_id)
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
