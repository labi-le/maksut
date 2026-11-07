use crate::chat_embeddings::index_pending;
use crate::chat_search::{parse_search_request, search_with};
use crate::qdrant::{point_id, QdrantClient};
use crate::test_support::*;
use crate::*;
use serde_json::json;
use std::time::Duration;

const DIM: usize = 1536;

fn axis(index: usize) -> Vec<f32> {
    let mut vector = vec![0.0f32; DIM];
    if let Some(slot) = vector.get_mut(index) {
        *slot = 1.0;
    }
    vector
}

fn embeddings_body(vectors: &[Vec<f32>]) -> String {
    let data: Vec<Value> = vectors
        .iter()
        .enumerate()
        .map(|(index, vector)| json!({"index": index, "embedding": vector}))
        .collect();
    json!({"data": data}).to_string()
}

fn chat_config(url: &Url) -> ChatConfig {
    ChatConfig::new(
        url.clone(),
        "key".into(),
        "test".into(),
        url.clone(),
        SearchProvider::DuckDuckGo,
        None,
    )
}

fn client(server: &MockServer) -> QdrantClient {
    QdrantClient::new(
        server.url.clone(),
        "test_memory".into(),
        Duration::from_secs(2),
        1024 * 1024,
    )
}

/// One collection create plus the seven payload indexes `ensure` sends.
fn ready_responses() -> Vec<String> {
    vec!["{}".to_string(); 8]
}

async fn recorded(pool: &SqlitePool, chat: i64, author: i64, name: &str, text: &str) {
    crate::memory::record_message(
        pool,
        RecordMessage::new(ChatId(chat), author, name, "user", text),
    )
    .await
    .unwrap();
}

#[test]
fn point_ids_are_stable_and_bounded() {
    let first = point_id("chat:1:2");
    assert_eq!(first, point_id("chat:1:2"));
    assert_ne!(first, point_id("chat:1:3"));
    assert_ne!(first, point_id("self:память"));
    assert!(first < 1u64 << 63);
}

