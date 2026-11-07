//! Direct vector indexing between SQLite and the Qdrant vector store.
//!
//! SQLite holds the raw `chat_messages` log and `bot_self_memory` rows, while
//! all vectors live exclusively in Qdrant. `sync_chat_points` embeds unflagged
//! chat messages in bounded batches and upserts them directly into Qdrant,
//! advancing a per-model `memory_meta` cursor only after Qdrant acknowledges
//! the batch. Self facts are embedded on write and reconciled when missing or
//! stale.

use crate::chat_embeddings::{embed_texts, unit_vector};
use crate::qdrant::{self, Point, QdrantClient};
use crate::*;
use serde_json::json;

type ChatMessageRow = (i64, i64, i64, String, i64, Option<i64>, String);
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

fn self_kind_filter() -> Value {
    qdrant::filter(vec![qdrant::key_match("kind", json!("self"))], Vec::new())
}

/// Parses a stored cursor (`"<m.id>"`, or the legacy `"<at>:<m.id>"` shape).
fn parse_cursor_id(raw: &str) -> i64 {
    let tail = raw.rsplit_once(':').map(|(_, id)| id).unwrap_or(raw).trim();
    tail.parse::<i64>().unwrap_or(0)
}
fn cursor_key_for(client: Option<&QdrantClient>, model: &str) -> String {
    match client {
        Some(client) => format!("qdrant_chat_cursor:{}|{model}", client.identity()),
        None => format!("qdrant_chat_cursor:{model}"),
    }
}

pub(crate) async fn chat_cursor_id(
    pool: &SqlitePool,
    client: Option<&QdrantClient>,
    model: &str,
) -> i64 {
    let scoped = cursor_key_for(client, model);
    if let Some(raw) = meta_value(pool, &scoped).await {
        return parse_cursor_id(&raw);
    }
    let legacy = format!("qdrant_chat_cursor:{model}");
    parse_cursor_id(meta_value(pool, &legacy).await.as_deref().unwrap_or(""))
}

