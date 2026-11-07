//! Bounded `/embeddings` transport and Qdrant chat-history indexer.
//!
//! Vectors live exclusively in Qdrant (`crate::qdrant_index`). SQLite holds only
//! the raw `chat_messages` log and a per-model `memory_meta` cursor. Only
//! `flags = 0` rows are embedded, matching the memory guarantee that injection
//! and secret messages never reach the model. Retrieval lives in
//! [`crate::chat_search`].

use crate::qdrant::QdrantClient;
use crate::*;
use serde_json::json;

pub(crate) fn enabled() -> bool {
    !config().memory.embedding_model.trim().is_empty()
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
    let configured = config().memory.embedding_url.trim();
    let mut url = if configured.is_empty() {
        chat.base_url.clone()
    } else {
        Url::parse(configured).map_err(|_| "embeddings url".to_string())?
    };
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

/// Indexes unflagged messages directly into Qdrant through `qdrant_index::sync_chat_points`.
pub(crate) async fn index_pending(
    chat: &ChatConfig,
    pool: &SqlitePool,
    client: &QdrantClient,
) -> Result<usize, String> {
    crate::qdrant_index::sync_chat_points(chat, pool, client).await
}

/// `(indexed, pending)` counts over unflagged non-empty messages for `model`
/// relative to the Qdrant cursor stored in `memory_meta`.
pub(crate) async fn coverage(
    pool: &SqlitePool,
    client: Option<&QdrantClient>,
    model: &str,
) -> Result<(i64, i64), String> {
    let cursor = crate::qdrant_index::chat_cursor_id(pool, client, model).await;
    let indexed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat_messages \
         WHERE flags = 0 AND trim(text) <> '' AND id <= ?",
    )
    .bind(cursor)
    .fetch_one(pool)
    .await
    .map_err(|_| "database".to_string())?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat_messages \
         WHERE flags = 0 AND trim(text) <> '' AND id > ?",
    )
    .bind(cursor)
    .fetch_one(pool)
    .await
    .map_err(|_| "database".to_string())?;
    Ok((indexed, pending))
}

/// Owner maintenance: bounded reindex passes into Qdrant plus a coverage report.
pub(crate) async fn reindex(chat: &ChatConfig, pool: &SqlitePool, passes: usize) -> String {
    let model = config().memory.embedding_model.trim().to_owned();
    if model.is_empty() {
        return "поиск по истории выключен: memory.embedding_model пуст".to_string();
    }
    let Some(client) = QdrantClient::from_config() else {
        return "векторный индекс выключен: memory.qdrant_url пуст".to_string();
    };
    reindex_with(chat, pool, &client, passes).await
}

pub(crate) async fn reindex_with(
    chat: &ChatConfig,
    pool: &SqlitePool,
    client: &QdrantClient,
    passes: usize,
) -> String {
    let model = config().memory.embedding_model.trim().to_owned();
    if model.is_empty() {
        return "поиск по истории выключен: memory.embedding_model пуст".to_string();
    }
    let indexed =
        match crate::qdrant_index::sync_chat_points_passes(chat, pool, client, passes).await {
            Ok(count) => count,
            Err((partial, reason)) => {
                return json!({
                    "note": "индексация остановлена",
                    "indexed_now": partial,
                    "error": reason,
                    "model": model,
                })
                .to_string();
            }
        };
    match coverage(pool, Some(client), &model).await {
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

/// Background Qdrant backfill. Idempotent, bounded, and silent when embeddings
/// or Qdrant are not configured; a failed batch leaves the cursor unchanged for
/// the next tick.
pub(crate) async fn chat_indexer(cfg: Arc<Settings>) {
    let mut auth_failed = false;
    loop {
        if let Some(chat) = cfg.chat.as_ref() {
            if enabled() && !auth_failed {
                if let Some(client) = QdrantClient::from_config() {
                    match index_pending(chat, &cfg.database, &client).await {
                        Ok(0) => {}
                        Ok(count) => tracing::debug!(count, "chat vectors indexed in qdrant"),
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
                            tracing::warn!("Qdrant chat index failed: {reason}");
                        }
                    }
                    if !auth_failed {
                        match crate::qdrant_index::sync_self_points(chat, &cfg.database, &client)
                            .await
                        {
                            Ok(0) => {}
                            Ok(count) => tracing::debug!(count, "self facts synced to qdrant"),
                            Err(reason) => {
                                tracing::warn!("Qdrant self memory sync failed: {reason}");
                            }
                        }
                    }
                }
            }
        }
        time::sleep(Duration::from_secs(config().memory.embedding_interval_secs)).await;
    }
}
