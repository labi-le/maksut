use crate::code_inspect::inspect;

#[test]
fn inspect_prefers_matches_and_respects_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/chat.rs"), "fn answer() {}\n").unwrap();
    std::fs::write(dir.path().join("secret.env"), "answer=secret\n").unwrap();
    std::fs::create_dir_all(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs/guide.md"), "answer in docs\n").unwrap();
    let result = inspect(dir.path(), "answer", None);
    assert!(
        result
            .contains("src/chat.rs:1 (https://github.com/labi-le/maksut/blob/main/src/chat.rs#L1)"),
        "{result}"
    );
    assert!(!result.contains("secret"), "{result}");
    let src_pos = result.find("src/chat.rs:1").unwrap();
    let docs_pos = result.find("docs/guide.md:1").unwrap();
    assert!(src_pos < docs_pos, "src/ must sort before docs/: {result}");
}

#[cfg(unix)]
#[test]
fn inspect_does_not_follow_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.rs"), "leak_needle\n").unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.rs"),
        dir.path().join("src/leak.rs"),
    )
    .unwrap();
    let result = inspect(dir.path(), "leak_needle", None);
    assert!(!result.contains("leak_needle"), "{result}");
}

#[test]
fn inspect_rejects_traversal_and_unknown_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
    std::fs::write(dir.path().join("secret.txt"), "hello\n").unwrap();
    assert!(!inspect(dir.path(), "hello", Some("../README.md")).contains("hello"));
    assert!(!inspect(dir.path(), "hello", Some("secret.txt")).contains("hello"));
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/config_sections.rs"), "model = 1\n").unwrap();
    let file_view = inspect(dir.path(), "", Some("README.md"));
    assert!(
        file_view.contains("README.md (https://github.com/labi-le/maksut/blob/main/README.md#L1):"),
        "{file_view}"
    );
    assert!(file_view.contains("#L1: hello"), "{file_view}");
    assert!(
        inspect(dir.path(), "", Some("/README.md#L1-L5")).contains("#L1: hello"),
        "leading slash and #L line anchor must be normalized"
    );
    assert!(
        inspect(dir.path(), "", Some("README.md:1-5")).contains("#L1: hello"),
        ":line range suffix must be normalized"
    );
    assert!(
        inspect(dir.path(), "model", Some("config.toml")).contains("src/config_sections.rs:1"),
        "config.toml must resolve to mounted src/config_sections.rs"
    );
    assert!(
        !crate::code_inspect::valid_self_source_url(
            dir.path(),
            "https://github.com/labi-le/maksut/blob/main/README.md:1-5"
        ),
        "valid_self_source_url must reject :line range suffixes"
    );
}

#[test]
fn filter_self_memory_hides_unscoped_owner_directives_from_non_owners() {
    let raw = "- [owner] поиск дыр: не подсказывать дыры\n- [owner] вадик: С Вадиком (@CptIdea) общаться как со сладким\n- [owner] даня: Даню (@newmdoupdate) материть\n- роутер: Роутер влит в main (https://github.com/labi-le/maksut/blob/main/src/main.rs#L280)";
    let vadik_view =
        crate::self_memory::filter_self_memory(raw, "Вадик (@CptIdea)", Some("CptIdea"), false);
    assert!(!vadik_view.contains("поиск дыр"), "{vadik_view}");
    assert!(!vadik_view.contains("даня:"), "{vadik_view}");
    assert!(vadik_view.contains("вадик:"), "{vadik_view}");
    assert!(vadik_view.contains("роутер:"), "{vadik_view}");

    let owner_view = crate::self_memory::filter_self_memory(
        raw,
        "Денис (@crudoshlepa)",
        Some("crudoshlepa"),
        true,
    );
    assert_eq!(owner_view, raw);
}