async fn meta_value(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM memory_meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

async fn set_meta(pool: &SqlitePool, key: &str, value: &str) {
    let _ = sqlx::query(
        "INSERT INTO memory_meta (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await;
}

/// Embeds unflagged chat messages and upserts their vectors directly into
/// Qdrant, bounded by `memory.embedding_passes` batches of
/// `memory.embedding_batch` rows per call. A per-model cursor in `memory_meta`
/// advances only after Qdrant acknowledges the upsert.
pub(crate) async fn sync_chat_points(
    chat: &ChatConfig,
    pool: &SqlitePool,
    client: &QdrantClient,
) -> Result<usize, String> {
    sync_chat_points_passes(chat, pool, client, config().memory.embedding_passes)
        .await
        .map_err(|(_, reason)| reason)
}

pub(crate) async fn sync_chat_points_passes(
    chat: &ChatConfig,
    pool: &SqlitePool,
    client: &QdrantClient,
    passes: usize,
) -> Result<usize, (usize, String)> {
    let model = model();
    if model.is_empty() {
        return Ok(0);
    }
    let batch = config().memory.embedding_batch.max(1);
    let passes = passes.max(1);
    let max_chars = config().memory.embedding_max_chars;
    let cursor_key = cursor_key_for(Some(client), &model);
    let mut cursor_id = chat_cursor_id(pool, Some(client), &model).await;
    let dim = config().memory.embedding_dim;
    if dim > 0 && client.ensure(dim).await.map_err(|e| (0, e))? {
        cursor_id = 0;
        set_meta(pool, &cursor_key, "0").await;
    }
    if cursor_id > 0 {
        let check_filter = qdrant::filter(
            vec![
                qdrant::key_match("kind", json!("chat")),
                qdrant::key_match("model", json!(model)),
            ],
            Vec::new(),
        );
        let missing_or_empty = match client.scroll(check_filter, 1).await {
            Ok(hits) => hits.is_empty(),
            Err(reason) if reason == "qdrant HTTP 404" => true,
            Err(reason) => return Err((0, reason)),
        };
        if missing_or_empty {
            cursor_id = 0;
            set_meta(pool, &cursor_key, "0").await;
        }
    }
    let mut synced = 0usize;
    let mut inferred_checked = dim > 0;
    for _ in 0..passes {
        let rows: Vec<ChatMessageRow> = sqlx::query_as(
            "SELECT m.id, m.chat_id, \
                    CASE WHEN m.role = 'assistant' THEN 0 ELSE m.author_id END, \
                    m.author_name, m.created_at, m.message_id, m.text \
             FROM chat_messages m \
             WHERE m.flags = 0 AND trim(m.text) <> '' AND m.id > ? \
             ORDER BY m.id LIMIT ?",
        )
        .bind(cursor_id)
        .bind(batch as i64)
        .fetch_all(pool)
        .await
        .map_err(|_| (synced, "database".to_string()))?;
        let Some(last_id) = rows.last().map(|row| row.0) else {
            break;
        };
        let scanned = rows.len();
        let mut truncated = Vec::with_capacity(scanned);
        let mut texts = Vec::with_capacity(scanned);
        for (id, chat_id, author_id, author_name, created_at, telegram_id, text) in rows {
            let chunk = crate::chat_text::truncate_text(&text, max_chars);
            texts.push(chunk.clone());
            truncated.push((
                id,
                chat_id,
                author_id,
                author_name,
                created_at,
                telegram_id,
                chunk,
            ));
        }
        let vectors = embed_texts(chat, &texts).await.map_err(|e| (synced, e))?;
        if vectors.len() != truncated.len() {
            return Err((synced, "embedding count mismatch".into()));
        }
        let mut points = Vec::with_capacity(scanned);
        for ((id, chat_id, author_id, author_name, created_at, telegram_id, text), vector) in
            truncated.into_iter().zip(vectors)
        {
            let vector =
                unit_vector(vector).ok_or_else(|| (synced, "empty embedding vector".into()))?;
            points.push(Point {
                id: qdrant::point_id(&format!("chat:{model}:{chat_id}:{id}")),
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
            if !inferred_checked {
                let inferred = points.first().map(|point| point.vector.len()).unwrap_or(0);
                let recreated = client.ensure(inferred).await.map_err(|e| (synced, e))?;
                inferred_checked = true;
                if recreated && cursor_id > 0 {
                    cursor_id = 0;
                    set_meta(pool, &cursor_key, "0").await;
                    continue;
                }
            }
            client.upsert(&points).await.map_err(|e| (synced, e))?;
            synced += points.len();
        }
        cursor_id = last_id;
        set_meta(pool, &cursor_key, &cursor_id.to_string()).await;
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
    let vector = embed_self_fact(chat, topic, fact).await?;
    let _ = client.ensure(vector.len()).await?;
    client
        .upsert(&[Point {
            id: self_point_id(topic),
            vector,
            payload: self_payload(topic, fact, source_url, &model),
        }])
        .await
}

async fn embed_self_fact(chat: &ChatConfig, topic: &str, fact: &str) -> Result<Vec<f32>, String> {
    let text = crate::chat_text::truncate_text(
        &format!("{topic}: {fact}"),
        config().memory.embedding_max_chars,
    );
    embed_texts(chat, std::slice::from_ref(&text))
        .await?
        .into_iter()
        .next()
        .and_then(unit_vector)
        .ok_or_else(|| "empty embedding vector".to_string())
}

pub(crate) async fn delete_self_points(
    client: &QdrantClient,
    topics: &[String],
) -> Result<(), String> {
    let ids: Vec<u64> = topics.iter().map(|topic| self_point_id(topic)).collect();
    client.delete(&ids).await
}

fn self_payload_matches(payload: &Value, model: &str, fact: &str, source_url: &str) -> bool {
    payload.get("model").and_then(Value::as_str) == Some(model)
        && payload.get("fact").and_then(Value::as_str) == Some(fact)
        && payload
            .get("source_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            == source_url
}

/// Reconciles Qdrant with `bot_self_memory`. One bounded scroll returns the
/// mirrored points with payload, so a fact whose text, source, or embedding
/// model changed is re-embedded even when its point id already exists. Points
/// whose topic disappeared from SQLite are deleted.
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
    let batch = config().memory.embedding_batch.max(1);
    let dim = config().memory.embedding_dim;
    // A zero `embedding_dim` means "accept the provider's dimension"; learn it
    // from the provider before touching a collection that may not exist yet.
    let mut precomputed: HashMap<String, Vec<f32>> = HashMap::new();
    if dim == 0 {
        for chunk in rows.chunks(batch) {
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
            for ((topic, _, _), vector) in chunk.iter().zip(vectors) {
                if let Some(vector) = unit_vector(vector) {
                    precomputed.insert(topic.clone(), vector);
                }
            }
        }
        match precomputed.values().next() {
            Some(first) => {
                let _ = client.ensure(first.len()).await?;
            }
            None => return Ok(0),
        }
    } else {
        let _ = client.ensure(dim).await?;
    }
    let remote: HashMap<u64, Value> = client
        .scroll(self_kind_filter(), 512)
        .await?
        .into_iter()
        .map(|hit| (hit.id, hit.payload))
        .collect();
    let current: std::collections::HashSet<u64> = rows
        .iter()
        .map(|(topic, _, _)| self_point_id(topic))
        .collect();
    let stale: Vec<u64> = remote
        .keys()
        .filter(|id| !current.contains(id))
        .copied()
        .collect();
    let missing: Vec<&(String, String, String)> = rows
        .iter()
        .filter(|(topic, fact, source_url)| {
            remote
                .get(&self_point_id(topic))
                .is_none_or(|payload| !self_payload_matches(payload, &model, fact, source_url))
        })
        .collect();
    let mut synced = 0usize;
    for chunk in missing.chunks(batch) {
        let vectors: Vec<Vec<f32>> = if dim == 0 {
            chunk
                .iter()
                .filter_map(|(topic, _, _)| precomputed.get(topic).cloned())
                .collect()
        } else {
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
            vectors.into_iter().filter_map(unit_vector).collect()
        };
        let mut points = Vec::with_capacity(vectors.len());
        for ((topic, fact, source_url), vector) in chunk.iter().zip(vectors) {
            points.push(Point {
                id: self_point_id(topic),
                vector,
                payload: self_payload(topic, fact, source_url, &model),
            });
        }
        client.upsert(&points).await?;
        synced += points.len();
    }
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
    let _ = client.ensure(vector.len()).await?;
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
