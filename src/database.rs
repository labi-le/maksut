use crate::*;

pub(crate) async fn open_database(url: &str) -> Result<SqlitePool, sqlx::Error> {
    let started = time::Instant::now();
    let options = SqliteConnectOptions::from_str(url)
        .inspect_err(|_| {
            tracing::error!(stage = "options", "database open failed");
        })?
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .inspect_err(|_| {
            tracing::error!(
                stage = "connect",
                elapsed_ms = elapsed_ms(started),
                "database open failed"
            );
        })?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS group_settings (chat_id INTEGER PRIMARY KEY, enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)))",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS download_failures (created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')), url_host TEXT NOT NULL, url_path TEXT NOT NULL, stage TEXT NOT NULL, detail TEXT NOT NULL, exit_code INTEGER)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS download_failure_events (failure_rowid INTEGER NOT NULL, event_index INTEGER NOT NULL, stage TEXT NOT NULL, detail TEXT NOT NULL, exit_code INTEGER, PRIMARY KEY(failure_rowid, event_index))",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS chat_turns (id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id INTEGER NOT NULL, user_id INTEGER NOT NULL, role TEXT NOT NULL CHECK(role IN ('user', 'assistant')), content TEXT NOT NULL, created_at INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS chat_turns_lookup ON chat_turns(chat_id, user_id, created_at)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS chat_messages (id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id INTEGER NOT NULL, author_id INTEGER NOT NULL, author_name TEXT NOT NULL, role TEXT NOT NULL CHECK(role IN ('user', 'assistant')), text TEXT NOT NULL, message_id INTEGER, flags INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, compacted_at INTEGER)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS chat_messages_lookup ON chat_messages(chat_id, compacted_at, id)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS chat_summaries (chat_id INTEGER PRIMARY KEY, summary TEXT NOT NULL, through_message_id INTEGER NOT NULL, updated_at INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await?;
    // Vectors live exclusively in Qdrant; drop any legacy SQLite vector table
    // so existing databases release the blob pages.
    sqlx::query("DROP INDEX IF EXISTS chat_embeddings_lookup")
        .execute(&pool)
        .await?;
    sqlx::query("DROP TABLE IF EXISTS chat_embeddings")
        .execute(&pool)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS memory_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS ignored_users (user_id INTEGER PRIMARY KEY, until INTEGER NOT NULL, reason TEXT NOT NULL, created_at INTEGER NOT NULL, source_chat_id INTEGER NOT NULL, source_message_id INTEGER)",
    )
    .execute(&pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS ignored_users_until ON ignored_users(until)")
        .execute(&pool)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS bot_self_memory (topic TEXT PRIMARY KEY, fact TEXT NOT NULL, source_url TEXT NOT NULL, updated_at INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS llm_usage (id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id INTEGER NOT NULL, user_id INTEGER NOT NULL, author_name TEXT NOT NULL, model TEXT NOT NULL, kind TEXT NOT NULL, input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL DEFAULT 0, reasoning_tokens INTEGER NOT NULL DEFAULT 0, cost_microdollars INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS llm_usage_chat_time ON llm_usage(chat_id, created_at)")
        .execute(&pool)
        .await?;
    // Retention deletes and the owner's all-chats report filter on time alone.
    sqlx::query("CREATE INDEX IF NOT EXISTS llm_usage_time ON llm_usage(created_at)")
        .execute(&pool)
        .await?;
    migrate_chat_turns(&pool).await?;
    init_cache(&pool).await?;
    tracing::info!(
        elapsed_ms = elapsed_ms(started),
        "database pool and schema ready"
    );
    Ok(pool)
}

pub(crate) async fn group_enabled(pool: &SqlitePool, chat_id: ChatId) -> Result<bool, sqlx::Error> {
    let enabled: Option<i64> =
        sqlx::query_scalar("SELECT enabled FROM group_settings WHERE chat_id = ?")
            .bind(chat_id.0)
            .fetch_optional(pool)
            .await?;
    Ok(enabled == Some(1))
}

pub(crate) async fn set_group_enabled(
    pool: &SqlitePool,
    chat_id: ChatId,
    enabled: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO group_settings (chat_id, enabled) VALUES (?, ?) ON CONFLICT(chat_id) DO UPDATE SET enabled = excluded.enabled",
    )
    .bind(chat_id.0)
    .bind(i64::from(enabled))
    .execute(pool)
    .await?;
    Ok(())
}

async fn migrate_chat_turns(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let migrated: Option<String> =
        sqlx::query_scalar("SELECT value FROM memory_meta WHERE key = 'chat_turns_migrated'")
            .fetch_optional(pool)
            .await?;
    if migrated.is_some() {
        return Ok(());
    }
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
        - 24 * 60 * 60;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO chat_messages (chat_id, author_id, author_name, role, text, message_id, flags, created_at, compacted_at) SELECT chat_id, CASE WHEN role = 'assistant' THEN 0 ELSE user_id END, CASE WHEN role = 'assistant' THEN 'максут' ELSE CAST(user_id AS TEXT) END, role, content, NULL, 0, created_at, NULL FROM chat_turns WHERE created_at >= ? ORDER BY id",
    )
    .bind(cutoff)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO memory_meta (key, value) VALUES ('chat_turns_migrated', '1')")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
