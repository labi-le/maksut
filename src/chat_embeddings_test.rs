use crate::chat_embeddings::{coverage, embed_texts, index_pending, reindex_with};
use crate::chat_search::{parse_search_request, search, search_with, SearchRequest};
use crate::qdrant::QdrantClient;
use crate::test_support::*;
use crate::*;
use serde_json::json;
use std::time::Duration;

const DIM: usize = 384;
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

async fn chat_config(url: &Url) -> ChatConfig {
    ChatConfig::new(
        url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        url.clone(),
        SearchProvider::DuckDuckGo,
        None,
    )
}

fn qdrant_client(server: &MockServer) -> QdrantClient {
    QdrantClient::new(
        server.url.clone(),
        "test_memory".into(),
        Duration::from_secs(2),
        1024 * 1024,
    )
}

fn ready_responses() -> Vec<String> {
    vec!["{}".to_string(); 8]
}

async fn recorded(pool: &SqlitePool, chat: i64, author: i64, name: &str, text: &str, flags: i64) {
    crate::memory::record_message(
        pool,
        RecordMessage::new(ChatId(chat), author, name, "user", text).with_flags(flags),
    )
    .await
    .unwrap();
}

fn request(args: Value) -> SearchRequest {
    parse_search_request(&args).unwrap()
}

#[tokio::test]
async fn embeddings_retry_a_transient_status() {
    let server =
        mock_server_with_statuses(vec![(503, "{}".into()), (200, embeddings_body(&[axis(0)]))])
            .await;
    let chat = chat_config(&server.url).await;
    let vectors = embed_texts(&chat, &["котики".to_string()]).await.unwrap();
    assert_eq!(vectors.len(), 1);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    server.task.abort();
}

#[tokio::test]
async fn embeddings_do_not_retry_a_rejected_request() {
    let server = mock_server_with_statuses(vec![(400, "{}".into())]).await;
    let chat = chat_config(&server.url).await;
    assert!(embed_texts(&chat, &["котики".to_string()]).await.is_err());
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    server.task.abort();
}

