#![cfg(unix)]
//! HTTP surface tests: authorization gates, the memory write path, the config
//! whitelist, and the router wiring (static assets, CSP, body limit).

use crate::web::WebState;
use crate::web_api::{delete_memory, get_config, list_memory, update_memory, MemoryEdit};
use crate::web_config::{EditKind, EDITABLE};
use crate::*;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::Response;
use axum::Json;
use tower::ServiceExt;

const TOKEN: &str = "123456:test-token";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn chat_config(base: &Path) -> ChatConfig {
    let mut chat = ChatConfig::new(
        Url::parse("http://127.0.0.1:9/v1").unwrap(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        Url::parse("http://127.0.0.1:9/search").unwrap(),
        SearchProvider::DuckDuckGo,
        None,
    );
    chat.code_dir = base.to_path_buf();
    chat
}

async fn fixture() -> (tempfile::TempDir, WebState) {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = crate::test_support::fake_settings(dir.path(), "#!/bin/sh\n").await;
    settings.chat = Some(chat_config(dir.path()));
    let state = WebState {
        settings: Arc::new(settings),
        bot_token: Arc::from(TOKEN),
        config_lock: Arc::new(tokio::sync::Mutex::new(())),
    };
    (dir, state)
}

fn auth_headers(user_id: u64) -> HeaderMap {
    let data = crate::test_support::signed_init_data(TOKEN, user_id, now());
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("tma {data}")).unwrap(),
    );
    headers
}

async fn insert_row(state: &WebState, topic: &str, fact: &str, source_url: &str) {
    sqlx::query(
        "INSERT INTO bot_self_memory (topic, fact, source_url, updated_at) VALUES (?, ?, ?, ?)",
    )
    .bind(topic)
    .bind(fact)
    .bind(source_url)
    .bind(now())
    .execute(&state.settings.database)
    .await
    .unwrap();
}

