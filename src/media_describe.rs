use crate::chat_embeddings::{embed_texts, unit_vector};
use crate::qdrant::{self, Point, QdrantClient};
use crate::*;
use serde_json::json;

static DESCRIBE_AUTH_FAILED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static DESCRIBE_CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    let cfg = config();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(cfg.telegram.connect_timeout_secs))
        .timeout(Duration::from_secs(cfg.garage.timeout_secs.max(1)))
        .pool_idle_timeout(Duration::from_secs(cfg.telegram.pool_idle_timeout_secs))
        .tcp_keepalive(Duration::from_secs(cfg.telegram.tcp_keepalive_secs))
        .tcp_nodelay(true)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

pub(crate) fn enabled() -> bool {
    !config().garage.describe_model.trim().is_empty()
        && !DESCRIBE_AUTH_FAILED.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn is_describable_image(kind: &str, content_type: &str, bytes_len: usize) -> bool {
    let cap = usize::try_from(config().chat.image_max_bytes).unwrap_or(usize::MAX);
    if bytes_len == 0 || bytes_len > cap {
        return false;
    }
    match kind {
        "photo" | "sticker" => true,
        "document" => {
            let essence = content_type
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            matches!(
                essence.as_str(),
                "image/jpeg" | "image/jpg" | "image/png" | "image/webp" | "image/gif"
            )
        }
        _ => false,
    }
}

pub(crate) async fn ensure_described(
    chat: &ChatConfig,
    pool: &SqlitePool,
    storage_key: &str,
    content_type: &str,
    bytes: &[u8],
    usage_site: Option<&UsageSite>,
) {
    if !enabled() {
        return;
    }
    if crate::media_store::blob_description(pool, storage_key)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    let Some((description, usage)) =
        describe_bytes(bytes, content_type, &chat.api_key, &chat.base_url).await
    else {
        return;
    };
    let model = config().garage.describe_model.trim();
    if let Some(site) = usage_site {
        site.record(pool, model, KIND_MEDIA_DESCRIBE, usage).await;
    }
    let flags = crate::memory_guard::detect_flags(&description);
    if flags & (crate::memory::FLAG_SECRET | crate::memory::FLAG_INJECTION) != 0 {
        tracing::warn!(stage = "describe_flags", "chat media description withheld");
        return;
    }
    let _ = crate::media_store::set_blob_description(
        pool,
        storage_key,
        &description,
        crate::memory::unix_now(),
    )
    .await;
    if let Some(client) = QdrantClient::from_config() {
        let _ = sync_media_points(chat, pool, &client).await;
    }
}

pub(crate) async fn describe_bytes(
    bytes: &[u8],
    content_type: &str,
    api_key: &str,
    base_url: &Url,
) -> Option<(String, Option<Usage>)> {
    let settings = &config().garage;
    let model = settings.describe_model.trim();
    if model.is_empty() || bytes.is_empty() {
        return None;
    }
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("image/jpeg")
        .trim();
    let mime = if mime.starts_with("image/") {
        mime
    } else {
        "image/jpeg"
    };
    let mut endpoint = base_url.clone();
    endpoint.set_path(&format!(
        "{}/chat/completions",
        endpoint.path().trim_end_matches('/')
    ));
    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": MEDIA_DESCRIBE_PROMPT},
            {"role": "user", "content": [{
                "type": "image_url",
                "image_url": {"url": ""}
            }]}
        ],
        "max_tokens": 200,
        "temperature": 0
    });
    *body.pointer_mut("/messages/1/content/0/image_url/url")? =
        Value::String(format!("data:{mime};base64,{}", base64_encode(bytes)));
    let timeout = Duration::from_secs(settings.timeout_secs.max(1));
    let request = DESCRIBE_CLIENT
        .post(endpoint)
        .bearer_auth(api_key)
        .header("content-type", "application/json")
        .timeout(timeout)
        .body(body.to_string());
    let response = time::timeout(timeout, request.send()).await.ok()?.ok()?;
    let code = response.status().as_u16();
    if !response.status().is_success() {
        let local = base_url
            .host_str()
            .is_some_and(|h| matches!(h, "127.0.0.1" | "localhost" | "::1" | "[::1]"));
        if matches!(code, 400 | 401 | 403 | 404) && !local {
            DESCRIBE_AUTH_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        tracing::warn!(status = code, "chat media description HTTP error");
        return None;
    }
    let max_resp = config().llm.max_response_bytes.max(1024);
    let mut buf = Vec::new();
    let mut resp = response;
    while let Some(chunk) = time::timeout(timeout, resp.chunk()).await.ok()?.ok()? {
        if buf.len().saturating_add(chunk.len()) > max_resp {
            return None;
        }
        buf.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&buf).ok()?;
    let usage = Usage::from_response(&value);
    let raw = crate::voice::transcript_from_response(&value)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let clamped = crate::chat_text::truncate_text(trimmed, settings.describe_max_chars);
    Some((clamped, usage))
}

fn media_cursor_key(client: Option<&QdrantClient>, model: &str) -> String {
    match client {
        Some(client) => format!("qdrant_media_cursor:{}|{model}", client.identity()),
        None => format!("qdrant_media_cursor:{model}"),
    }
}

