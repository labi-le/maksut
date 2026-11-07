//! Write-through and backfill between SQLite and the Qdrant index.
//!
//! SQLite stays canonical: chat vectors live in `chat_embeddings` and owner
//! memory in `bot_self_memory`. This module only mirrors them into Qdrant, so
//! an absent service loses nothing and the next tick retries. Chat points are
//! pushed from the stored vectors (never re-embedded), while self facts are
//! embedded on write and re-embedded only when the point is missing.

use crate::chat_embeddings::{decode_vector, embed_texts, unit_vector};
use crate::qdrant::{self, Point, QdrantClient};
use crate::*;
use serde_json::json;

type ChatPointRow = (i64, i64, i64, String, i64, Option<i64>, String, Vec<u8>);

pub(crate) struct SelfFact {
    pub(crate) topic: String,
    pub(crate) fact: String,
    pub(crate) source_url: String,
}

fn model() -> String {
    config().memory.embedding_model.trim().to_owned()
}

fn self_point_id(topic: &str) -> u64 {
    qdrant::point_id(&format!("self:{topic}"))
}

fn self_filter(model: &str) -> Value {
    qdrant::filter(
        vec![
            qdrant::key_match("kind", json!("self")),
            qdrant::key_match("model", json!(model)),
        ],
        Vec::new(),
    )
}

async fn meta_i64(pool: &SqlitePool, key: &str) -> i64 {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM memory_meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    raw.and_then(|value| value.parse().ok()).unwrap_or(0)
}