#[tokio::test]
async fn chat_sync_uploads_stored_vectors_without_reembedding() {
    let (_dir, pool) = test_pool().await;
    recorded(&pool, 1, 10, "аня", "котики любят рыбу").await;
    recorded(&pool, 1, 11, "петя", "собаки любят мясо").await;
    let embed = mock_server(vec![embeddings_body(&[axis(0), axis(1)])]).await;
    let chat = chat_config(&embed.url);
    assert_eq!(index_pending(&chat, &pool, 10).await.unwrap(), 2);

    let qdrant =
        mock_server([ready_responses(), vec!["{}".to_string(), "{}".to_string()]].concat()).await;
    let synced = crate::qdrant_index::sync_chat_points(&pool, &client(&qdrant))
        .await
        .unwrap();
    assert_eq!(synced, 2);
    assert_eq!(embed.requests.lock().unwrap().len(), 1);

    {
        let requests = qdrant.requests.lock().unwrap();
        assert_eq!(requests.len(), 9);
        let create = body_of(&requests[0]);
        assert_eq!(create["vectors"]["size"], json!(DIM));
        let upsert = body_of(&requests[8]);
        let points = upsert["points"].as_array().unwrap();
        assert_eq!(points.len(), 2);
        assert_eq!(points[0]["payload"]["kind"], "chat");
        assert_eq!(points[0]["payload"]["chat_id"], 1);
        assert_eq!(points[0]["payload"]["text"], "котики любят рыбу");
    }
    let cursor: String =
        sqlx::query_scalar("SELECT value FROM memory_meta WHERE key LIKE 'qdrant_chat_cursor:%'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (cursor_at, cursor_id) = cursor.split_once(':').unwrap();
    assert!(cursor_at.parse::<i64>().unwrap() > 0);
    assert_eq!(cursor_id, "2");

    // Simulate a re-embedded row: the id stays, the index write time moves.
    sqlx::query("UPDATE chat_embeddings SET created_at = created_at + 1 WHERE message_id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let resynced = crate::qdrant_index::sync_chat_points(&pool, &client(&qdrant))
        .await
        .unwrap();
    assert_eq!(resynced, 1);
    {
        let requests = qdrant.requests.lock().unwrap();
        assert_eq!(requests.len(), 10);
        let upsert = body_of(&requests[9]);
        let points = upsert["points"].as_array().unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0]["payload"]["message_id"], 1);
    }
    embed.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn chat_search_answers_from_qdrant_when_indexed() {
    let (_dir, pool) = test_pool().await;
    recorded(&pool, 1, 10, "аня", "котики любят рыбу").await;
    let message_id: i64 = sqlx::query_scalar("SELECT id FROM chat_messages LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let embed = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let chat = chat_config(&embed.url);
    let qdrant = mock_server(
        [
            ready_responses(),
            vec![json!({
                "result": {"points": [{
                    "id": 1,
                    "score": 0.9,
                    "payload": {
                        "message_id": message_id,
                        "author_id": 10,
                        "author_name": "аня",
                        "created_at": 1_700_000_000,
                        "text": "котики любят рыбу",
                    },
                }]},
            })
            .to_string()],
        ]
        .concat(),
    )
    .await;
    let client = client(&qdrant);
    let request = parse_search_request(&json!({"query": "котики"})).unwrap();
    let output = search_with(&chat, &pool, ChatId(1), 0, &request, Some(&client))
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["results"][0]["text"], "котики любят рыбу");
    assert!(value.get("degraded").is_none(), "{value}");

    let requests = qdrant.requests.lock().unwrap();
    let query = body_of(&requests[8]);
    let must = query["filter"]["must"].as_array().unwrap();
    assert!(must
        .iter()
        .any(|clause| clause["key"] == "kind" && clause["match"]["value"] == "chat"));
    assert!(must
        .iter()
        .any(|clause| clause["key"] == "chat_id" && clause["match"]["value"] == 1));
    assert!(must.iter().any(|clause| clause["key"] == "model"));
    embed.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn chat_search_degrades_explicitly_when_qdrant_is_down() {
    let (_dir, pool) = test_pool().await;
    recorded(&pool, 1, 10, "аня", "котики любят рыбу").await;
    let embed = mock_server(vec![
        embeddings_body(&[axis(0)]),
        embeddings_body(&[axis(0)]),
    ])
    .await;
    let chat = chat_config(&embed.url);
    index_pending(&chat, &pool, 10).await.unwrap();
    let dead = QdrantClient::new(
        Url::parse("http://127.0.0.1:9/").unwrap(),
        "test_memory".into(),
        Duration::from_secs(1),
        1024 * 1024,
    );
    let request = parse_search_request(&json!({"query": "котики"})).unwrap();
    let output = search_with(&chat, &pool, ChatId(1), 0, &request, Some(&dead))
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["results"][0]["text"], "котики любят рыбу");
    let degraded = value["degraded"].as_str().unwrap();
    assert!(degraded.starts_with("qdrant:"), "{value}");
    embed.task.abort();
}

#[tokio::test]
async fn self_fact_upsert_and_delete_hit_the_same_point() {
    let embed = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let chat = chat_config(&embed.url);
    let qdrant =
        mock_server([ready_responses(), vec!["{}".to_string(), "{}".to_string()]].concat()).await;
    let client = client(&qdrant);
    crate::qdrant_index::index_self_fact(
        &chat,
        &client,
        "память",
        "хранит summary",
        "https://github.com/labi-le/maxwell/blob/main/src/memory.rs#L11",
    )
    .await
    .unwrap();
    crate::qdrant_index::delete_self_points(&client, &["память".to_string()])
        .await
        .unwrap();

    let requests = qdrant.requests.lock().unwrap();
    assert_eq!(requests.len(), 10);
    let upsert = body_of(&requests[8]);
    let point = &upsert["points"][0];
    assert_eq!(point["id"], json!(point_id("self:память")));
    assert_eq!(point["payload"]["kind"], "self");
    assert_eq!(point["payload"]["topic"], "память");
    assert_eq!(point["payload"]["model"], "text-embedding-3-small");
    let delete = body_of(&requests[9]);
    assert_eq!(delete["points"][0], json!(point_id("self:память")));
    embed.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn self_sync_embeds_only_missing_facts_and_drops_stale_points() {
    let (_dir, pool) = test_pool().await;
    let base = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(base.path().join("src")).unwrap();
    std::fs::write(base.path().join("src/memory.rs"), "// memory\n").unwrap();
    let offline = offline_chat();
    for (topic, fact) in [("память", "хранит summary"), ("поиск", "идёт через qdrant")]
    {
        crate::code_inspect::record_self_fact(
            &offline,
            &pool,
            base.path(),
            topic,
            fact,
            "https://github.com/labi-le/maxwell/blob/main/src/memory.rs#L1",
        )
        .await
        .unwrap();
    }

    let existing_id = point_id("self:память");
    let stale_id = point_id("self:удалено");
    let model = config().memory.embedding_model.trim().to_owned();
    let source = "https://github.com/labi-le/maxwell/blob/main/src/memory.rs#L1";
    let qdrant = mock_server(
        [
            ready_responses(),
            vec![
                json!({"result": {"points": [
                    {
                        "id": existing_id,
                        "payload": {
                            "topic": "память",
                            "fact": "хранит summary",
                            "source_url": source,
                            "model": model,
                        },
                    },
                    {"id": stale_id, "payload": {"topic": "удалено"}},
                ]}})
                .to_string(),
                "{}".to_string(),
                "{}".to_string(),
            ],
        ]
        .concat(),
    )
    .await;
    let embed = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let chat = chat_config(&embed.url);
    let synced = crate::qdrant_index::sync_self_points(&chat, &pool, &client(&qdrant))
        .await
        .unwrap();
    assert_eq!(synced, 1);
    let embed_requests = embed.requests.lock().unwrap();
    assert_eq!(embed_requests.len(), 1);
    let input = body_of(&embed_requests[0])["input"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(input.contains("поиск"), "{input}");

    let requests = qdrant.requests.lock().unwrap();
    assert_eq!(requests.len(), 11);
    let upsert = body_of(&requests[9]);
    assert_eq!(upsert["points"][0]["payload"]["topic"], "поиск");
    let delete = body_of(&requests[10]);
    assert_eq!(delete["points"][0], json!(stale_id));
    embed.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn self_memory_keeps_owner_directives_while_ranking_facts() {
    let (_dir, pool) = test_pool().await;
    let base = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(base.path().join("src")).unwrap();
    std::fs::write(base.path().join("src/memory.rs"), "// memory\n").unwrap();
    let offline = offline_chat();
    crate::code_inspect::record_self_fact(&offline, &pool, base.path(), "даня", "материть", "")
        .await
        .unwrap();
    crate::code_inspect::record_self_fact(
        &offline,
        &pool,
        base.path(),
        "память",
        "хранит summary",
        "https://github.com/labi-le/maxwell/blob/main/src/memory.rs#L1",
    )
    .await
    .unwrap();
    // More facts than `memory.self_fact_limit` (default 10), so the ranked path
    // runs instead of the full-injection shortcut.
    for index in 0..11 {
        crate::code_inspect::record_self_fact(
            &offline,
            &pool,
            base.path(),
            &format!("факт{index}"),
            "просто факт",
            "https://github.com/labi-le/maxwell/blob/main/src/memory.rs#L1",
        )
        .await
        .unwrap();
    }

    let qdrant = mock_server(
        [
            ready_responses(),
            vec![json!({
                "result": {"points": [{
                    "id": point_id("self:память"),
                    "score": 0.8,
                    "payload": {
                        "topic": "память",
                        "fact": "хранит summary",
                        "source_url": "https://github.com/labi-le/maxwell/blob/main/src/memory.rs#L1",
                    },
                }]},
            })
            .to_string()],
        ]
        .concat(),
    )
    .await;
    let embed = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let chat = chat_config(&embed.url);
    let memory = crate::self_memory::load_self_memory_with(
        &chat,
        &pool,
        "что там с памятью",
        Some(&client(&qdrant)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(memory.contains("- [owner] даня: материть"), "{memory}");
    assert!(memory.contains("память: хранит summary"), "{memory}");
    assert!(!memory.contains("факт0:"), "{memory}");
    assert_eq!(embed.requests.lock().unwrap().len(), 1);
    embed.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn self_memory_eviction_keeps_memory_max_rows() {
    let (_dir, pool) = test_pool().await;
    let offline = offline_chat();
    let base = tempfile::tempdir().unwrap();
    for index in 0..51 {
        crate::code_inspect::record_self_fact(
            &offline,
            &pool,
            base.path(),
            &format!("тема{index}"),
            "заметка",
            "",
        )
        .await
        .unwrap();
    }
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bot_self_memory")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, config().code.memory_max_rows);
}

#[tokio::test]
async fn self_sync_refreshes_changed_fact_payload() {
    let (_dir, pool) = test_pool().await;
    let base = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(base.path().join("src")).unwrap();
    std::fs::write(base.path().join("src/memory.rs"), "// memory\n").unwrap();
    let source = "https://github.com/labi-le/maxwell/blob/main/src/memory.rs#L1";
    let offline = offline_chat();
    crate::code_inspect::record_self_fact(
        &offline,
        &pool,
        base.path(),
        "память",
        "хранит summary",
        source,
    )
    .await
    .unwrap();
    let model = config().memory.embedding_model.trim().to_owned();
    let qdrant = mock_server(
        [
            ready_responses(),
            vec![
                json!({"result": {"points": [{
                    "id": point_id("self:память"),
                    "payload": {
                        "topic": "память",
                        "fact": "старый текст",
                        "source_url": source,
                        "model": model,
                    },
                }]}})
                .to_string(),
                "{}".to_string(),
            ],
        ]
        .concat(),
    )
    .await;
    let embed = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let chat = chat_config(&embed.url);
    let synced = crate::qdrant_index::sync_self_points(&chat, &pool, &client(&qdrant))
        .await
        .unwrap();
    assert_eq!(synced, 1);
    let requests = qdrant.requests.lock().unwrap();
    assert_eq!(requests.len(), 10);
    let upsert = body_of(&requests[9]);
    assert_eq!(upsert["points"][0]["payload"]["fact"], "хранит summary");
    embed.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn chat_search_rejects_stale_author_payload() {
    let (_dir, pool) = test_pool().await;
    recorded(&pool, 1, 10, "аня", "котики любят рыбу").await;
    crate::memory::record_message(
        &pool,
        RecordMessage::new(ChatId(1), 99, "бот", "assistant", "бот ответил про котиков"),
    )
    .await
    .unwrap();
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM chat_messages ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let (user_id, assistant_id) = (ids[0], ids[1]);
    let embed = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let chat = chat_config(&embed.url);
    let qdrant = mock_server(
        [
            ready_responses(),
            vec![json!({
                "result": {"points": [
                    {
                        "id": 1,
                        "score": 0.9,
                        "payload": {
                            "message_id": user_id,
                            "author_id": 10,
                            "author_name": "аня",
                            "created_at": 1_700_000_000,
                            "text": "котики любят рыбу",
                        },
                    },
                    {
                        "id": 2,
                        "score": 0.8,
                        "payload": {
                            "message_id": assistant_id,
                            "author_id": 10,
                            "author_name": "бот",
                            "created_at": 1_700_000_001,
                            "text": "бот ответил про котиков",
                        },
                    },
                ]},
            })
            .to_string()],
        ]
        .concat(),
    )
    .await;
    let request = parse_search_request(&json!({"query": "котики", "author_id": 10})).unwrap();
    let output = search_with(&chat, &pool, ChatId(1), 0, &request, Some(&client(&qdrant)))
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    let texts: Vec<&str> = value["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["котики любят рыбу"]);
    embed.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn chat_search_empty_result_is_not_degraded() {
    let (_dir, pool) = test_pool().await;
    recorded(&pool, 1, 10, "аня", "котики любят рыбу").await;
    let embed = mock_server(vec![
        embeddings_body(&[axis(0)]),
        embeddings_body(&[axis(1)]),
    ])
    .await;
    let chat = chat_config(&embed.url);
    index_pending(&chat, &pool, 10).await.unwrap();
    let qdrant = mock_server(
        [
            ready_responses(),
            vec![json!({"result": {"points": []}}).to_string()],
        ]
        .concat(),
    )
    .await;
    let request = parse_search_request(&json!({"query": "зебра"})).unwrap();
    let output = search_with(&chat, &pool, ChatId(1), 0, &request, Some(&client(&qdrant)))
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["results"].as_array().unwrap().len(), 0);
    assert!(value.get("degraded").is_none(), "{value}");
    embed.task.abort();
    qdrant.task.abort();
}
