use crate::test_support::chat_reply;
use crate::test_support::*;
use crate::*;
use serde_json::json;

fn test_source(id: usize, title: &str, url: &str) -> ResearchSource {
    ResearchSource {
        id,
        title: title.to_owned(),
        url: url.to_owned(),
        domain: "example.com".to_owned(),
        snippet: "snippet".to_owned(),
        queries: vec![1],
        best_position: 0,
    }
}

fn research_call(question: &str, queries: &[&str]) -> String {
    tool_response(
        "research",
        json!({"question": question, "queries": queries}),
    )
}

#[test]
fn research_urls_are_canonicalized_and_tracking_removed() {
    let normalized =
        normalize_url("HTTPS://Example.COM:443/Path?utm_source=x&b=2&a=1&fbclid=y#frag").unwrap();
    assert_eq!(normalized.display, "https://example.com/Path?b=2&a=1");
    assert_eq!(normalized.canonical, "https://example.com/Path?a=1&b=2");
    assert_eq!(normalized.domain, "example.com");

    let www = normalize_url("https://www.Example.com/x").unwrap();
    assert_eq!(www.domain, "example.com");
    assert_eq!(www.display, "https://www.example.com/x");

    for rejected in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "data:text/html,x",
        "https://user:pass@example.com/",
        "https://",
    ] {
        assert!(normalize_url(rejected).is_none(), "{rejected}");
    }
}

#[test]
fn research_text_strips_markup_entities_and_mentions() {
    let text = sanitize_research_text(
        "A &amp; B &mdash; &#8212; &hellip; <script>x</script> @user [1] https://evil.example/x",
        160,
    );
    assert!(!text.contains('<'), "{text}");
    assert!(!text.contains('@'), "{text}");
    assert!(!text.contains("[1]"), "{text}");
    assert!(!text.contains("https://"), "{text}");
    assert!(text.contains("A & B"), "{text}");
    assert!(text.contains('—'), "{text}");
    assert!(text.contains('…'), "{text}");
    assert!(text.contains("(1)"), "{text}");
}

#[test]
fn research_style_preserves_paragraphs_and_appends_sources() {
    let sources = vec![
        test_source(1, "Rust Telegram", "https://example.com/rust"),
        test_source(2, "Teloxide", "https://github.com/teloxide/teloxide"),
        test_source(3, "Third", "https://example.org/third"),
        test_source(4, "Fourth", "https://example.net/fourth"),
    ];
    let reply = style_research_reply(
        "Первый абзац [1].\n\n\n\nВторой — абзац [99].\n\nТретий https://Example.com/Path [2].",
        &sources,
    )
    .unwrap();
    assert!(reply.contains("Первый абзац [1]."), "{reply}");
    assert!(!reply.contains("[99]"), "{reply}");
    assert!(reply.contains("Второй - абзац"), "{reply}");
    assert!(
        reply.contains("Третий https://Example.com/Path [2]"),
        "{reply}"
    );
    assert!(
        reply.contains("источники:\n[1] Rust Telegram - https://example.com/rust"),
        "{reply}"
    );
    assert!(
        reply.contains("[2] Teloxide - https://github.com/teloxide/teloxide"),
        "{reply}"
    );
    assert!(!reply.contains("[3]"), "{reply}");
    assert!(!reply.contains("\n\n\n\n"), "{reply}");
    assert_eq!(
        style_research_reply("   \n\n  ", &sources).unwrap_err(),
        "empty"
    );
}

#[test]
fn research_style_without_citations_uses_first_sources_and_bounds_length() {
    let long_url = format!("https://example.com/{}", "a".repeat(1900));
    let sources = vec![
        test_source(1, "First", &long_url),
        test_source(2, "Second", "https://example.org/second"),
        test_source(3, "Third", "https://example.net/third"),
    ];
    let body = "слово ".repeat(2000);
    let reply = style_research_reply(&body, &sources).unwrap();
    assert!(reply.chars().count() <= 6000, "{}", reply.chars().count());
    assert!(reply.contains("источники:"), "{reply}");
    assert!(
        reply.contains(long_url.as_str()),
        "URL must not be truncated"
    );
    assert!(reply.contains("[1] First - "), "{}", reply);
}

