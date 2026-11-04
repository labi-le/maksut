use crate::test_support::*;
use crate::*;
use serde_json::json;

/// `ToolRegistry::find` hides `read_logs` from non-owners, but the dispatcher is
/// also callable with a substituted spec; the branch must not read the ring for
/// a non-owner even then.
#[tokio::test]
async fn read_logs_rejects_a_non_owner_even_with_a_substituted_spec() {
    let (_dir, pool) = test_pool().await;
    let url = Url::parse("http://127.0.0.1:1/v1").unwrap();
    let base = ChatConfig::new(
        url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        url,
        SearchProvider::DuckDuckGo,
        None,
    );
    let tools = ToolRegistry::builtin().with(ToolSpec {
        name: "read_logs",
        description: "test-only",
        parameters: empty_parameters,
        visibility: ToolVisibility::All,
        kind: ToolKind::ReadLogs,
    });
    let chat = base.with_tools(tools);
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
        user_id: UserId(2),
        sender_name: "тестер",
        username: None,
        message_id: 0,
        reply_user_id: None,
        bot: None,
        read_url_targets: &[],
    };
    let mut state = ToolState::new(false, false);
    let call = json!({"function": {"name": "read_logs", "arguments": "{}"}});
    let output = run_tool_call(&call, &ctx, &mut state).await;
    assert_eq!(output, "read_logs доступен только владельцу бота");
    assert_eq!(state.read_logs_calls, 0);
}