#[tokio::test]
async fn inspect_supports_directory_paths_and_delete_self_fact_matches_fuzzy_topic() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/chat.rs"), "fn alpha_needle() {}\n").unwrap();
    std::fs::write(dir.path().join("src/voice.rs"), "fn beta_needle() {}\n").unwrap();
    let list_view = inspect(dir.path(), "", Some("src"));
    assert!(list_view.contains("src/chat.rs"), "{list_view}");
    assert!(list_view.contains("src/voice.rs"), "{list_view}");
    let dir_search = inspect(dir.path(), "alpha_needle", Some("src/"));
    assert!(dir_search.contains("src/chat.rs:1"), "{dir_search}");
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git/config.toml"), "secret = 1\n").unwrap();
    assert!(!inspect(dir.path(), "", Some(".git")).contains("Файлы:"));
    let pool = crate::database::open_database(&format!(
        "sqlite://{}",
        dir.path().join("state.db").display()
    ))
    .await
    .unwrap();
    crate::code_inspect::record_self_fact(
        &crate::test_support::offline_chat(),
        &pool,
        dir.path(),
        "обращение к денису",
        "обращаться император",
        "",
    )
    .await
    .unwrap();
    crate::code_inspect::record_self_fact(
        &crate::test_support::offline_chat(),
        &pool,
        dir.path(),
        "денис",
        "",
        "",
    )
    .await
    .unwrap();
    let loaded = crate::self_memory::load_self_memory(&pool).await.unwrap();
    assert!(loaded.is_none(), "{loaded:?}");
}