#[tokio::test]
async fn index_writes_vectors_for_unflagged_messages_only() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![embeddings_body(&[axis(0), axis(1)])]).await;
    let qdrant = mock_server([ready_responses(), vec!["{}".to_string()]].concat()).await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "котики любят рыбу", 0).await;
    recorded(
        &pool,
        1,
        11,
        "петя",
        "секрет token=abcdef",
        crate::memory::FLAG_SECRET,
    )
    .await;
    recorded(&pool, 1, 12, "вася", "собаки любят мясо", 0).await;

    let indexed = index_pending(&chat, &pool, &qdrant_client(&qdrant))
        .await
        .unwrap();
    assert_eq!(indexed, 2);
    let model = config().memory.embedding_model.trim().to_owned();
    assert_eq!(
        coverage(&pool, Some(&qdrant_client(&qdrant)), &model)
            .await
            .unwrap(),
        (2, 0)
    );

    let body = body_of(&server.requests.lock().unwrap()[0]);
    assert_eq!(body["model"], json!(model));
    let inputs = body["input"].as_array().unwrap();
    assert_eq!(inputs.len(), 2);
    assert!(!format!("{inputs:?}").contains("секрет"));
    server.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn search_scopes_to_one_chat_and_drops_low_scores() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "котики любят рыбу", 0).await;
    recorded(&pool, 1, 11, "петя", "погода в москве", 0).await;
    recorded(&pool, 2, 10, "аня", "котики в чужом чате", 0).await;
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM chat_messages ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let qdrant = mock_server(
        [
            ready_responses(),
            vec![json!({
                "result": {"points": [
                    {
                        "id": 1,
                        "score": 0.9,
                        "payload": {
                            "message_id": ids[0],
                            "author_id": 10,
                            "author_name": "аня",
                            "created_at": 1_700_000_000,
                            "text": "котики любят рыбу",
                        },
                    },
                    {
                        "id": 2,
                        "score": 0.1,
                        "payload": {
                            "message_id": ids[1],
                            "author_id": 11,
                            "author_name": "петя",
                            "created_at": 1_700_000_001,
                            "text": "погода в москве",
                        },
                    },
                    {
                        "id": 3,
                        "score": 0.95,
                        "payload": {
                            "message_id": ids[2],
                            "author_id": 10,
                            "author_name": "аня",
                            "created_at": 1_700_000_002,
                            "text": "котики в чужом чате",
                        },
                    },
                ]},
            })
            .to_string()],
        ]
        .concat(),
    )
    .await;
    let client = qdrant_client(&qdrant);
    let output = search_with(
        &chat,
        &pool,
        ChatId(1),
        0,
        &request(json!({"query": "котики"})),
        Some(&client),
    )
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
    server.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn search_merges_recent_messages_and_dedupes() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "старая реплика про котиков", 0).await;
    recorded(&pool, 1, 11, "петя", "новая реплика", 0).await;
    let first_id: i64 = sqlx::query_scalar("SELECT id FROM chat_messages ORDER BY id LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let qdrant = mock_server(
        [
            ready_responses(),
            vec![json!({
                "result": {"points": [{
                    "id": 1,
                    "score": 0.9,
                    "payload": {
                        "message_id": first_id,
                        "author_id": 10,
                        "author_name": "аня",
                        "created_at": 1_700_000_000,
                        "text": "старая реплика про котиков",
                    },
                }]},
            })
            .to_string()],
        ]
        .concat(),
    )
    .await;
    let client = qdrant_client(&qdrant);
    let output = search_with(
        &chat,
        &pool,
        ChatId(1),
        0,
        &request(json!({"query": "котики"})),
        Some(&client),
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["results"].as_array().unwrap().len(), 1);
    let recent: Vec<&str> = value["recent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["text"].as_str().unwrap())
        .collect();
    assert_eq!(recent, vec!["новая реплика"]);
    server.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn chat_search_tool_scopes_to_the_current_chat() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![]).await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "котики любят рыбу", 0).await;
    recorded(&pool, 2, 10, "аня", "котики в чужом чате", 0).await;

    let stickers = StickerReplies::for_tests();
    let catalog = StickerCatalog::for_tests(&[]);
    let ctx = ToolContext {
        chat: &chat,
        pool: &pool,
        stickers: &stickers,
        catalog: &catalog,
        proposal: None,
        owner: false,
        chat_id: ChatId(1),
        user_id: UserId(10),
        sender_name: "аня",
        username: None,
        message_id: 0,
        reply_user_id: None,
        bot: None,
        read_url_targets: &[],
    };
    let mut state = ToolState::new(false, false);
    let call = json!({
        "function": {
            "name": "chat_search",
            "arguments": json!({"query": "котики", "chat_id": 2}).to_string(),
        },
    });
    let output = run_tool_call(&call, &ctx, &mut state).await;
    let value: Value = serde_json::from_str(&output).unwrap();
    let texts: Vec<&str> = value["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["котики любят рыбу"]);
    assert_eq!(state.chat_search_calls, 1);
    server.task.abort();
}