#[tokio::test]
async fn research_requires_two_distinct_queries_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_database(&format!(
        "sqlite://{}",
        dir.path().join("state.db").display()
    ))
    .await
    .unwrap();
    let llm = mock_server(vec![
        research_call("вопрос", &["только один", "ТОЛЬКО   ОДИН"]),
        reply_response("ок"),
    ])
    .await;
    let search = mock_server(vec![]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search.url.clone(),
        SearchProvider::Firecrawl,
        None,
    )
    .with_research_cooldown(Duration::ZERO);

    let reply = chat_reply!(
        &config,
        &pool,
        &StickerReplies::for_tests(),
        &StickerCatalog::default(),
        ChatId(1),
        UserId(2),
        "tester",
        0,
        true,
        "ресерч",
        None,
    )
    .await
    .unwrap();
    assert_eq!(reply.text, "ок");
    let requests = llm.requests.lock().unwrap();
    assert!(tool_contents(&requests[1])[0].contains("нужно 2-4 разных запроса"));
    assert!(
        search.requests.lock().unwrap().is_empty(),
        "validation must run before the network"
    );
    llm.task.abort();
    search.task.abort();
}

#[tokio::test]
async fn research_collects_dedupes_and_cites_sources() {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_database(&format!(
        "sqlite://{}",
        dir.path().join("state.db").display()
    ))
    .await
    .unwrap();
    let llm = mock_server(vec![
        research_call(
            "стоит ли rust для бота",
            &["rust telegram bot", "teloxide production"],
        ),
        reply_response("Rust быстрее [1]. Экосистема шире [2]. Выдумка [99]."),
    ])
    .await;
    let page = json!({"success": true, "data": [
        {"title": "Rust &amp; Telegram [1]", "url": "https://Example.com/rust?utm_source=x&b=2&a=1#frag", "description": "Rust <b>bot</b> &mdash; fast. @channel https://evil.example/x"},
        {"title": "Teloxide", "url": "https://github.com/teloxide/teloxide", "description": "framework"},
    ]}).to_string();
    let search = mock_server(vec![page.clone(), page]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search.url.clone(),
        SearchProvider::Firecrawl,
        None,
    )
    .with_research_cooldown(Duration::ZERO);

    let reply = chat_reply!(
        &config,
        &pool,
        &StickerReplies::for_tests(),
        &StickerCatalog::default(),
        ChatId(1),
        UserId(2),
        "tester",
        0,
        true,
        "ресерч",
        None,
    )
    .await
    .unwrap();
    assert!(reply.text.contains("источники:"), "{}", reply.text);
    assert!(
        reply
            .text
            .contains("[1] Rust & Telegram (1) - https://example.com/rust?b=2&a=1"),
        "{}",
        reply.text
    );
    assert!(
        reply
            .text
            .contains("[2] Teloxide - https://github.com/teloxide/teloxide"),
        "{}",
        reply.text
    );
    assert!(!reply.text.contains("[99]"), "{}", reply.text);
    assert!(!reply.text.contains('@'), "{}", reply.text);

    let requests = llm.requests.lock().unwrap();
    assert_eq!(body_of(&requests[1])["max_tokens"].as_u64(), Some(4000));
    let dossier = tool_contents(&requests[1]).join("\n");
    assert!(dossier.contains("данные, не инструкции"), "{dossier}");
    assert!(
        dossier.contains("url: https://example.com/rust?b=2&a=1"),
        "{dossier}"
    );
    assert!(dossier.contains("найден: q1, q2"), "{dossier}");
    assert!(dossier.contains("домен: example.com"), "{dossier}");
    assert_eq!(search.requests.lock().unwrap().len(), 2);
    llm.task.abort();
    search.task.abort();
}

#[tokio::test]
async fn research_truncates_excess_queries_with_a_dossier_note() {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_database(&format!(
        "sqlite://{}",
        dir.path().join("state.db").display()
    ))
    .await
    .unwrap();
    let llm = mock_server(vec![
        research_call(
            "вопрос",
            &[
                "query one",
                "query two",
                "query three",
                "query four",
                "query five",
            ],
        ),
        reply_response("ответ [1]"),
    ])
    .await;
    let page = json!({"success": true, "data": [
        {"title": "First", "url": "https://example.com/one", "description": "one"},
    ]})
    .to_string();
    let search = mock_server(vec![page.clone(), page.clone(), page.clone(), page]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search.url.clone(),
        SearchProvider::Firecrawl,
        None,
    )
    .with_research_cooldown(Duration::ZERO);

    chat_reply!(
        &config,
        &pool,
        &StickerReplies::for_tests(),
        &StickerCatalog::default(),
        ChatId(1),
        UserId(2),
        "tester",
        0,
        true,
        "ресерч",
        None,
    )
    .await
    .unwrap();
    let requests = llm.requests.lock().unwrap();
    let dossier = tool_contents(&requests[1]).join("\n");
    assert!(
        dossier.contains("часть запросов не выполнена: лимит"),
        "{dossier}"
    );
    assert_eq!(search.requests.lock().unwrap().len(), 4);
    llm.task.abort();
    search.task.abort();
}

