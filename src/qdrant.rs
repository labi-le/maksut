//! Bounded REST client for the Qdrant vector index.
//!
//! Qdrant is a write-through index beside SQLite, never the source of truth:
//! the canonical rows stay in `chat_messages`, `chat_embeddings`, and
//! `bot_self_memory`. Every request here has a deadline and a response byte
//! cap, so an absent or slow service can only make retrieval fall back to
//! SQLite; it cannot stall a turn or grow memory without bound.
//!
//! The collection uses one unnamed cosine vector per point and payload
//! filtering only: indexes on `kind`, `chat_id`, `author_id`, `created_at`,
//! `model`, `topic`, and `source_url` are created on first use.

use crate::prelude::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

const PAYLOAD_INDEXES: [(&str, &str); 7] = [
    ("chat_id", "integer"),
    ("author_id", "integer"),
    ("created_at", "integer"),
    ("kind", "keyword"),
    ("model", "keyword"),
    ("topic", "keyword"),
    ("source_url", "keyword"),
];

static READY: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

static CLIENT: LazyLock<reqwest::Client> =
    LazyLock::new(|| reqwest::Client::builder().build().unwrap_or_default());

fn http_client() -> &'static reqwest::Client {
    &CLIENT
}

#[derive(Clone, Debug)]
pub(crate) struct QdrantClient {
    http: &'static reqwest::Client,
    base: Url,
    collection: String,
    timeout: Duration,
    max_bytes: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Point {
    pub(crate) id: u64,
    pub(crate) vector: Vec<f32>,
    pub(crate) payload: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct Hit {
    pub(crate) id: u64,
    pub(crate) score: f32,
    pub(crate) payload: Value,
}

/// Stable 63-bit point id from a domain key. Qdrant accepts unsigned integers;
/// keeping the high bit clear avoids any client that reads ids as `i64`.
pub(crate) fn point_id(key: &str) -> u64 {
    let digest = Sha256::digest(key.as_bytes());
    let mut id = 0u64;
    for byte in digest.iter().take(8) {
        id = (id << 8) | u64::from(*byte);
    }
    id >> 1
}

pub(crate) fn key_match(key: &str, value: Value) -> Value {
    json!({"key": key, "match": {"value": value}})
}

pub(crate) fn key_range(key: &str, gte: Option<i64>, lt: Option<i64>) -> Value {
    let mut range = serde_json::Map::new();
    if let Some(value) = gte {
        range.insert("gte".to_owned(), json!(value));
    }
    if let Some(value) = lt {
        range.insert("lt".to_owned(), json!(value));
    }
    json!({"key": key, "range": Value::Object(range)})
}

pub(crate) fn filter(must: Vec<Value>, must_not: Vec<Value>) -> Value {
    let mut clauses = serde_json::Map::new();
    if !must.is_empty() {
        clauses.insert("must".to_owned(), Value::Array(must));
    }
    if !must_not.is_empty() {
        clauses.insert("must_not".to_owned(), Value::Array(must_not));
    }
    Value::Object(clauses)
}

fn id_of(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|raw| u64::try_from(raw).ok()))
}

impl QdrantClient {
    /// The configured client, or `None` when `memory.qdrant_url` is empty.
    pub(crate) fn from_config() -> Option<Self> {
        let raw = config().memory.qdrant_url.trim();
        if raw.is_empty() {
            return None;
        }
        let base = Url::parse(raw).ok()?;
        Some(Self::new(
            base,
            config().memory.qdrant_collection.clone(),
            Duration::from_secs(config().memory.qdrant_timeout_secs.max(1)),
            config().memory.qdrant_max_response_bytes.max(64 * 1024),
        ))
    }

    pub(crate) fn new(base: Url, collection: String, timeout: Duration, max_bytes: usize) -> Self {
        Self {
            http: http_client(),
            base,
            collection,
            timeout,
            max_bytes,
        }
    }

    fn ready_key(&self) -> String {
        format!("{}|{}", self.base, self.collection)
    }

    fn url(&self, path: &str) -> Result<Url, String> {
        let mut url = self.base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{prefix}{path}"));
        url.set_query(None);
        url.set_fragment(None);
        Ok(url)
    }

