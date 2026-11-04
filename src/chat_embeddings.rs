//! Vector index over `chat_messages` plus the bounded `/embeddings` transport.
//!
//! `chat_embeddings` stores one unit vector per unflagged message. Vectors are
//! normalized on write so a query scores by dot product alone. The model name
//! is stored beside each vector: a configured model change makes existing rows
//! foreign, and the scan re-embeds them instead of mixing dimensions.
//!
//! Only `flags = 0` rows are indexed, matching the memory guarantee that
//! injection and secret messages never reach the model. Retrieval lives in
//! [`crate::chat_search`].

use crate::*;
use serde_json::json;

pub(crate) fn enabled() -> bool {
    !config().memory.embedding_model.trim().is_empty()
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Unit vector for dot-product scoring; `None` for an empty, zero, or
/// non-finite vector, which the provider should never return.
pub(crate) fn unit_vector(vector: Vec<f32>) -> Option<Vec<f32>> {
    if vector.is_empty() || vector.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return None;
    }
    Some(vector.into_iter().map(|value| value / norm).collect())
}

/// Parses the `data` array of an embeddings response. Vectors keep request
/// order except where a provider-supplied `index` says otherwise; every vector
/// must match `memory.embedding_dim` when that is set, so a model or provider
/// swap cannot silently store incompatible dimensions.
fn parse_embeddings(bytes: &[u8], expected: usize) -> Result<Vec<Vec<f32>>, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "embeddings json".to_string())?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "embeddings data".to_string())?;
    if data.len() != expected {
        return Err("embeddings count mismatch".into());
    }
    let dim = config().memory.embedding_dim;
    let mut vectors = vec![Vec::new(); expected];
    for (position, item) in data.iter().enumerate() {
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|index| index as usize)
            .unwrap_or(position);
        let slot = vectors
            .get_mut(index)
            .ok_or_else(|| "embeddings index out of range".to_string())?;
        let raw = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| "embeddings vector".to_string())?;
        if raw.len() > 8192 {
            return Err("embeddings dimension out of range".into());
        }
        if dim > 0 && raw.len() != dim {
            return Err(format!("embeddings dimension {} != {dim}", raw.len()));
        }
        let mut vector = Vec::with_capacity(raw.len());
        for value in raw {
            let value = value
                .as_f64()
                .ok_or_else(|| "embeddings value".to_string())?;
            vector.push(value as f32);
        }
        *slot = vector;
    }
    if vectors.iter().any(Vec::is_empty) {
        return Err("embeddings missing vector".into());
    }
    Ok(vectors)
}

/// One OpenAI-compatible `/embeddings` call under the shared LLM retry policy:
/// transport errors and `config().retry.llm.retry_statuses` get exponential
/// backoff until `attempts` or the deadline runs out. The response body is
/// capped by `memory.embedding_max_response_bytes`, separately from
/// `llm.max_response_bytes`, because a batch of vectors is far larger than a
/// completion.
pub(crate) async fn embed_texts(
    chat: &ChatConfig,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let model = config().memory.embedding_model.trim();
    if model.is_empty() {
        return Err("embeddings disabled".into());
    }
    let mut url = chat.base_url.clone();
    url.set_path(&format!("{}/embeddings", url.path().trim_end_matches('/')));
    let payload = json!({"model": model, "input": inputs}).to_string();
    let policy = &config().retry.llm;
    let max_bytes = config().memory.embedding_max_response_bytes;
    let deadline = time::Instant::now()
        + Duration::from_secs(config().llm.request_timeout_secs).saturating_mul(policy.attempts);
    let mut attempt = 0u32;
    loop {
        let remaining = deadline.saturating_duration_since(time::Instant::now());
        if remaining.is_zero() {
            return Err("embeddings timeout".into());
        }
        let request = chat
            .http
            .post(url.clone())
            .bearer_auth(&chat.api_key)
            .header("content-type", "application/json")
            .body(payload.clone());
        let response = match time::timeout(remaining, request.send()).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => {
                if policy.retry_transport && policy.should_retry(attempt) {
                    if !policy.backoff(attempt, deadline).await {
                        return Err("embeddings timeout".into());
                    }
                    attempt += 1;
                    continue;
                }
                return Err("embeddings transport".into());
            }
            Err(_) => {
                if policy.retry_transport && policy.should_retry(attempt) {
                    if !policy.backoff(attempt, deadline).await {
                        return Err("embeddings timeout".into());
                    }
                    attempt += 1;
                    continue;
                }
                return Err("embeddings timeout".into());
            }
        };
        let status = response.status();
        if !status.is_success() {
            if policy.retry_status(status.as_u16()) && policy.should_retry(attempt) {
                if !policy.backoff(attempt, deadline).await {
                    return Err(format!("embeddings HTTP {}", status.as_u16()));
                }
                attempt += 1;
                continue;
            }
            return Err(format!("embeddings HTTP {}", status.as_u16()));
        }
        let remaining = deadline.saturating_duration_since(time::Instant::now());
        if remaining.is_zero() {
            return Err("embeddings timeout".into());
        }
        let bytes = match time::timeout(remaining, response.bytes()).await {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(_)) | Err(_) => {
                if policy.retry_transport && policy.should_retry(attempt) {
                    if !policy.backoff(attempt, deadline).await {
                        return Err("embeddings timeout".into());
                    }
                    attempt += 1;
                    continue;
                }
                return Err("embeddings transport".into());
            }
        };
        if bytes.len() > max_bytes {
            return Err("embeddings response too large".into());
        }
        return parse_embeddings(&bytes, inputs.len());
    }
}