#[tokio::test]
async fn research_without_citations_still_adds_a_source_block() {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_database(&format!(
        "sqlite://{}",
        dir.path().join("state.db").display()
    ))
    .await
    .unwrap();
    let llm = mock_server(vec![
        research_call("вопрос", &["first query", "second query"]),
        reply_response("короткий ответ без цитат"),
    ])
    .await;
    let page = json!({"success": true, "data": [
        {"title": "First", "url": "https://example.com/one", "description": "one"},
        {"title": "Second", "url": "https://example.org/two", "description": "two"},
    ]})
    .to_string();
    let search = mock_server(vec![page.clone(), page]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search.url.clone(),
        SearchProvider::Firecrawl,
        None,
    )
    .with_research_cooldown(Duration::ZERO);

    let reply = chat_reply!(
        &config,
        &pool,
        &StickerReplies::for_tests(),
        &StickerCatalog::default(),
        ChatId(1),
        UserId(2),
        "tester",
        0,
        true,
        "ресерч",
        None,
    )
    .await
    .unwrap();
    assert!(
        reply
            .text
            .starts_with("короткий ответ без цитат\n\nисточники:"),
        "{}",
        reply.text
    );
    assert!(
        reply.text.contains("[1] First - https://example.com/one"),
        "{}",
        reply.text
    );
    assert!(
        reply.text.contains("[2] Second - https://example.org/two"),
        "{}",
        reply.text
    );
    llm.task.abort();
    search.task.abort();
}

#[tokio::test]
async fn research_degrades_when_one_query_fails() {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_database(&format!(
        "sqlite://{}",
        dir.path().join("state.db").display()
    ))
    .await
    .unwrap();
    let llm = mock_server(vec![
        research_call("вопрос", &["first query", "second query"]),
        reply_response("часть выдачи недоступна [1]"),
    ])
    .await;
    let success = json!({"success": true, "data": [
        {"title": "First", "url": "https://example.com/one", "description": "one"},
    ]})
    .to_string();
    let failure = json!({"success": false, "error": "quota"}).to_string();
    let search = mock_server(vec![success, failure]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search.url.clone(),
        SearchProvider::Firecrawl,
        None,
    )
    .with_research_cooldown(Duration::ZERO);

    let reply = chat_reply!(
        &config,
        &pool,
        &StickerReplies::for_tests(),
        &StickerCatalog::default(),
        ChatId(1),
        UserId(2),
        "tester",
        0,
        true,
        "ресерч",
        None,
    )
    .await
    .unwrap();
    assert!(reply.text.contains("источники:"), "{}", reply.text);
    let requests = llm.requests.lock().unwrap();
    let dossier = tool_contents(&requests[1]).join("\n");
    assert!(dossier.contains("ошибки:"), "{dossier}");
    assert!(dossier.contains("недоступен"), "{dossier}");
    llm.task.abort();
    search.task.abort();
}

#[tokio::test]
async fn research_all_failures_report_unavailable_without_sources() {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_database(&format!(
        "sqlite://{}",
        dir.path().join("state.db").display()
    ))
    .await
    .unwrap();
    let llm = mock_server(vec![
        research_call("вопрос", &["first query", "second query"]),
        reply_response("поиск недоступен, сорян"),
    ])
    .await;
    let failure = json!({"success": false, "error": "quota"}).to_string();
    let search = mock_server(vec![failure.clone(), failure]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search.url.clone(),
        SearchProvider::Firecrawl,
        None,
    )
    .with_research_cooldown(Duration::ZERO);

    let reply = chat_reply!(
        &config,
        &pool,
        &StickerReplies::for_tests(),
        &StickerCatalog::default(),
        ChatId(1),
        UserId(2),
        "tester",
        0,
        true,
        "ресерч",
        None,
    )
    .await
    .unwrap();
    assert_eq!(reply.text, "поиск недоступен, сорян");
    assert!(!reply.text.contains("источники:"));
    let requests = llm.requests.lock().unwrap();
    assert!(tool_contents(&requests[1])[0].contains("поиск недоступен"));
    llm.task.abort();
    search.task.abort();
}