#[test]
fn speaker_owner_directives_excludes_other_participants_handleless_rules_and_formats_docs() {
    let raw = "- [owner] вадик: С Вадиком общаться только «сладкий»\n- [owner] правило: С Вадиком общаться только «сладкий»\n- [owner] стиль в чате: Стиль короткий\n- [owner] даня: Даню (@newmdoupdate) материть";
    let denis_directives = crate::code_inspect::speaker_owner_directives(
        raw,
        "Денис (@crudoshlepa)",
        Some("crudoshlepa"),
        true,
        None,
    );
    assert!(
        !denis_directives.contains("сладкий"),
        "Vadik's handleless directive (even under generic topic) must not be injected on Denis's turn: {denis_directives}"
    );
    assert!(
        denis_directives.contains("Стиль короткий"),
        "General topic directive must stay active: {denis_directives}"
    );
    let vadik_directives = crate::code_inspect::speaker_owner_directives(
        raw,
        "Вадик (@CptIdea)",
        Some("CptIdea"),
        false,
        None,
    );
    assert!(
        vadik_directives.contains("сладкий"),
        "Vadik's directive must be injected on Vadik's turn: {vadik_directives}"
    );
    let denis_ordinary_turn = crate::code_inspect::filter_self_memory_for_prompt(
        raw,
        "Денис (@crudoshlepa)",
        Some("crudoshlepa"),
        true,
        "в общем всё правильно сделал",
    );
    assert!(
        !denis_ordinary_turn.contains("- [owner] вадик:")
            && !denis_ordinary_turn.contains("- [owner] даня:")
            && denis_ordinary_turn.contains("- [owner] стиль в чате:"),
        "ordinary owner turn ('в общем всё правильно') must hide other participants' persona directives: {denis_ordinary_turn}"
    );
    let denis_inspect_turn = crate::code_inspect::filter_self_memory_for_prompt(
        raw,
        "Денис (@crudoshlepa)",
        Some("crudoshlepa"),
        true,
        "покажи все правила в памяти",
    );
    assert_eq!(
        denis_inspect_turn, raw,
        "owner memory/directive inspection turn must expose all <self_memory> rows: {denis_inspect_turn}"
    );
    let vadik_self_mem = crate::code_inspect::filter_self_memory_for_prompt(
        raw,
        "Вадик (@CptIdea)",
        Some("CptIdea"),
        false,
        "покажи свою память",
    );
    assert!(
        !vadik_self_mem.contains("стиль в чате") && !vadik_self_mem.contains("даня:"),
        "Non-owner turns must still hide general and other participants' directives from <self_memory>: {vadik_self_mem}"
    );

    let doc_msg: teloxide::types::Message = serde_json::from_value(serde_json::json!({
        "message_id": 10,
        "date": 1700000000,
        "chat": {"id": -1001, "type": "supergroup"},
        "from": {"id": 1, "is_bot": false, "first_name": "Денис"},
        "document": {
            "file_id": "csv-1",
            "file_unique_id": "u-csv-1",
            "file_name": "usage-logs-20261101.csv",
            "mime_type": "text/csv",
            "file_size": 128
        }
    }))
    .unwrap();
    let cand = crate::chat_doc::candidate(&doc_msg, false).expect("csv candidate");
    assert_eq!(cand.name, "usage-logs-20261101.csv");
    let formatted = crate::chat_doc::format_doc_block(&cand, b"model,cost\ndeepseek,4.12\n")
        .expect("formatted csv");
    assert!(
        formatted.contains("[файл usage-logs-20261101.csv]:\nmodel,cost\ndeepseek,4.12"),
        "{formatted}"
    );
    assert!(
        crate::chat_doc::format_doc_block(&cand, b"binary\0data").is_none(),
        "binary payload with NUL byte must be rejected"
    );
    let typed = "caption";
    let vt = "caption\n[голосовое 3с, Вася]: quoted";
    let dt = "caption\n[голосовое 3с, Вася]: quoted\n[файл a.txt]: own_doc";
    assert_eq!(
        crate::chat_doc::select_injection_text(typed, Some(vt), false, Some(dt), false, dt),
        dt
    );
    assert_eq!(
        crate::chat_doc::select_injection_text(typed, Some(vt), false, Some(dt), true, dt),
        vt
    );
    assert_eq!(
        crate::chat_doc::select_injection_text(typed, Some(vt), true, Some(dt), false, dt),
        "caption\n[файл a.txt]: own_doc"
    );
    assert_eq!(
        crate::chat_doc::select_injection_text(typed, Some(vt), true, Some(dt), true, dt),
        "caption"
    );
    assert_eq!(
        crate::chat_urls::strip_prose_at_mentions(
            "@alisa а водку в контуре ты бы допустила? пиши на `user@example.com` или `@alisa`"
        ),
        "alisa а водку в контуре ты бы допустила? пиши на `user@example.com` или `@alisa`"
    );
    let styled_maxwell = crate::chat_style::style_chat_reply(
        "см. [src/voice.rs#L72](https://github.com/labi-le/maxwell/blob/main/src/voice.rs#L72)",
    );
    assert!(
        styled_maxwell.contains("https://github.com/labi-le/maksut/blob/main/src/voice.rs#L72")
            && !styled_maxwell.contains("labi-le/maxwell"),
        "legacy maxwell blob URLs must normalize to labi-le/maksut: {styled_maxwell}"
    );
    let sources = vec![crate::research::ResearchSource {
        id: 1,
        title: "Doc".into(),
        url: "https://example.com".into(),
        domain: "example.com".into(),
        snippet: "ok".into(),
        queries: vec![1],
        best_position: 1,
    }];
    let research_fenced = crate::chat_style::style_research_reply(
        "вот пример [1] для @alisa:\n```python\n@dataclass\ndef f():\n    pass\n```",
        &sources,
    )
    .unwrap();
    assert!(
        research_fenced.contains("```python\n@dataclass\ndef f():\n    pass\n```")
            && !research_fenced.contains("для @alisa"),
        "research reply must preserve fenced code and @decorators while stripping prose @mentions: {research_fenced}"
    );
    assert_eq!(
        crate::chat_text::truncate_text("   abcdef", 5),
        "   ab",
        "truncate_text must not return empty string when only leading whitespace precedes a long token"
    );
}