/// Indexes up to `limit` unflagged messages that have no vector for the
/// configured model, in bounded API batches. Returns the number written.
pub(crate) async fn index_pending(
    chat: &ChatConfig,
    pool: &SqlitePool,
    limit: usize,
) -> Result<usize, String> {
    let model = config().memory.embedding_model.trim().to_owned();
    if model.is_empty() {
        return Err("search disabled".into());
    }
    let rows: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT m.id, m.chat_id, m.text FROM chat_messages m \
         LEFT JOIN chat_embeddings e ON e.message_id = m.id AND e.model = ? \
         WHERE m.flags = 0 AND trim(m.text) <> '' AND e.message_id IS NULL \
         ORDER BY m.id LIMIT ?",
    )
    .bind(&model)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .map_err(|_| "database".to_string())?;
    if rows.is_empty() {
        return Ok(0);
    }
    let max_chars = config().memory.embedding_max_chars;
    let batch = config().memory.embedding_batch.max(1);
    let mut indexed = 0;
    for chunk in rows.chunks(batch) {
        let mut pending = Vec::with_capacity(chunk.len());
        for (id, chat_id, text) in chunk {
            pending.push((
                *id,
                *chat_id,
                crate::chat_text::truncate_text(text, max_chars),
            ));
        }
        let texts: Vec<String> = pending.iter().map(|(_, _, text)| text.clone()).collect();
        let vectors = embed_texts(chat, &texts).await?;
        if vectors.len() != pending.len() {
            return Err("embedding count mismatch".into());
        }
        let mut normalized = Vec::with_capacity(pending.len());
        for ((id, chat_id, text), vector) in pending.into_iter().zip(vectors) {
            let vector = unit_vector(vector).ok_or("empty embedding vector")?;
            normalized.push((id, chat_id, text, encode_vector(&vector)));
        }
        let mut tx = pool.begin().await.map_err(|_| "database".to_string())?;
        for (id, chat_id, text, bytes) in normalized {
            sqlx::query(
                "INSERT INTO chat_embeddings (chat_id, message_id, chunk_text, vector, model, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(chat_id, message_id) DO UPDATE SET \
                     chunk_text = excluded.chunk_text, \
                     vector = excluded.vector, \
                     model = excluded.model, \
                     created_at = excluded.created_at",
            )
            .bind(chat_id)
            .bind(id)
            .bind(&text)
            .bind(&bytes)
            .bind(&model)
            .bind(unix_now())
            .execute(&mut *tx)
            .await
            .map_err(|_| "database".to_string())?;
            indexed += 1;
        }
        tx.commit().await.map_err(|_| "database".to_string())?;
    }
    Ok(indexed)
}

/// `(indexed, pending)` counts over unflagged non-empty messages for `model`.
pub(crate) async fn coverage(pool: &SqlitePool, model: &str) -> Result<(i64, i64), String> {
    let indexed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat_messages m \
         JOIN chat_embeddings e ON e.message_id = m.id AND e.model = ? \
         WHERE m.flags = 0",
    )
    .bind(model)
    .fetch_one(pool)
    .await
    .map_err(|_| "database".to_string())?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat_messages m \
         LEFT JOIN chat_embeddings e ON e.message_id = m.id AND e.model = ? \
         WHERE m.flags = 0 AND trim(m.text) <> '' AND e.message_id IS NULL",
    )
    .bind(model)
    .fetch_one(pool)
    .await
    .map_err(|_| "database".to_string())?;
    Ok((indexed, pending))
}

/// Owner maintenance: bounded reindex passes plus a coverage report.
pub(crate) async fn reindex(chat: &ChatConfig, pool: &SqlitePool, passes: usize) -> String {
    let model = config().memory.embedding_model.trim().to_owned();
    if model.is_empty() {
        return "поиск по истории выключен: memory.embedding_model пуст".to_string();
    }
    let mut indexed = 0usize;
    for _ in 0..passes {
        match index_pending(chat, pool, config().memory.embedding_batch).await {
            Ok(0) => break,
            Ok(count) => indexed += count,
            Err(reason) => {
                return json!({
                    "note": "индексация остановлена",
                    "indexed_now": indexed,
                    "error": reason,
                    "model": model,
                })
                .to_string();
            }
        }
    }
    match coverage(pool, &model).await {
        Ok((done, pending)) => json!({
            "note": "отчёт по покрытию индекса",
            "indexed_now": indexed,
            "indexed": done,
            "pending": pending,
            "model": model,
        })
        .to_string(),
        Err(reason) => json!({
            "note": "индексация выполнена, отчёт недоступен",
            "indexed_now": indexed,
            "error": reason,
        })
        .to_string(),
    }
}

/// Background backfill. Idempotent, bounded, and silent when embeddings are
/// not configured; a failed batch leaves rows pending for the next tick.
pub(crate) async fn chat_indexer(cfg: Arc<Settings>) {
    let mut auth_failed = false;
    loop {
        if let Some(chat) = cfg.chat.as_ref() {
            if enabled() && !auth_failed {
                for _ in 0..config().memory.embedding_passes {
                    match index_pending(chat, &cfg.database, config().memory.embedding_batch).await
                    {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(reason) => {
                            if matches!(
                                reason.as_str(),
                                "embeddings HTTP 400"
                                    | "embeddings HTTP 401"
                                    | "embeddings HTTP 403"
                                    | "embeddings HTTP 404"
                            ) {
                                auth_failed = true;
                            }
                            tracing::warn!("Chat embedding index failed: {reason}");
                            break;
                        }
                    }
                }
            }
        }
        time::sleep(Duration::from_secs(config().memory.embedding_interval_secs)).await;
    }
}
