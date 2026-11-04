use crate::test_support::*;
use crate::*;
use serde_json::json;

fn tokens(input: i64, output: i64, cached: i64, reasoning: i64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cached_tokens: cached,
        reasoning_tokens: reasoning,
    }
}

fn site(chat_id: i64, user_id: u64, name: &str) -> UsageSite {
    UsageSite {
        chat_id: ChatId(chat_id),
        user_id: UserId(user_id),
        author_name: name.to_owned(),
    }
}

fn tool_call(name: &str, arguments: Value) -> Value {
    serde_json::from_str::<Value>(&tool_response(name, arguments))
        .unwrap()
        .pointer("/choices/0/message/tool_calls/0")
        .cloned()
        .unwrap()
}

#[test]
fn parses_provider_usage_shapes() {
    let openai = json!({"usage": {
        "prompt_tokens": 1200,
        "completion_tokens": 300,
        "total_tokens": 1500,
        "prompt_tokens_details": {"cached_tokens": 200},
        "completion_tokens_details": {"reasoning_tokens": 50},
    }});
    assert_eq!(
        Usage::from_response(&openai),
        Some(tokens(1200, 300, 200, 50))
    );

    let alternate = json!({"usage": {"input_tokens": 10, "output_tokens": 5}});
    assert_eq!(Usage::from_response(&alternate), Some(tokens(10, 5, 0, 0)));

    let total_only = json!({"usage": {"total_tokens": 42}});
    assert_eq!(Usage::from_response(&total_only), Some(tokens(42, 0, 0, 0)));

    let split_fallback = json!({"usage": {"completion_tokens": 4, "total_tokens": 10}});
    assert_eq!(
        Usage::from_response(&split_fallback),
        Some(tokens(6, 4, 0, 0))
    );

    assert_eq!(Usage::from_response(&json!({"choices": []})), None);
    assert_eq!(Usage::from_response(&json!({"usage": {}})), None);
}

#[test]
fn bills_cached_input_at_its_own_price() {
    // Defaults: $0.27/M input, $0.07/M cached input, $1.10/M output.
    assert_eq!(tokens(1000, 500, 200, 50).cost_microdollars(), 780);
    assert_eq!(tokens(1000, 0, 0, 0).cost_microdollars(), 270);
    assert_eq!(tokens(0, 1000, 0, 0).cost_microdollars(), 1100);
    // Cached tokens cannot exceed input; only input price applies to the rest.
    assert_eq!(tokens(100, 0, 500, 0).cost_microdollars(), 7);
    assert_eq!(tokens(0, 0, 0, 0).cost_microdollars(), 0);
}

#[test]
fn formats_cost_as_usd() {
    assert_eq!(format_cost(780), "$0.0008");
    assert_eq!(format_cost(12_300), "$0.0123");
    assert_eq!(format_cost(0), "$0.0000");
    assert_eq!(format_cost(-5), "$0.0000");
}

#[test]
fn usage_config_bounds_are_validated() {
    let mut cfg = Config::default();
    cfg.usage.retention_days = 0;
    assert!(cfg.validate().unwrap_err().contains("usage.retention_days"));

    let mut cfg = Config::default();
    cfg.usage.retention_days = 3651;
    assert!(cfg.validate().unwrap_err().contains("usage.retention_days"));

    let mut cfg = Config::default();
    cfg.usage.input_price_per_m = -0.1;
    assert!(cfg
        .validate()
        .unwrap_err()
        .contains("usage.input_price_per_m"));

    let mut cfg = Config::default();
    cfg.usage.cached_input_price_per_m = f64::INFINITY;
    assert!(cfg
        .validate()
        .unwrap_err()
        .contains("usage.cached_input_price_per_m"));

    let mut cfg = Config::default();
    cfg.usage.output_price_per_m = f64::NAN;
    assert!(cfg
        .validate()
        .unwrap_err()
        .contains("usage.output_price_per_m"));

    let mut cfg = Config::default();
    cfg.chat.usage_stats_calls_per_reply = 0;
    assert!(cfg
        .validate()
        .unwrap_err()
        .contains("chat.usage_stats_calls_per_reply"));
}