#[tokio::test]
async fn second_research_and_search_budget_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_database(&format!(
        "sqlite://{}",
        dir.path().join("state.db").display()
    ))
    .await
    .unwrap();
    let mut calls = vec![json!({
        "id": "call_research_2", "type": "function",
        "function": {"name": "research", "arguments": json!({
            "question": "вопрос", "queries": ["first query", "second query"],
        }).to_string()},
    })];
    for index in 0..5 {
        calls.push(json!({
            "id": format!("call_search_{index}"), "type": "function",
            "function": {"name": "web_search", "arguments": json!({"query": "rust"}).to_string()},
        }));
    }
    let multi = json!({"choices": [{"message": {
        "role": "assistant", "content": null, "tool_calls": calls,
    }}]})
    .to_string();
    let llm = mock_server(vec![
        research_call("вопрос", &["first query", "second query"]),
        multi,
        reply_response("ответ [1]"),
    ])
    .await;
    let html = r#"<a rel="nofollow" class="result__a" href="https://example.com/page">Example</a><a class="result__snippet">Snippet</a>"#;
    let search = mock_server(vec![
        html.into(),
        html.into(),
        html.into(),
        html.into(),
        html.into(),
        html.into(),
    ])
    .await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search.url.clone(),
        SearchProvider::DuckDuckGo,
        None,
    )
    .with_research_cooldown(Duration::ZERO);

    chat_reply!(
        &config,
        &pool,
        &StickerReplies::for_tests(),
        &StickerCatalog::default(),
        ChatId(1),
        UserId(2),
        "tester",
        0,
        true,
        "ресерч",
        None,
    )
    .await
    .unwrap();
    let requests = llm.requests.lock().unwrap();
    let results = tool_contents(&requests[2]).join("\n");
    assert!(results.contains("один ресерч за ответ"), "{results}");
    assert!(results.contains("лимит поиска на ответ"), "{results}");
    assert_eq!(search.requests.lock().unwrap().len(), 6);
    llm.task.abort();
    search.task.abort();
}

#[tokio::test]
async fn research_cooldown_blocks_a_repeat() {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_database(&format!(
        "sqlite://{}",
        dir.path().join("state.db").display()
    ))
    .await
    .unwrap();
    let llm = mock_server(vec![
        research_call("вопрос", &["first query", "second query"]),
        reply_response("ответ [1]"),
        research_call("вопрос", &["first query", "second query"]),
        reply_response("ок"),
    ])
    .await;
    let page = json!({"success": true, "data": [
        {"title": "First", "url": "https://example.com/one", "description": "one"},
    ]})
    .to_string();
    let search = mock_server(vec![page.clone(), page]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search.url.clone(),
        SearchProvider::Firecrawl,
        None,
    )
    .with_research_cooldown(Duration::from_secs(20));

    let first = chat_reply!(
        &config,
        &pool,
        &StickerReplies::for_tests(),
        &StickerCatalog::default(),
        ChatId(1),
        UserId(2),
        "tester",
        0,
        false,
        "ресерч",
        None,
    )
    .await
    .unwrap();
    assert!(first.text.contains("источники:"), "{}", first.text);
    let second = chat_reply!(
        &config,
        &pool,
        &StickerReplies::for_tests(),
        &StickerCatalog::default(),
        ChatId(1),
        UserId(2),
        "tester",
        1,
        false,
        "ещё ресерч",
        None,
    )
    .await
    .unwrap();
    assert_eq!(second.text, "ок");
    let requests = llm.requests.lock().unwrap();
    assert!(tool_contents(&requests[3])[0].contains("ресерч не чаще раза в 20 секунд"));
    assert_eq!(search.requests.lock().unwrap().len(), 2);
    llm.task.abort();
    search.task.abort();
}