#[tokio::test]
async fn search_never_returns_flagged_rows() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let chat = chat_config(&server.url).await;
    recorded(
        &pool,
        1,
        10,
        "аня",
        "секрет token=abcdef",
        crate::memory::FLAG_SECRET,
    )
    .await;
    let message_id: i64 = sqlx::query_scalar("SELECT id FROM chat_messages LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let qdrant = mock_server(
        [
            ready_responses(),
            vec![json!({
                "result": {"points": [{
                    "id": 1,
                    "score": 0.99,
                    "payload": {
                        "message_id": message_id,
                        "author_id": 10,
                        "author_name": "аня",
                        "created_at": 1_700_000_000,
                        "text": "секрет token=abcdef",
                    },
                }]},
            })
            .to_string()],
        ]
        .concat(),
    )
    .await;
    let client = qdrant_client(&qdrant);

    let output = search_with(
        &chat,
        &pool,
        ChatId(1),
        0,
        &request(json!({"query": "секрет"})),
        Some(&client),
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert!(value["results"].as_array().unwrap().is_empty());
    assert!(value["recent"].as_array().unwrap().is_empty());
    server.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn search_filters_by_author_and_date_window() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![]).await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "реплика номер один", 0).await;
    recorded(&pool, 1, 11, "петя", "реплика номер два", 0).await;
    sqlx::query("UPDATE chat_messages SET created_at = ?")
        .bind(1_780_000_000_i64)
        .execute(&pool)
        .await
        .unwrap();

    let by_author = search(
        &chat,
        &pool,
        ChatId(1),
        0,
        &request(json!({"query": "реплика", "author_id": 11})),
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_str(&by_author).unwrap();
    let texts: Vec<&str> = value["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["реплика номер два"]);

    let inside = search(
        &chat,
        &pool,
        ChatId(1),
        0,
        &request(json!({"query": "реплика", "since": "2026-05-01", "until": "2026-07-01"})),
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_str(&inside).unwrap();
    let mut texts: Vec<&str> = value["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["text"].as_str().unwrap())
        .collect();
    texts.sort_unstable();
    assert_eq!(texts, vec!["реплика номер два", "реплика номер один"]);

    let outside = search(
        &chat,
        &pool,
        ChatId(1),
        0,
        &request(json!({"query": "реплика", "since": "2026-07-01"})),
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_str(&outside).unwrap();
    assert!(value["results"].as_array().unwrap().is_empty());
    server.task.abort();
}

#[tokio::test]
async fn index_reembeds_rows_left_by_another_model() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let qdrant = mock_server([ready_responses(), vec!["{}".to_string()]].concat()).await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "котики", 0).await;
    sqlx::query(
        "INSERT INTO memory_meta (key, value) VALUES ('qdrant_chat_cursor:old-model', '1')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let model = config().memory.embedding_model.trim().to_owned();
    assert_eq!(coverage(&pool, None, &model).await.unwrap(), (0, 1));
    assert_eq!(
        index_pending(&chat, &pool, &qdrant_client(&qdrant))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        coverage(&pool, Some(&qdrant_client(&qdrant)), &model)
            .await
            .unwrap(),
        (1, 0)
    );
    server.task.abort();
    qdrant.task.abort();
}

#[tokio::test]
async fn reindex_reports_coverage() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let qdrant = mock_server([ready_responses(), vec!["{}".to_string()]].concat()).await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "котики", 0).await;

    let report: Value =
        serde_json::from_str(&reindex_with(&chat, &pool, &qdrant_client(&qdrant), 2).await)
            .unwrap();
    assert_eq!(report["indexed_now"], json!(1));
    assert_eq!(report["indexed"], json!(1));
    assert_eq!(report["pending"], json!(0));
    server.task.abort();
    qdrant.task.abort();
}
#[test]
fn search_request_rejects_bad_arguments_and_bounds_limit() {
    assert!(parse_search_request(&json!({"query": "  "})).is_err());
    assert!(parse_search_request(&json!({"query": "x", "author_id": 0})).is_err());
    assert!(parse_search_request(&json!({"query": "x", "since": "2026-13-01"})).is_err());
    assert!(parse_search_request(&json!({"query": "x", "until": "not-a-date"})).is_err());
    assert!(parse_search_request(&json!({
        "query": "x", "since": "2026-05-01", "until": "2026-05-01"
    }))
    .is_err());
    let limit = config().memory.search_limit;
    assert_eq!(
        parse_search_request(&json!({"query": "x", "limit": 999}))
            .unwrap()
            .limit,
        limit
    );
}

#[tokio::test]
async fn search_falls_back_to_lexical_matching_when_embeddings_return_403() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server_with_statuses(vec![(403, "{}".into())]).await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "Денис", "какой самый лучший пылесос", 0).await;
    crate::memory::record_message(
        &pool,
        RecordMessage::new(
            ChatId(1),
            10,
            "максут",
            "assistant",
            "бери Roborock Qrevo Edge 3 Pro, волосы не мотает",
        ),
    )
    .await
    .unwrap();
    recorded(&pool, 1, 11, "петя", "погода в москве", 0).await;

    let output = search(
        &chat,
        &pool,
        ChatId(1),
        0,
        &request(json!({"query": "пылесос"})),
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    let texts: Vec<&str> = value["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["text"].as_str().unwrap())
        .collect();
    assert!(texts.contains(&"какой самый лучший пылесос"), "{texts:?}");
    assert!(
        texts.contains(&"бери Roborock Qrevo Edge 3 Pro, волосы не мотает"),
        "adjacent assistant reply must be paired in lexical search: {texts:?}"
    );
    server.task.abort();
}