async fn set_meta(pool: &SqlitePool, key: &str, value: i64) {
    let _ = sqlx::query(
        "INSERT INTO memory_meta (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value.to_string())
    .execute(pool)
    .await;
}

/// Mirrors stored chat vectors into Qdrant, bounded by
/// `memory.qdrant_sync_batches` batches per tick. A `memory_meta` cursor per
/// model keeps the sweep idempotent and lets a restart continue where the last
/// tick stopped instead of rescanning the whole log.
pub(crate) async fn sync_chat_points(
    pool: &SqlitePool,
    client: &QdrantClient,
) -> Result<usize, String> {
    let model = model();
    if model.is_empty() {
        return Ok(0);
    }
    let batch = config().memory.qdrant_sync_batch.max(1);
    let cursor_key = format!("qdrant_chat_cursor:{model}");
    let mut cursor = meta_i64(pool, &cursor_key).await;
    let dim = config().memory.embedding_dim;
    let mut synced = 0usize;
    for _ in 0..config().memory.qdrant_sync_batches.max(1) {
        let rows: Vec<ChatPointRow> = sqlx::query_as(
            "SELECT m.id, m.chat_id, \
                    CASE WHEN m.role = 'assistant' THEN 0 ELSE m.author_id END, \
                    m.author_name, m.created_at, m.message_id, e.chunk_text, e.vector \
             FROM chat_embeddings e JOIN chat_messages m ON m.id = e.message_id \
             WHERE e.model = ? AND m.flags = 0 AND m.id > ? \
             ORDER BY m.id LIMIT ?",
        )
        .bind(&model)
        .bind(cursor)
        .bind(batch as i64)
        .fetch_all(pool)
        .await
        .map_err(|_| "database".to_string())?;
        let Some(last) = rows.last().map(|row| row.0) else {
            break;
        };
        let scanned = rows.len();
        let mut points = Vec::with_capacity(scanned);
        for (id, chat_id, author_id, author_name, created_at, telegram_id, text, bytes) in rows {
            let Some(vector) = decode_vector(&bytes) else {
                continue;
            };
            points.push(Point {
                id: qdrant::point_id(&format!("chat:{chat_id}:{id}")),
                vector,
                payload: json!({
                    "kind": "chat",
                    "chat_id": chat_id,
                    "author_id": author_id,
                    "author_name": author_name,
                    "created_at": created_at,
                    "message_id": id,
                    "telegram_message_id": telegram_id,
                    "text": text,
                    "model": model,
                }),
            });
        }
        if !points.is_empty() {
            let dim = if dim > 0 {
                dim
            } else {
                points.first().map(|point| point.vector.len()).unwrap_or(0)
            };
            client.ensure(dim).await?;
            let ids: Vec<u64> = points.iter().map(|point| point.id).collect();
            let existing = client.existing(&ids).await?;
            let missing: Vec<Point> = points
                .into_iter()
                .filter(|point| !existing.contains(&point.id))
                .collect();
            synced += missing.len();
            client.upsert(&missing).await?;
        }
        cursor = last;
        set_meta(pool, &cursor_key, cursor).await;
        if scanned < batch {
            break;
        }
    }
    Ok(synced)
}

fn self_payload(topic: &str, fact: &str, source_url: &str, model: &str) -> Value {
    json!({
        "kind": "self",
        "topic": topic,
        "fact": fact,
        "source_url": source_url,
        "updated_at": crate::memory::unix_now(),
        "model": model,
    })
}

/// Writes one code-verified fact through to Qdrant. Owner directives
/// (`source_url` empty) are deliberately never indexed: they must stay fully
/// injected on every turn, and a relevance threshold must never drop them.
pub(crate) async fn index_self_fact(
    chat: &ChatConfig,
    client: &QdrantClient,
    topic: &str,
    fact: &str,
    source_url: &str,
) -> Result<(), String> {
    if source_url.trim().is_empty() {
        return Ok(());
    }
    let model = model();
    if model.is_empty() {
        return Ok(());
    }
    let text = crate::chat_text::truncate_text(
        &format!("{topic}: {fact}"),
        config().memory.embedding_max_chars,
    );
    let vector = embed_texts(chat, std::slice::from_ref(&text))
        .await?
        .into_iter()
        .next()
        .and_then(unit_vector)
        .ok_or_else(|| "empty embedding vector".to_string())?;
    client.ensure(vector.len()).await?;
    client
        .upsert(&[Point {
            id: self_point_id(topic),
            vector,
            payload: self_payload(topic, fact, source_url, &model),
        }])
        .await
}

pub(crate) async fn delete_self_points(
    client: &QdrantClient,
    topics: &[String],
) -> Result<(), String> {
    let ids: Vec<u64> = topics.iter().map(|topic| self_point_id(topic)).collect();
    client.delete(&ids).await
}

/// Reconciles Qdrant with `bot_self_memory`: embeds only missing facts, then
/// removes points whose topic no longer exists in SQLite.
pub(crate) async fn sync_self_points(
    chat: &ChatConfig,
    pool: &SqlitePool,
    client: &QdrantClient,
) -> Result<usize, String> {
    let model = model();
    if model.is_empty() {
        return Ok(0);
    }
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT topic, fact, source_url FROM bot_self_memory \
         WHERE trim(source_url) <> '' ORDER BY updated_at DESC, topic ASC LIMIT ?",
    )
    .bind(config().code.memory_max_rows)
    .fetch_all(pool)
    .await
    .map_err(|_| "database".to_string())?;
    let current: std::collections::HashSet<u64> = rows
        .iter()
        .map(|(topic, _, _)| self_point_id(topic))
        .collect();
    let dim = config().memory.embedding_dim;
    // A zero `embedding_dim` means "accept the provider's dimension", so the
    // collection size is learned from the first vector instead of the config.
    let mut ensured = dim > 0;
    if ensured {
        client.ensure(dim).await?;
    }
    let missing: Vec<&(String, String, String)> = if ensured {
        let ids: Vec<u64> = current.iter().copied().collect();
        let existing = client.existing(&ids).await?;
        rows.iter()
            .filter(|(topic, _, _)| !existing.contains(&self_point_id(topic)))
            .collect()
    } else {
        rows.iter().collect()
    };
    let batch = config().memory.embedding_batch.max(1);
    let mut synced = 0usize;
    for chunk in missing.chunks(batch) {
        let texts: Vec<String> = chunk
            .iter()
            .map(|(topic, fact, _)| {
                crate::chat_text::truncate_text(
                    &format!("{topic}: {fact}"),
                    config().memory.embedding_max_chars,
                )
            })
            .collect();
        let vectors = embed_texts(chat, &texts).await?;
        if vectors.len() != chunk.len() {
            return Err("embedding count mismatch".into());
        }
        let mut points = Vec::with_capacity(chunk.len());
        for ((topic, fact, source_url), vector) in chunk.iter().zip(vectors) {
            let Some(vector) = unit_vector(vector) else {
                continue;
            };
            points.push(Point {
                id: self_point_id(topic),
                vector,
                payload: self_payload(topic, fact, source_url, &model),
            });
        }
        if !ensured {
            if let Some(first) = points.first() {
                client.ensure(first.vector.len()).await?;
                ensured = true;
            }
        }
        client.upsert(&points).await?;
        synced += points.len();
    }
    if !ensured {
        return Ok(synced);
    }
    let stale: Vec<u64> = client
        .scroll(
            qdrant::filter(vec![qdrant::key_match("kind", json!("self"))], Vec::new()),
            512,
        )
        .await?
        .into_iter()
        .map(|hit| hit.id)
        .filter(|id| !current.contains(id))
        .collect();
    client.delete(&stale).await?;
    Ok(synced)
}

/// Top facts for one turn, ordered by semantic score. Returns an empty vector
/// when the index has no fact for the current model; callers then keep the
/// full SQLite injection so no fact is lost.
pub(crate) async fn search_self_facts(
    chat: &ChatConfig,
    client: &QdrantClient,
    query: &str,
    limit: usize,
) -> Result<Vec<SelfFact>, String> {
    let model = model();
    if model.is_empty() {
        return Ok(Vec::new());
    }
    let text = crate::chat_text::truncate_text(query, config().memory.embedding_max_chars);
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let vector = embed_texts(chat, std::slice::from_ref(&text))
        .await?
        .into_iter()
        .next()
        .and_then(unit_vector)
        .ok_or_else(|| "empty embedding vector".to_string())?;
    client.ensure(vector.len()).await?;
    let hits = client
        .query(&vector, self_filter(&model), limit, 0.0)
        .await?;
    Ok(hits
        .into_iter()
        .filter_map(|hit| {
            let topic = hit.payload.get("topic")?.as_str()?.to_owned();
            let fact = hit.payload.get("fact")?.as_str()?.to_owned();
            let source_url = hit
                .payload
                .get("source_url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            Some(SelfFact {
                topic,
                fact,
                source_url,
            })
        })
        .collect())
}