#[test]
fn omp_search_parsers_and_public_consensus_merge() {
    let sse = br#"event: message
data: {"jsonrpc":"2.0","id":"1","result":{"content":[{"type":"text","text":"Title: Exa Hit\nURL: https://exa.ai/post\nText: Exa snippet text\n\nTitle: Second Hit\nURL: https://example.com/two\nText: Second snippet"}]}}
"#;
    let rpc_res = crate::search::parse_mcp_jsonrpc_result(sse).unwrap();
    let exa_hits = crate::search::parse_exa_mcp_hits(&rpc_res, 5);
    assert_eq!(exa_hits.len(), 2);
    assert_eq!(exa_hits[0].title, "Exa Hit");
    assert_eq!(exa_hits[0].url, "https://exa.ai/post");
    assert_eq!(exa_hits[0].snippet, "Exa snippet text");

    let parallel_json = br#"{"jsonrpc":"2.0","id":"1","result":{"structuredContent":{"search_id":"s1","results":[{"title":"Parallel Hit","url":"https://parallel.ai/doc","excerpts":["first excerpt","second excerpt"]}]}}}"#;
    let par_rpc = crate::search::parse_mcp_jsonrpc_result(parallel_json).unwrap();
    let par_hits = crate::search::parse_parallel_mcp_hits(&par_rpc, 5);
    assert_eq!(par_hits.len(), 1);
    assert_eq!(par_hits[0].url, "https://parallel.ai/doc");
    assert!(par_hits[0].snippet.contains("first excerpt"));

    let sp_html = r#"<div class="result"><a class="result-link" href="https://example.org/sp"><h2>Startpage Title</h2></a><p class="description">Startpage snippet</p></div>"#;
    let sp_hits = crate::search::extract_startpage_results(sp_html, 5);
    assert_eq!(sp_hits.len(), 1);
    assert_eq!(sp_hits[0].title, "Startpage Title");

    let merged = crate::search::merge_public_hits(
        &[
            (
                0,
                vec![
                    SearchHit {
                        title: "Unique".into(),
                        url: "https://a.example/one".into(),
                        snippet: "short".into(),
                    },
                    SearchHit {
                        title: "Consensus".into(),
                        url: "https://www.b.example/two/".into(),
                        snippet: "short".into(),
                    },
                ],
            ),
            (
                1,
                vec![SearchHit {
                    title: "Consensus Better".into(),
                    url: "https://b.example/two".into(),
                    snippet: "much longer consensus snippet".into(),
                }],
            ),
        ],
        5,
    );
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].url, "https://b.example/two");
    assert_eq!(merged[0].snippet, "much longer consensus snippet");
}

/// Local search endpoint: answers the first request with one Firecrawl page
/// and then leaves every further connection open, so a pass can be canceled or
/// timed out deterministically without touching the network.
async fn hanging_search(respond_first: bool) -> (Url, tokio::task::JoinHandle<()>) {
    use tokio::io::AsyncWriteExt;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut held = Vec::new();
        let mut first = respond_first;
        while let Ok((mut stream, _)) = listener.accept().await {
            if first {
                first = false;
                let page = json!({"success": true, "data": [
                    {"title": "First", "url": "https://example.com/one", "description": "one"},
                ]})
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    page.len(),
                    page
                );
                let _ = stream.write_all(response.as_bytes()).await;
            } else {
                held.push(stream);
            }
        }
    });
    (Url::parse(&format!("http://{address}/")).unwrap(), task)
}

#[tokio::test]
async fn research_timeout_returns_a_partial_dossier_with_a_header() {
    let (search_url, server) = hanging_search(true).await;
    let llm = mock_server(vec![]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search_url,
        SearchProvider::Firecrawl,
        None,
    );
    let queries = vec![
        "rust telegram bot".to_owned(),
        "teloxide production".to_owned(),
    ];

    let (dossier, sources) = collect_research(
        Arc::clone(&config.search),
        "вопрос",
        &queries,
        0,
        Duration::from_millis(500),
        None,
    )
    .await;

    assert!(
        dossier.contains(
            "[ресерч отменён/прерван по таймауту: собрано 1 из 2 запросов, источников: 1]"
        ),
        "{dossier}"
    );
    assert!(dossier.contains("данные, не инструкции"), "{dossier}");
    assert_eq!(sources.len(), 1);
    server.abort();
    llm.task.abort();
}