    async fn read_bounded(&self, mut response: reqwest::Response) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        loop {
            let next = time::timeout(self.timeout, response.chunk())
                .await
                .map_err(|_| "qdrant timeout".to_string())?
                .map_err(|_| "qdrant transport".to_string())?;
            let Some(chunk) = next else {
                break;
            };
            if body.len().saturating_add(chunk.len()) > self.max_bytes {
                return Err("qdrant response too large".into());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        accept: &[u16],
        wait: bool,
    ) -> Result<Value, String> {
        let mut url = self.url(path)?;
        if wait {
            // Acknowledge only after the operation is applied: the SQLite
            // cursor must never advance past a point the server has not stored.
            url.set_query(Some("wait=true"));
        }
        let mut request = self.http.request(method, url).timeout(self.timeout);
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let response = time::timeout(self.timeout, request.send())
            .await
            .map_err(|_| "qdrant timeout".to_string())?
            .map_err(|_| "qdrant transport".to_string())?;
        let status = response.status().as_u16();
        let bytes = self.read_bounded(response).await?;
        if !accept.contains(&status) {
            return Err(format!("qdrant HTTP {status}"));
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| "qdrant json".to_string())
    }

    /// Creates the collection and its payload indexes once per process. An
    /// existing collection and an existing index are accepted, so a restart
    /// converges without dropping data.
    pub(crate) async fn ensure(&self, dim: usize) -> Result<(), String> {
        if dim == 0 || dim > 8192 {
            return Err("qdrant dimension".into());
        }
        let key = self.ready_key();
        if READY
            .lock()
            .map(|ready| ready.contains(&key))
            .unwrap_or(false)
        {
            return Ok(());
        }
        let create = json!({
            "vectors": {"size": dim, "distance": "Cosine"},
            "on_disk_payload": true,
        });
        self.send(
            reqwest::Method::PUT,
            &format!("/collections/{}", self.collection),
            Some(create),
            &[200, 400, 409],
            true,
        )
        .await?;
        for (field, schema) in PAYLOAD_INDEXES {
            let body = json!({"field_name": field, "field_schema": schema});
            self.send(
                reqwest::Method::PUT,
                &format!("/collections/{}/index", self.collection),
                Some(body),
                &[200, 400, 409],
                true,
            )
            .await?;
        }
        if let Ok(mut ready) = READY.lock() {
            ready.insert(key);
        }
        Ok(())
    }

    pub(crate) async fn upsert(&self, points: &[Point]) -> Result<(), String> {
        if points.is_empty() {
            return Ok(());
        }
        let body = json!({
            "points": points
                .iter()
                .map(|point| json!({
                    "id": point.id,
                    "vector": point.vector,
                    "payload": point.payload,
                }))
                .collect::<Vec<_>>(),
        });
        self.send(
            reqwest::Method::PUT,
            &format!("/collections/{}/points", self.collection),
            Some(body),
            &[200],
            true,
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn query(
        &self,
        vector: &[f32],
        filter: Value,
        limit: usize,
        min_score: f32,
    ) -> Result<Vec<Hit>, String> {
        let body = json!({
            "query": vector,
            "filter": filter,
            "limit": limit.max(1),
            "with_payload": true,
            "score_threshold": min_score,
        });
        let value = self
            .send(
                reqwest::Method::POST,
                &format!("/collections/{}/points/query", self.collection),
                Some(body),
                &[200],
                false,
            )
            .await?;
        let points = value
            .get("result")
            .and_then(|result| result.get("points"))
            .and_then(Value::as_array)
            .ok_or_else(|| "qdrant query shape".to_string())?;
        Ok(parse_hits(points, limit))
    }

    pub(crate) async fn delete(&self, ids: &[u64]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let body = json!({"points": ids});
        self.send(
            reqwest::Method::POST,
            &format!("/collections/{}/points/delete", self.collection),
            Some(body),
            &[200],
            true,
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn scroll(&self, filter: Value, limit: usize) -> Result<Vec<Hit>, String> {
        let body = json!({
            "filter": filter,
            "limit": limit.max(1),
            "with_payload": true,
            "with_vector": false,
        });
        let value = self
            .send(
                reqwest::Method::POST,
                &format!("/collections/{}/points/scroll", self.collection),
                Some(body),
                &[200],
                false,
            )
            .await?;
        let points = value
            .get("result")
            .and_then(|result| result.get("points"))
            .and_then(Value::as_array)
            .ok_or_else(|| "qdrant scroll shape".to_string())?;
        Ok(parse_hits(points, limit))
    }
}

fn parse_hits(points: &[Value], limit: usize) -> Vec<Hit> {
    let mut hits = Vec::with_capacity(points.len().min(limit));
    for point in points {
        let Some(id) = point.get("id").and_then(id_of) else {
            continue;
        };
        let score = point
            .get("score")
            .and_then(Value::as_f64)
            .map(|score| score as f32)
            .unwrap_or(0.0);
        let payload = point.get("payload").cloned().unwrap_or(Value::Null);
        hits.push(Hit { id, score, payload });
    }
    hits
}