pub(crate) async fn media_cursor_id(
    pool: &SqlitePool,
    client: Option<&QdrantClient>,
    model: &str,
) -> i64 {
    let key = media_cursor_key(client, model);
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM memory_meta WHERE key = ?")
        .bind(&key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    raw.as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

async fn set_media_cursor(pool: &SqlitePool, key: &str, id: i64) {
    let _ = sqlx::query(
        "INSERT INTO memory_meta (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(id.to_string())
    .execute(pool)
    .await;
}

type MediaIndexRow = (
    i64,
    i64,
    i64,
    String,
    i64,
    i64,
    String,
    String,
    String,
    String,
);

pub(crate) async fn sync_media_points(
    chat: &ChatConfig,
    pool: &SqlitePool,
    client: &QdrantClient,
) -> Result<usize, String> {
    let model = config().memory.embedding_model.trim().to_owned();
    if model.is_empty() {
        return Ok(0);
    }
    let batch = config().memory.embedding_batch.max(1);
    let passes = config().memory.embedding_passes.max(1);
    let max_chars = config().memory.embedding_max_chars;
    let cursor_key = media_cursor_key(Some(client), &model);
    let mut cursor_id = media_cursor_id(pool, Some(client), &model).await;
    let dim = config().memory.embedding_dim;
    if dim > 0 && client.ensure(dim).await? {
        cursor_id = 0;
        set_media_cursor(pool, &cursor_key, 0).await;
    }
    if cursor_id > 0 {
        let check_filter = qdrant::filter(
            vec![
                qdrant::key_match("kind", json!("media")),
                qdrant::key_match("model", json!(model)),
            ],
            Vec::new(),
        );
        let missing_or_empty = match client.scroll(check_filter, 1).await {
            Ok(hits) => hits.is_empty(),
            Err(reason) if reason == "qdrant HTTP 404" => true,
            Err(reason) => return Err(reason),
        };
        if missing_or_empty {
            cursor_id = 0;
            set_media_cursor(pool, &cursor_key, 0).await;
        }
    }
    let mut synced = 0usize;
    let mut inferred_checked = dim > 0;
    for _ in 0..passes {
        let rows: Vec<MediaIndexRow> = sqlx::query_as(
            "SELECT m.id, m.chat_id, \
                    CASE WHEN m.role = 'assistant' THEN 0 ELSE m.author_id END, \
                    m.author_name, m.created_at, c.message_id, m.text, \
                    c.kind, c.storage_key, COALESCE(b.description, '') \
             FROM chat_messages m \
             JOIN chat_media c ON c.chat_id = m.chat_id AND c.message_id = m.message_id \
             LEFT JOIN chat_media_blobs b ON b.sha256 = c.storage_key \
             WHERE (m.flags & ~4) = 0 AND m.id > ? \
               AND (trim(COALESCE(b.description, '')) <> '' \
                    OR (trim(m.text) <> '' AND trim(m.text) <> '[вложение]')) \
             ORDER BY m.id LIMIT ?",
        )
        .bind(cursor_id)
        .bind(batch as i64)
        .fetch_all(pool)
        .await
        .map_err(|_| "database".to_string())?;
        let Some(last_id) = rows.last().map(|r| r.0) else {
            break;
        };
        let scanned = rows.len();
        let mut items = Vec::with_capacity(scanned);
        let mut texts = Vec::with_capacity(scanned);
        for (id, chat_id, author_id, author_name, created_at, tg_id, msg_text, mkind, skey, desc) in
            rows
        {
            let combined = compose_media_search_text(&mkind, &msg_text, &desc);
            let chunk = crate::chat_text::truncate_text(&combined, max_chars);
            texts.push(chunk.clone());
            items.push((
                id,
                chat_id,
                author_id,
                author_name,
                created_at,
                tg_id,
                mkind,
                skey,
                chunk,
            ));
        }
        let vectors = embed_texts(chat, &texts).await?;
        if vectors.len() != items.len() {
            return Err("embedding count mismatch".into());
        }
        let mut points = Vec::with_capacity(scanned);
        for ((id, chat_id, author_id, author_name, created_at, tg_id, mkind, skey, text), vector) in
            items.into_iter().zip(vectors)
        {
            let vector = unit_vector(vector).ok_or_else(|| "empty embedding vector".to_string())?;
            points.push(Point {
                id: qdrant::point_id(&format!("media:{model}:{chat_id}:{id}")),
                vector,
                payload: json!({
                    "kind": "media",
                    "chat_id": chat_id,
                    "author_id": author_id,
                    "author_name": author_name,
                    "created_at": created_at,
                    "message_id": id,
                    "telegram_message_id": tg_id,
                    "media_kind": mkind,
                    "storage_key": skey,
                    "text": text,
                    "model": model,
                }),
            });
        }
        if !points.is_empty() {
            if !inferred_checked {
                let inferred = points.first().map(|p| p.vector.len()).unwrap_or(0);
                let _ = client.ensure(inferred).await?;
                inferred_checked = true;
            }
            client.upsert(&points).await?;
            synced += points.len();
        }
        cursor_id = last_id;
        set_media_cursor(pool, &cursor_key, cursor_id).await;
        if scanned < batch {
            break;
        }
    }
    Ok(synced)
}

pub(crate) fn compose_media_search_text(
    media_kind: &str,
    message_text: &str,
    description: &str,
) -> String {
    let caption = message_text.trim();
    let caption = if caption == "[вложение]" {
        ""
    } else {
        caption
    };
    let desc = description.trim();
    match (!caption.is_empty(), !desc.is_empty()) {
        (true, true) => format!("{caption} [{media_kind}: {desc}]"),
        (false, true) => format!("[{media_kind}: {desc}]"),
        (true, false) => format!("{caption} [{media_kind}]"),
        (false, false) => format!("[{media_kind}]"),
    }
}