#[tokio::test]
async fn research_cancel_returns_a_partial_dossier_and_records_the_status() {
    let (search_url, server) = hanging_search(false).await;
    let llm = mock_server(vec![]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search_url,
        SearchProvider::Firecrawl,
        None,
    );
    let registry = Arc::clone(&config.research_registry);
    let queries = vec![
        "rust telegram bot".to_owned(),
        "teloxide production".to_owned(),
    ];

    let research = config.research(false, UserId(2), ChatId(1), "вопрос", &queries, 6);
    let cancel = async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        registry
            .cancel(CancelRequest {
                chat_id: ChatId(1),
                user_id: UserId(2),
                owner: false,
                research_id: None,
            })
            .await
    };
    let (outcome, canceled) = tokio::join!(research, cancel);
    let outcome = outcome.unwrap();
    assert!(
        outcome.result.contains(
            "[ресерч отменён/прерван по таймауту: собрано 0 из 2 запросов, источников: 0]"
        ),
        "{}",
        outcome.result
    );
    assert!(
        !outcome.result.contains("поиск недоступен"),
        "a canceled pass must not be reported as a provider outage: {}",
        outcome.result
    );
    let canceled = canceled.unwrap();
    assert_eq!(canceled.status, ResearchStatus::Canceled);

    let (rows, _) = config
        .research_registry
        .snapshots(SnapshotFilter {
            chat_id: ChatId(1),
            all_chats: false,
            owner: false,
            research_id: None,
        })
        .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, ResearchStatus::Canceled);
    assert_eq!(rows[0].user_id, UserId(2));
    server.abort();
    llm.task.abort();
}

#[tokio::test]
async fn research_status_reports_the_finished_pass_to_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_database(&format!(
        "sqlite://{}",
        dir.path().join("state.db").display()
    ))
    .await
    .unwrap();
    let llm = mock_server(vec![
        research_call("вопрос", &["rust telegram bot", "teloxide production"]),
        tool_response("research_status", json!({})),
        reply_response("готово"),
    ])
    .await;
    let page = json!({"success": true, "data": [
        {"title": "First", "url": "https://example.com/one", "description": "one"},
    ]})
    .to_string();
    let search = mock_server(vec![page.clone(), page]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search.url.clone(),
        SearchProvider::Firecrawl,
        None,
    )
    .with_research_cooldown(Duration::ZERO);

    chat_reply!(
        &config,
        &pool,
        &StickerReplies::for_tests(),
        &StickerCatalog::default(),
        ChatId(1),
        UserId(2),
        "tester",
        0,
        true,
        "ресерч",
        None,
    )
    .await
    .unwrap();

    let requests = llm.requests.lock().unwrap();
    let snapshot = tool_contents(&requests[2]).join("\n");
    assert!(snapshot.contains("\"id\":\"res-1\""), "{snapshot}");
    assert!(snapshot.contains("\"status\":\"completed\""), "{snapshot}");
    assert!(snapshot.contains("\"completed_queries\":2"), "{snapshot}");
    assert!(snapshot.contains("\"total_queries\":2"), "{snapshot}");
    assert!(snapshot.contains("\"sources\":1"), "{snapshot}");
    assert!(snapshot.contains("\"user_id\":2"), "{snapshot}");
    assert!(snapshot.contains("\"elapsed_ms\":"), "{snapshot}");
    llm.task.abort();
    search.task.abort();
}

#[tokio::test]
async fn research_cancel_payload_reports_the_canceled_pass_then_refuses_a_repeat() {
    let llm = mock_server(vec![]).await;
    let search = mock_server(vec![]).await;
    let config = ChatConfig::new(
        llm.url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        search.url.clone(),
        SearchProvider::Firecrawl,
        None,
    );
    let job = config
        .research_registry
        .start(ChatId(1), UserId(2), "вопрос", 3)
        .await
        .unwrap();
    let id = job.id().to_owned();

    let first =
        crate::research_tools::cancel_payload(&config, ChatId(1), UserId(2), false, &json!({}))
            .await;
    let value: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(value["canceled"], true, "{first}");
    assert_eq!(value["id"], id.as_str(), "{first}");
    assert_eq!(value["status"], "canceled", "{first}");
    assert_eq!(value["total_queries"], 3, "{first}");
    assert_eq!(value["completed_queries"], 0, "{first}");
    assert_eq!(value["sources"], 0, "{first}");

    let second =
        crate::research_tools::cancel_payload(&config, ChatId(1), UserId(2), false, &json!({}))
            .await;
    let value: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(value["canceled"], false, "{second}");
    assert_eq!(value["error"], "нет активного ресерча", "{second}");

    let by_id = crate::research_tools::cancel_payload(
        &config,
        ChatId(1),
        UserId(2),
        false,
        &json!({"research_id": id}),
    )
    .await;
    let value: serde_json::Value = serde_json::from_str(&by_id).unwrap();
    assert_eq!(value["canceled"], false, "{by_id}");
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("уже завершён: canceled"),
        "{by_id}"
    );

    job.finish(ResearchStatus::Canceled, 0, 0).await;
    llm.task.abort();
    search.task.abort();
}
