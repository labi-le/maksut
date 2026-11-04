use crate::chat_embeddings::{
    coverage, index_pending, parse_search_request, reindex, search, SearchRequest,
};
use crate::test_support::*;
use crate::*;
use serde_json::json;

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
async fn index_writes_vectors_for_unflagged_messages_only() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![embeddings_body(&[axis(0), axis(1)])]).await;
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

    let indexed = index_pending(&chat, &pool, 10).await.unwrap();
    assert_eq!(indexed, 2);
    let model = config().memory.embedding_model.trim().to_owned();
    assert_eq!(coverage(&pool, &model).await.unwrap(), (2, 0));

    let body = body_of(&server.requests.lock().unwrap()[0]);
    assert_eq!(body["model"], json!(model));
    let inputs = body["input"].as_array().unwrap();
    assert_eq!(inputs.len(), 2);
    assert!(!format!("{inputs:?}").contains("секрет"));
    server.task.abort();
}

#[tokio::test]
async fn search_scopes_to_one_chat_and_drops_low_scores() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![
        embeddings_body(&[axis(0), axis(1), axis(0)]),
        embeddings_body(&[axis(0)]),
    ])
    .await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "котики любят рыбу", 0).await;
    recorded(&pool, 1, 11, "петя", "погода в москве", 0).await;
    recorded(&pool, 2, 10, "аня", "котики в чужом чате", 0).await;
    index_pending(&chat, &pool, 10).await.unwrap();

    let output = search(
        &chat,
        &pool,
        ChatId(1),
        0,
        &request(json!({"query": "котики"})),
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
}

#[tokio::test]
async fn search_merges_recent_messages_and_dedupes() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![
        embeddings_body(&[axis(0)]),
        embeddings_body(&[axis(0)]),
    ])
    .await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "старая реплика про котиков", 0).await;
    index_pending(&chat, &pool, 10).await.unwrap();
    recorded(&pool, 1, 11, "петя", "новая реплика", 0).await;

    let output = search(
        &chat,
        &pool,
        ChatId(1),
        0,
        &request(json!({"query": "котики"})),
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
}

#[tokio::test]
async fn chat_search_tool_scopes_to_the_current_chat() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![
        embeddings_body(&[axis(0), axis(0)]),
        embeddings_body(&[axis(0)]),
    ])
    .await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "котики любят рыбу", 0).await;
    recorded(&pool, 2, 10, "аня", "котики в чужом чате", 0).await;
    index_pending(&chat, &pool, 10).await.unwrap();

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
    let bytes: Vec<u8> = axis(0)
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    sqlx::query(
        "INSERT INTO chat_embeddings (chat_id, message_id, chunk_text, vector, model, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(1_i64)
    .bind(message_id)
    .bind("секрет token=abcdef")
    .bind(bytes)
    .bind(config().memory.embedding_model.trim().to_owned())
    .bind(0_i64)
    .execute(&pool)
    .await
    .unwrap();

    let output = search(
        &chat,
        &pool,
        ChatId(1),
        0,
        &request(json!({"query": "секрет"})),
    )
    .await
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert!(value["results"].as_array().unwrap().is_empty());
    assert!(value["recent"].as_array().unwrap().is_empty());
    server.task.abort();
}

#[tokio::test]
async fn search_filters_by_author_and_date_window() {
    let (_dir, pool) = test_pool().await;
    let same = axis(0);
    let server = mock_server(vec![
        embeddings_body(&[same.clone(), same.clone()]),
        embeddings_body(std::slice::from_ref(&same)),
        embeddings_body(std::slice::from_ref(&same)),
        embeddings_body(std::slice::from_ref(&same)),
    ])
    .await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "реплика номер один", 0).await;
    recorded(&pool, 1, 11, "петя", "реплика номер два", 0).await;
    sqlx::query("UPDATE chat_messages SET created_at = ?")
        .bind(1_780_000_000_i64)
        .execute(&pool)
        .await
        .unwrap();
    index_pending(&chat, &pool, 10).await.unwrap();

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
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "котики", 0).await;
    let message_id: i64 = sqlx::query_scalar("SELECT id FROM chat_messages LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO chat_embeddings (chat_id, message_id, chunk_text, vector, model, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(1_i64)
    .bind(message_id)
    .bind("котики")
    .bind(vec![0u8; DIM * 4])
    .bind("old-embedding-model")
    .bind(0_i64)
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(index_pending(&chat, &pool, 10).await.unwrap(), 1);
    let model = config().memory.embedding_model.trim().to_owned();
    assert_eq!(coverage(&pool, &model).await.unwrap(), (1, 0));
    server.task.abort();
}

#[tokio::test]
async fn reindex_reports_coverage() {
    let (_dir, pool) = test_pool().await;
    let server = mock_server(vec![embeddings_body(&[axis(0)])]).await;
    let chat = chat_config(&server.url).await;
    recorded(&pool, 1, 10, "аня", "котики", 0).await;

    let report: Value = serde_json::from_str(&reindex(&chat, &pool, 2).await).unwrap();
    assert_eq!(report["indexed_now"], json!(1));
    assert_eq!(report["indexed"], json!(1));
    assert_eq!(report["pending"], json!(0));
    server.task.abort();
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