#[tokio::test]
async fn completion_returns_provider_usage() {
    let server = mock_server(vec![json!({
        "choices": [{"message": {"role": "assistant", "content": "ok"}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13},
    })
    .to_string()])
    .await;
    let chat = ChatConfig::new(
        server.url.clone(),
        "key".to_owned(),
        "test-model".to_owned(),
        server.url.clone(),
        SearchProvider::Auto,
        None,
    );
    let messages = [json!({"role": "user", "content": "hi"})];
    let completion = chat
        .complete(&CompletionRequest::new(&messages, false, 100))
        .await
        .unwrap();
    assert_eq!(completion.usage, Some(tokens(10, 3, 0, 0)));
    assert_eq!(completion.message["content"], "ok");
}

#[tokio::test]
async fn records_and_reports_spend_for_one_chat() {
    let (_dir, pool) = test_pool().await;
    let chat = ChatId(-100);
    site(-100, 7, "Алиса")
        .record(&pool, "test-model", KIND_CHAT, Some(tokens(1000, 0, 0, 0)))
        .await;
    site(-100, 8, "Боб")
        .record(&pool, "test-model", KIND_CHAT, Some(tokens(0, 1000, 0, 0)))
        .await;
    site(-100, 9, "Без usage")
        .record(&pool, "test-model", KIND_CHAT, None)
        .await;

    let report = crate::usage::report(
        &pool,
        UsageQuery {
            chat_id: chat.0,
            days: 7,
            user_id: None,
            all_chats: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(report["totals"]["requests"], 2);
    assert_eq!(report["totals"]["input_tokens"], 1000);
    assert_eq!(report["totals"]["output_tokens"], 1000);
    assert_eq!(report["totals"]["cost_usd"], "$0.0014");
    assert_eq!(report["truncated"], false);
    assert_eq!(report["users"][0]["user_id"], 8);
    assert_eq!(report["users"][0]["cost_usd"], "$0.0011");
    assert_eq!(report["users"][1]["user_id"], 7);
    assert_eq!(report["users"][1]["cost_usd"], "$0.0003");

    let filtered = crate::usage::report(
        &pool,
        UsageQuery {
            chat_id: chat.0,
            days: 7,
            user_id: Some(7),
            all_chats: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(filtered["totals"]["requests"], 1);
    assert_eq!(filtered["users"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["users"][0]["user_id"], 7);
}

#[tokio::test]
async fn scopes_chat_reports_and_prunes_retention() {
    let (_dir, pool) = test_pool().await;
    site(-1, 7, "Алиса")
        .record(&pool, "test-model", KIND_CHAT, Some(tokens(100, 0, 0, 0)))
        .await;
    site(-2, 7, "Алиса")
        .record(&pool, "test-model", KIND_CHAT, Some(tokens(0, 10, 0, 0)))
        .await;

    let one_chat = crate::usage::report(
        &pool,
        UsageQuery {
            chat_id: -1,
            days: 7,
            user_id: None,
            all_chats: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(one_chat["totals"]["requests"], 1);
    assert_eq!(one_chat["totals"]["input_tokens"], 100);
    assert!(one_chat.get("chats").is_none());

    let all_chats = crate::usage::report(
        &pool,
        UsageQuery {
            chat_id: -1,
            days: 7,
            user_id: None,
            all_chats: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(all_chats["totals"]["requests"], 2);
    assert_eq!(all_chats["chats"].as_array().unwrap().len(), 2);

    sqlx::query(
        "INSERT INTO llm_usage (chat_id, user_id, author_name, model, kind, input_tokens, output_tokens, cached_tokens, reasoning_tokens, cost_microdollars, created_at) \
         VALUES (?, ?, ?, ?, ?, 1, 0, 0, 0, 0, ?)",
    )
    .bind(-1_i64)
    .bind(7_i64)
    .bind("Алиса")
    .bind("test-model")
    .bind(KIND_CHAT)
    .bind(unix_now() - 91 * 86_400)
    .execute(&pool)
    .await
    .unwrap();

    // A wider request cannot see rows that retention already discards.
    let retained = crate::usage::report(
        &pool,
        UsageQuery {
            chat_id: -1,
            days: 365,
            user_id: None,
            all_chats: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(retained["days"], 90);
    assert_eq!(retained["totals"]["requests"], 1);
    assert_eq!(retained["totals"]["input_tokens"], 100);

    let pruned = crate::usage::prune(&pool, unix_now()).await.unwrap();
    assert_eq!(pruned, 1);
}

#[tokio::test]
async fn caps_grouped_rows_but_keeps_full_totals() {
    let (_dir, pool) = test_pool().await;
    for user_id in 1..=20 {
        site(-1, user_id, "участник")
            .record(&pool, "test-model", KIND_CHAT, Some(tokens(10, 0, 0, 0)))
            .await;
    }
    let query = UsageQuery {
        chat_id: -1,
        days: 7,
        user_id: None,
        all_chats: false,
    };
    let exact = crate::usage::report(&pool, query).await.unwrap();
    assert_eq!(exact["users"].as_array().unwrap().len(), 20);
    assert_eq!(exact["truncated"], false);

    site(-1, 21, "участник")
        .record(&pool, "test-model", KIND_CHAT, Some(tokens(10, 0, 0, 0)))
        .await;
    let capped = crate::usage::report(
        &pool,
        UsageQuery {
            chat_id: -1,
            days: 7,
            user_id: None,
            all_chats: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(capped["users"].as_array().unwrap().len(), 20);
    assert_eq!(capped["totals"]["requests"], 21);
    assert_eq!(capped["totals"]["input_tokens"], 210);
    assert_eq!(capped["truncated"], true);
}

#[tokio::test]
async fn usage_tool_keeps_other_chats_private_and_honors_budget() {
    let (_dir, pool) = test_pool().await;
    site(-1, 7, "Алиса")
        .record(&pool, "test-model", KIND_CHAT, Some(tokens(100, 0, 0, 0)))
        .await;
    site(-2, 8, "Боб")
        .record(&pool, "test-model", KIND_CHAT, Some(tokens(0, 100, 0, 0)))
        .await;

    let chat = ChatConfig::new(
        Url::parse("http://127.0.0.1:1/v1").unwrap(),
        "key".to_owned(),
        "test-model".to_owned(),
        Url::parse("http://127.0.0.1:1/").unwrap(),
        SearchProvider::Auto,
        None,
    );
    let stickers = StickerReplies::for_tests();
    let catalog = StickerCatalog::for_tests(&[]);
    let call = tool_call("usage_stats", json!({"all_chats": true}));
    let ctx = ToolContext {
        chat: &chat,
        pool: &pool,
        stickers: &stickers,
        catalog: &catalog,
        proposal: None,
        owner: false,
        chat_id: ChatId(-1),
        user_id: UserId(7),
        reply_user_id: None,
        sender_name: "Алиса",
        username: None,
        message_id: 1,
        bot: None,
        read_url_targets: &[],
    };

    let mut member_state = ToolState::new(false, false);
    let member: Value =
        serde_json::from_str(&run_tool_call(&call, &ctx, &mut member_state).await).unwrap();
    assert_eq!(member["chat_id"], -1);
    assert_eq!(member["totals"]["requests"], 1);
    assert!(member.get("chats").is_none());

    let owner_ctx = ToolContext { owner: true, ..ctx };
    let mut owner_state = ToolState::new(false, false);
    let owner: Value =
        serde_json::from_str(&run_tool_call(&call, &owner_ctx, &mut owner_state).await).unwrap();
    assert_eq!(owner["totals"]["requests"], 2);
    assert_eq!(owner["chats"].as_array().unwrap().len(), 2);

    let mut budget_state = ToolState::new(false, false);
    let mut last = String::new();
    for _ in 0..=config().chat.usage_stats_calls_per_reply {
        last = run_tool_call(&call, &owner_ctx, &mut budget_state).await;
    }
    assert!(last.contains("лимит"), "{last}");
}
