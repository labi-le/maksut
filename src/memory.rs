use crate::*;

pub(crate) const FLAG_INJECTION: i64 = 1;
pub(crate) const FLAG_SECRET: i64 = 2;
pub(crate) const FLAG_JUNK: i64 = 4;
pub(crate) const FLAG_IGNORED: i64 = 8;

pub(crate) const IGNORE_REASON: &str = "prompt_injection";

#[derive(Clone)]
pub(crate) struct ChatMessage {
    pub(crate) author_id: i64,
    pub(crate) author_name: String,
    pub(crate) role: String,
    pub(crate) text: String,
}

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) async fn ignore_active(
    pool: &SqlitePool,
    user_id: UserId,
    now: i64,
) -> Result<bool, sqlx::Error> {
    let active: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM ignored_users WHERE user_id = ? AND until > ? LIMIT 1")
            .bind(user_id.0 as i64)
            .bind(now)
            .fetch_optional(pool)
            .await?;
    Ok(active.is_some())
}

pub(crate) async fn ignore_user(
    pool: &SqlitePool,
    user_id: UserId,
    now: i64,
    chat_id: ChatId,
    message_id: i32,
) -> Result<bool, sqlx::Error> {
    if now <= 0 {
        tracing::warn!("Ignore timestamp is invalid: {now}");
    }
    let row: Option<i64> = sqlx::query_scalar(
        "INSERT INTO ignored_users (user_id, until, reason, created_at, source_chat_id, source_message_id) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(user_id) DO UPDATE SET \
             until = excluded.until, \
             reason = excluded.reason, \
             created_at = excluded.created_at, \
             source_chat_id = excluded.source_chat_id, \
             source_message_id = excluded.source_message_id \
         WHERE ignored_users.until <= excluded.created_at \
         RETURNING user_id",
    )
    .bind(user_id.0 as i64)
    .bind(now + config().memory.ignore_secs)
    .bind(IGNORE_REASON)
    .bind(now)
    .bind(chat_id.0)
    .bind(i64::from(message_id))
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

pub(crate) async fn prune_expired_ignores(pool: &SqlitePool, now: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ignored_users WHERE until <= ?")
        .bind(now)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub(crate) struct RecordMessage<'a> {
    pub(crate) chat_id: ChatId,
    pub(crate) author_id: i64,
    pub(crate) author_name: &'a str,
    pub(crate) role: &'a str,
    pub(crate) text: &'a str,
    pub(crate) message_id: Option<i32>,
    pub(crate) flags: i64,
}

impl<'a> RecordMessage<'a> {
    pub(crate) fn new(
        chat_id: ChatId,
        author_id: i64,
        author_name: &'a str,
        role: &'a str,
        text: &'a str,
    ) -> Self {
        Self {
            chat_id,
            author_id,
            author_name,
            role,
            text,
            message_id: None,
            flags: 0,
        }
    }

    pub(crate) fn with_message_id(mut self, message_id: Option<i32>) -> Self {
        self.message_id = message_id;
        self
    }

    pub(crate) fn with_flags(mut self, flags: i64) -> Self {
        self.flags = flags;
        self
    }
}

pub(crate) async fn record_message(
    pool: &SqlitePool,
    message: RecordMessage<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO chat_messages (chat_id, author_id, author_name, role, text, message_id, flags, created_at, compacted_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(message.chat_id.0)
    .bind(message.author_id)
    .bind(message.author_name)
    .bind(message.role)
    .bind(message.text)
    .bind(message.message_id.map(i64::from))
    .bind(message.flags)
    .bind(unix_now())
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn load_memory(
    pool: &SqlitePool,
    chat_id: ChatId,
    current_id: i32,
) -> Result<(Option<String>, Vec<ChatMessage>), sqlx::Error> {
    let summary: Option<(String,)> =
        sqlx::query_as("SELECT summary FROM chat_summaries WHERE chat_id = ?")
            .bind(chat_id.0)
            .fetch_optional(pool)
            .await?;
    let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT author_id, author_name, role, text FROM chat_messages WHERE chat_id = ? AND compacted_at IS NULL AND flags = 0 AND (message_id IS NULL OR message_id != ?) ORDER BY id DESC LIMIT 200",
    )
    .bind(chat_id.0)
    .bind(i64::from(current_id))
    .fetch_all(pool)
    .await?;
    let mut selected = Vec::new();
    let mut chars = 0;
    for (author_id, author_name, role, text) in rows {
        let line = format_line(&author_name, author_id, &text);
        chars += line.chars().count();
        if chars > config().memory.recent_max_chars && !selected.is_empty() {
            break;
        }
        selected.push(ChatMessage {
            author_id,
            author_name,
            role,
            text,
        });
        if selected.len() >= config().memory.recent_messages {
            break;
        }
    }
    selected.reverse();
    let summary = summary
        .map(|(value,)| value)
        .filter(|value| !value.trim().is_empty());
    Ok((summary, selected))
}

pub(crate) fn format_line(author_name: &str, author_id: i64, text: &str) -> String {
    format_line_with_owner(author_name, author_id, false, text)
}

pub(crate) fn format_line_with_owner(
    author_name: &str,
    author_id: i64,
    owner: bool,
    text: &str,
) -> String {
    // Only the formatter may emit `[owner]`; neutralize it inside user-controlled text.
    let name = author_name
        .replace("[owner]", "[ owner ]")
        .replace('\n', " ");
    let body = text.replace("[owner]", "[ owner ]").replace('\n', " ");
    let role_tag = if owner { " [owner]" } else { "" };
    format!("{name} [{author_id}]{role_tag}: {body}")
}

pub(crate) fn validate_summary(summary: &str) -> Result<String, String> {
    let summary = summary.trim();
    if summary.is_empty() {
        return Err("empty summary".into());
    }
    let lowered = summary.to_lowercase();
    if lowered.contains("http://") || lowered.contains("https://") {
        return Err("summary contains a URL".into());
    }
    const REJECT: &[&str] = &[
        "ignore previous",
        "system prompt",
        "ты теперь",
        "developer mode",
        "jailbreak",
        "новые инструкции",
        "prompt injection",
        "разрешил всем",
        "владелец разрешил",
        "можно всем",
        "owner_id",
    ];
    if REJECT.iter().any(|marker| lowered.contains(marker)) {
        return Err("summary contains instructions".into());
    }
    Ok(summary
        .chars()
        .take(config().memory.summary_max_chars)
        .collect())
}

pub(crate) async fn compact_chat(
    chat: &ChatConfig,
    pool: &SqlitePool,
    chat_id: i64,
) -> Result<bool, String> {
    let rows: Vec<(i64, i64, String, String, i64)> = sqlx::query_as(
        "SELECT id, author_id, author_name, text, created_at FROM chat_messages WHERE chat_id = ? AND compacted_at IS NULL AND flags = 0 ORDER BY id DESC LIMIT ?",
    )
    .bind(chat_id)
    .bind(config().memory.load_limit)
    .fetch_all(pool)
    .await
    .map_err(|_| "database".to_string())?;
    if rows.is_empty() {
        return Ok(false);
    }
    let now = unix_now();
    let keep = if rows.len() > config().memory.keep_fresh {
        config().memory.keep_fresh
    } else {
        config().memory.keep_newest
    };
    let keep_ids: Vec<i64> = rows.iter().take(keep).map(|row| row.0).collect();
    let cutoff_old = now - config().memory.compact_after_secs;
    let cutoff_fresh = now - 60 * 60;
    let mut candidates: Vec<_> = rows
        .into_iter()
        .filter(|row| !keep_ids.contains(&row.0))
        .filter(|row| {
            if row.4 < cutoff_old {
                true
            } else {
                keep == config().memory.keep_fresh && row.4 < cutoff_fresh
            }
        })
        .collect();
    if candidates.is_empty() {
        return Ok(false);
    }
    candidates.reverse();
    let mut selected = Vec::new();
    let mut log = String::new();
    for (id, author_id, author_name, text, _created_at) in candidates {
        let text: String = text
            .chars()
            .take(config().memory.compact_message_chars)
            .collect();
        let line = format_line(&author_name, author_id, &text);
        if selected.len() >= config().memory.compact_batch_messages
            || (!log.is_empty()
                && log.chars().count() + line.chars().count() + 1
                    > config().memory.compact_batch_chars)
        {
            break;
        }
        log.push_str(&line);
        log.push('\n');
        selected.push(id);
    }
    if selected.is_empty() {
        return Ok(false);
    }
    let previous: Option<(String,)> =
        sqlx::query_as("SELECT summary FROM chat_summaries WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| "database".to_string())?;
    let previous = previous.map(|(value,)| value).unwrap_or_default();
    let summary = match chat.summarize(&previous, &log).await {
        Ok(summary) => summary,
        Err(reason) if reason == "empty" => {
            if previous.trim().is_empty() {
                tracing::warn!("Memory summary empty for chat {chat_id}; batch left uncompacted");
                return Ok(false);
            }
            previous.clone()
        }
        Err(reason) => return Err(reason),
    };
    let summary = match validate_summary(&summary) {
        Ok(summary) => summary,
        Err(reason) => {
            tracing::warn!("Memory summary rejected for chat {chat_id}: {reason}");
            if previous.trim().is_empty() {
                return Ok(false);
            }
            previous.trim().to_owned()
        }
    };
    let through = *selected.iter().max().unwrap_or(&0);
    let mut tx = pool.begin().await.map_err(|_| "database".to_string())?;
    sqlx::query(
        "INSERT INTO chat_summaries (chat_id, summary, through_message_id, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT(chat_id) DO UPDATE SET summary = excluded.summary, through_message_id = excluded.through_message_id, updated_at = excluded.updated_at",
    )
    .bind(chat_id)
    .bind(&summary)
    .bind(through)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|_| "database".to_string())?;
    for chunk in selected.chunks(100) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let query =
            format!("UPDATE chat_messages SET compacted_at = ? WHERE id IN ({placeholders})");
        // `placeholders` is built from the chunk length alone, never from a
        // message body, so the statement shape is fixed and injection-free;
        // sqlx 0.9 requires that audit to be stated explicitly.
        let mut request = sqlx::query(sqlx::AssertSqlSafe(query)).bind(now);
        for id in chunk {
            request = request.bind(id);
        }
        request
            .execute(&mut *tx)
            .await
            .map_err(|_| "database".to_string())?;
    }
    tx.commit().await.map_err(|_| "database".to_string())?;
    Ok(true)
}

pub(crate) async fn memory_compactor(cfg: Arc<Settings>) {
    loop {
        match prune_expired_ignores(&cfg.database, unix_now()).await {
            Ok(_) => {}
            Err(error) => tracing::warn!("Ignore cleanup failed: {error}"),
        }
        if let Some(chat) = cfg.chat.as_ref() {
            let chats: Vec<i64> = sqlx::query_scalar(
                "SELECT DISTINCT chat_id FROM chat_messages WHERE compacted_at IS NULL AND flags = 0",
            )
            .fetch_all(&cfg.database)
            .await
            .unwrap_or_default();
            for chat_id in chats {
                let lock = cfg.chat_lock(chat_id).await;
                let _guard = lock.write().await;
                for _ in 0..config().memory.compact_passes {
                    match compact_chat(chat, &cfg.database, chat_id).await {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(reason) => {
                            tracing::warn!("Memory compact failed for chat {chat_id}: {reason}");
                            break;
                        }
                    }
                }
            }
        }
        time::sleep(Duration::from_secs(config().memory.compact_interval_secs)).await;
    }
}