async fn json(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

const SOURCE: &str = "https://github.com/labi-le/maxwell/blob/main/src/main.rs#L1";

#[tokio::test]
async fn rejects_requests_without_auth() {
    let (_dir, state) = fixture().await;
    let app = crate::web::router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/memory")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejects_another_user_and_expired_payloads() {
    let (_dir, state) = fixture().await;
    let response = list_memory(State(state.clone()), auth_headers(2)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let stale = crate::test_support::signed_init_data(TOKEN, 1, now() - 3 * 86_400);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("tma {stale}")).unwrap(),
    );
    let response = list_memory(State(state), headers).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn lists_memory_for_the_owner() {
    let (_dir, state) = fixture().await;
    insert_row(&state, "src/main.rs", "факт один", SOURCE).await;
    insert_row(&state, "директива", "директива владельца", "").await;
    let response = list_memory(State(state), auth_headers(1)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let items = json(response).await;
    let items = items.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .all(|item| item.get("fact_max_chars").is_some()));
}

#[tokio::test]
async fn edits_a_code_fact_through_the_shared_write_path() {
    let (dir, state) = fixture().await;
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    insert_row(&state, "main fn", "старый факт", SOURCE).await;

    let response = update_memory(
        State(state.clone()),
        AxumPath("main fn".into()),
        auth_headers(1),
        Json(MemoryEdit {
            fact: "новый факт".into(),
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let item = json(response).await;
    assert_eq!(item["fact"], "новый факт");
    assert_eq!(item["source_url"], SOURCE);

    let stored: String = sqlx::query_scalar("SELECT fact FROM bot_self_memory WHERE topic = ?")
        .bind("main fn")
        .fetch_one(&state.settings.database)
        .await
        .unwrap();
    assert_eq!(stored, "новый факт");
}

#[tokio::test]
async fn refuses_owner_directives_and_unknown_rows() {
    let (_dir, state) = fixture().await;
    insert_row(&state, "директива", "не трогать", "").await;

    let edit = update_memory(
        State(state.clone()),
        AxumPath("директива".into()),
        auth_headers(1),
        Json(MemoryEdit {
            fact: "новое".into(),
        }),
    )
    .await;
    assert_eq!(edit.status(), StatusCode::CONFLICT);

    let delete = delete_memory(
        State(state.clone()),
        AxumPath("директива".into()),
        auth_headers(1),
    )
    .await;
    assert_eq!(delete.status(), StatusCode::CONFLICT);

    let missing = delete_memory(State(state), AxumPath("нет такой".into()), auth_headers(1)).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deletes_a_code_fact() {
    let (_dir, state) = fixture().await;
    insert_row(&state, "src/main.rs", "факт", SOURCE).await;
    let response = delete_memory(
        State(state.clone()),
        AxumPath("src/main.rs".into()),
        auth_headers(1),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["deleted"], true);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bot_self_memory")
        .fetch_one(&state.settings.database)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn rejects_an_empty_fact_edit() {
    let (dir, state) = fixture().await;
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    insert_row(&state, "main fn", "факт", SOURCE).await;
    let response = update_memory(
        State(state),
        AxumPath("main fn".into()),
        auth_headers(1),
        Json(MemoryEdit { fact: " ".into() }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn exposes_only_the_config_whitelist() {
    let (_dir, state) = fixture().await;
    let response = get_config(State(state), auth_headers(1)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let items = json(response).await;
    let items = items.as_array().unwrap();
    assert_eq!(items.len(), EDITABLE.len());
    let media = items
        .iter()
        .find(|item| item["key"] == "media.max_bytes")
        .unwrap();
    assert_eq!(media["kind"], "int");
    assert_eq!(media["restart_required"], true);
    assert!(items
        .iter()
        .all(|item| item["kind"] == "int" || item["kind"] == "str"));
    assert!(EDITABLE.iter().any(|entry| entry.kind == EditKind::Str));
}

#[tokio::test]
async fn serves_public_static_assets_with_a_strict_csp() {
    let (_dir, state) = fixture().await;
    let app = crate::web::router(state);
    let response = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let csp = response
        .headers()
        .get("content-security-policy")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("default-src 'none'"), "{csp}");
    assert!(
        csp.contains("script-src 'self' https://telegram.org"),
        "{csp}"
    );
    // The Telegram Web SDK injects its iframe stylesheet; tightening this
    // without moving the SDK to a same-origin copy breaks the Web client.
    assert!(csp.contains("style-src 'self' 'unsafe-inline'"), "{csp}");
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("panel-memory"));
    assert!(html.contains("/app.js"));

    let script = app
        .oneshot(
            Request::builder()
                .uri("/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(script.status(), StatusCode::OK);
}

#[tokio::test]
async fn rejects_a_body_over_the_configured_limit() {
    let (_dir, state) = fixture().await;
    let app = crate::web::router(state);
    let payload = "x".repeat(20_000);
    let request = Request::builder()
        .method("PUT")
        .uri("/api/memory/topic")
        .header(
            header::AUTHORIZATION,
            format!(
                "tma {}",
                crate::test_support::signed_init_data(TOKEN, 1, now())
            ),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, payload.len())
        .body(Body::from(payload))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[test]
fn settings_reply_builds_a_web_app_button_only_when_configured() {
    use crate::web::{settings_reply, SettingsReply};
    let mut web = Config::default().web;
    assert!(matches!(settings_reply(&web), SettingsReply::Text(_)));
    web.enabled = true;
    assert!(matches!(settings_reply(&web), SettingsReply::Text(_)));
    web.public_url = Some("https://example.org/".into());
    let SettingsReply::Button(keyboard) = settings_reply(&web) else {
        panic!("configured app must produce a button");
    };
    let value = serde_json::to_value(keyboard).unwrap();
    assert_eq!(
        value["inline_keyboard"][0][0]["web_app"]["url"],
        "https://example.org/"
    );
    web.public_url = Some("not a url".into());
    assert!(matches!(settings_reply(&web), SettingsReply::Text(_)));
}

#[test]
fn settings_command_matches_only_its_forms() {
    use crate::web::is_settings_command;
    assert!(is_settings_command("/settings", "MyBot"));
    assert!(is_settings_command("/settings@MyBot extra", "mybot"));
    assert!(!is_settings_command("/settings@Other", "MyBot"));
    assert!(!is_settings_command("/start", "MyBot"));
    assert!(!is_settings_command("settings", "MyBot"));
}

#[tokio::test]
async fn serves_the_api_over_real_http() {
    let (_dir, state) = fixture().await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let app = crate::web::router(state);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base = format!("http://{address}");
    let client = reqwest::Client::new();

    let index = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(index.status(), 200);
    assert!(index.headers().contains_key("content-security-policy"));

    let anonymous = client
        .get(format!("{base}/api/memory"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);

    let data = crate::test_support::signed_init_data(TOKEN, 1, now());
    let owned = client
        .get(format!("{base}/api/memory"))
        .header("Authorization", format!("tma {data}"))
        .send()
        .await
        .unwrap();
    assert_eq!(owned.status(), 200);
    let rows: Value = owned.json().await.unwrap();
    assert!(rows.as_array().is_some());

    server.abort();
}

fn settings_message(chat_id: i64, from_id: u64, text: &str) -> Message {
    serde_json::from_value(serde_json::json!({
        "message_id": 1,
        "date": 1_700_000_000,
        "chat": {
            "id": chat_id,
            "type": if chat_id < 0 { "supergroup" } else { "private" }
        },
        "from": { "id": from_id, "is_bot": false, "first_name": "Owner" },
        "text": text,
    }))
    .unwrap()
}

#[tokio::test]
async fn settings_command_answers_the_owner_and_ignores_everyone_else() {
    let fixture = crate::feature_support::FeatureFixture::new(&[], "").await;
    let bot = fixture.api.bot.clone();

    let owner = settings_message(1, 1, "/settings");
    assert!(crate::web::handle_settings_command(&bot, &owner, &fixture.cfg).await);
    let requests = fixture.api.snapshot().await;
    assert_eq!(requests.len(), 1);
    // The default config keeps the app disabled, so the answer names the key
    // instead of carrying a button.
    assert!(requests[0].reply_text().contains("[web]"));
    assert!(requests[0].fields["reply_markup"].is_null());

    fixture.api.clear().await;
    let stranger = settings_message(1, 2, "/settings");
    assert!(crate::web::handle_settings_command(&bot, &stranger, &fixture.cfg).await);
    let group = settings_message(-100, 1, "/settings");
    assert!(crate::web::handle_settings_command(&bot, &group, &fixture.cfg).await);
    let unrelated = settings_message(1, 1, "/start");
    assert!(!crate::web::handle_settings_command(&bot, &unrelated, &fixture.cfg).await);
    assert!(fixture.api.snapshot().await.is_empty());
}
