use crate::chat_profile::{profile_from_member, run, ProfileCache};
use crate::test_support::*;
use crate::*;
use serde_json::json;

struct Fixture {
    _dir: tempfile::TempDir,
    pool: SqlitePool,
    chat: ChatConfig,
    stickers: StickerReplies,
    catalog: StickerCatalog,
}

async fn fixture() -> Fixture {
    let (dir, pool) = test_pool().await;
    let url = Url::parse("http://127.0.0.1:9/").unwrap();
    let chat = ChatConfig::new(
        url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        url,
        SearchProvider::DuckDuckGo,
        None,
    );
    Fixture {
        _dir: dir,
        pool,
        chat,
        stickers: StickerReplies::for_tests(),
        catalog: StickerCatalog::for_tests(&[]),
    }
}

fn context<'a>(
    fixture: &'a Fixture,
    bot: Option<&'a Bot>,
    owner: bool,
    user_id: UserId,
    reply_user_id: Option<UserId>,
) -> ToolContext<'a> {
    ToolContext {
        chat: &fixture.chat,
        pool: &fixture.pool,
        stickers: &fixture.stickers,
        catalog: &fixture.catalog,
        proposal: None,
        owner,
        chat_id: ChatId(1),
        user_id,
        sender_name: "tester (@tester)",
        username: Some("tester"),
        message_id: 5,
        reply_user_id,
        bot,
        read_url_targets: &[],
    }
}

async fn record_author(pool: &SqlitePool, chat_id: i64, author_id: i64, name: &str) {
    crate::memory::record_message(
        pool,
        crate::memory::RecordMessage::new(ChatId(chat_id), author_id, name, "user", "привет"),
    )
    .await
    .unwrap();
}

/// Bot API double that echoes the requested `user_id` back with a full
/// administrator profile, and records every request it answered.
async fn echoing_bot() -> (
    Bot,
    Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let records = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let captured = Arc::clone(&records);
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let (head, body) = read_bot_api_request(&mut stream).await;
            let path = head
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let body = String::from_utf8_lossy(&body).into_owned();
            let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let id = request["user_id"].as_u64().unwrap_or(0);
            let response = json!({
                "ok": true,
                "result": {
                    "status": "administrator",
                    "user": {
                        "id": id,
                        "is_bot": false,
                        "first_name": "Target",
                        "last_name": "Member",
                        "username": "target",
                        "is_premium": true
                    },
                    "can_be_edited": false,
                    "is_anonymous": false,
                    "can_manage_chat": true,
                    "can_delete_messages": true,
                    "can_manage_video_chats": false,
                    "can_restrict_members": false,
                    "can_promote_members": false,
                    "can_change_info": false,
                    "can_invite_users": false,
                    "custom_title": "Boss"
                }
            })
            .to_string();
            captured.lock().await.push((path.clone(), body));
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            let _ = stream.write_all(headers.as_bytes()).await;
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    let bot = Bot::new("123:dummy").set_api_url(Url::parse(&format!("http://{address}/")).unwrap());
    (bot, records, server)
}

async fn member_requests(records: &tokio::sync::Mutex<Vec<(String, String)>>) -> Vec<Value> {
    records
        .lock()
        .await
        .iter()
        .filter(|(path, _)| api_method_is(path, "getChatMember"))
        .filter_map(|(_, body)| serde_json::from_str::<Value>(body).ok())
        .collect()
}

fn profile_of(answer: &str) -> Value {
    let value: Value = serde_json::from_str(answer).expect("tool answer is JSON");
    value["profile"].clone()
}

#[test]
fn cache_expires_by_ttl_and_evicts_the_oldest_entry() {
    let now = time::Instant::now();
    let ttl = Duration::from_secs(config().chat.profile_cache_ttl_secs);
    let mut cache = ProfileCache::default();
    cache.insert(ChatId(1), UserId(1), json!({"user_id": 1}), now);
    assert_eq!(
        cache.get(ChatId(1), UserId(1), now).expect("cached")["user_id"],
        json!(1)
    );
    assert!(cache.get(ChatId(1), UserId(1), now + ttl).is_none());
    assert!(cache.get(ChatId(2), UserId(1), now).is_none());

    let max = config().chat.profile_cache_max_entries as u64;
    let mut cache = ProfileCache::default();
    for id in 1..=max + 1 {
        cache.insert(
            ChatId(1),
            UserId(id),
            json!({"user_id": id}),
            now + Duration::from_millis(id),
        );
    }
    let after = now + Duration::from_secs(1);
    assert!(cache.get(ChatId(1), UserId(1), after).is_none());
    assert_eq!(
        cache
            .get(ChatId(1), UserId(max + 1), after)
            .expect("cached")["user_id"],
        json!(max + 1)
    );
}

#[test]
fn profile_from_member_keeps_only_documented_fields() {
    let member: teloxide::types::ChatMember = serde_json::from_value(json!({
        "status": "administrator",
        "user": {
            "id": 42,
            "is_bot": false,
            "first_name": "Ada",
            "last_name": "Lovelace",
            "username": "ada",
            "is_premium": true
        },
        "can_be_edited": false,
        "is_anonymous": false,
        "can_manage_chat": true,
        "can_delete_messages": true,
        "can_manage_video_chats": false,
        "can_restrict_members": false,
        "can_promote_members": false,
        "can_change_info": false,
        "can_invite_users": false,
        "custom_title": "Boss"
    }))
    .unwrap();
    let profile = profile_from_member(&member).expect("profile");
    assert_eq!(profile["user_id"], json!(42));
    assert_eq!(profile["first_name"], json!("Ada"));
    assert_eq!(profile["last_name"], json!("Lovelace"));
    assert_eq!(profile["username"], json!("ada"));
    assert_eq!(profile["is_bot"], json!(false));
    assert_eq!(profile["is_premium"], json!(true));
    assert_eq!(profile["status"], json!("administrator"));
    assert_eq!(profile["custom_title"], json!("Boss"));
    assert!(profile.get("joined_at").is_none());
    assert!(profile.get("can_manage_chat").is_none());
}

#[tokio::test]
async fn non_owner_gets_own_profile_by_default() {
    let fixture = fixture().await;
    let (bot, records, server) = echoing_bot().await;
    let ctx = context(&fixture, Some(&bot), false, UserId(5), None);
    let answer = run(&json!({}), &ctx).await;
    assert_eq!(profile_of(&answer)["user_id"], json!(5));
    let requests = member_requests(&records).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["user_id"], json!(5));
    server.abort();
}

#[tokio::test]
async fn non_owner_cannot_read_a_stranger() {
    let fixture = fixture().await;
    record_author(&fixture.pool, 1, 7, "Семён (@sem)").await;
    record_author(&fixture.pool, 2, 8, "Чужой (@other)").await;
    let (bot, records, server) = echoing_bot().await;
    let ctx = context(&fixture, Some(&bot), false, UserId(5), None);
    for target in [99, 8] {
        let answer = run(&json!({"user_id": target}), &ctx).await;
        assert!(answer.contains("только свой профиль"), "{answer}");
    }
    assert!(member_requests(&records).await.is_empty());
    server.abort();
}

#[tokio::test]
async fn non_owner_reads_recent_authors_and_the_replied_author() {
    let fixture = fixture().await;
    record_author(&fixture.pool, 1, 7, "Семён (@sem)").await;
    let (bot, records, server) = echoing_bot().await;
    let ctx = context(&fixture, Some(&bot), false, UserId(5), Some(UserId(9)));
    let recent = run(&json!({"user_id": 7}), &ctx).await;
    assert_eq!(profile_of(&recent)["user_id"], json!(7));
    let reply_author = run(&json!({"user_id": 9}), &ctx).await;
    assert_eq!(profile_of(&reply_author)["user_id"], json!(9));
    assert_eq!(member_requests(&records).await.len(), 2);
    server.abort();
}

#[tokio::test]
async fn username_resolves_through_chat_history() {
    let fixture = fixture().await;
    record_author(&fixture.pool, 1, 7, "Семён (@sem)").await;
    let (bot, _, server) = echoing_bot().await;
    let ctx = context(&fixture, Some(&bot), false, UserId(5), None);
    let answer = run(&json!({"username": "@SEM"}), &ctx).await;
    assert_eq!(profile_of(&answer)["user_id"], json!(7));
    server.abort();
}

#[tokio::test]
async fn cyrillic_display_name_matches_case_insensitively() {
    let fixture = fixture().await;
    record_author(&fixture.pool, 1, 7, "Даня (@danya)").await;
    let (bot, _, server) = echoing_bot().await;
    let ctx = context(&fixture, Some(&bot), false, UserId(5), None);
    let answer = run(&json!({"username": "даня"}), &ctx).await;
    assert_eq!(profile_of(&answer)["user_id"], json!(7));
    let pasted = run(&json!({"username": "Даня (@danya)"}), &ctx).await;
    assert_eq!(profile_of(&pasted)["user_id"], json!(7));
    server.abort();
}

#[tokio::test]
async fn author_older_than_the_prompt_window_stays_authorized() {
    let fixture = fixture().await;
    record_author(&fixture.pool, 1, 7, "Старый (@old)").await;
    for offset in 0..=config().memory.recent_messages {
        record_author(&fixture.pool, 1, 9, &format!("Новый{offset}")).await;
    }
    let (bot, _, server) = echoing_bot().await;
    let ctx = context(&fixture, Some(&bot), false, UserId(5), None);
    let answer = run(&json!({"user_id": 7}), &ctx).await;
    assert_eq!(profile_of(&answer)["user_id"], json!(7));
    server.abort();
}

#[tokio::test]
async fn ambiguous_display_name_is_rejected() {
    let fixture = fixture().await;
    record_author(&fixture.pool, 1, 7, "даня").await;
    record_author(&fixture.pool, 1, 8, "даня").await;
    let (bot, records, server) = echoing_bot().await;
    let ctx = context(&fixture, Some(&bot), false, UserId(5), None);
    let answer = run(&json!({"username": "даня"}), &ctx).await;
    assert!(answer.contains("несколько участников"), "{answer}");
    assert!(member_requests(&records).await.is_empty());
    server.abort();
}

#[tokio::test]
async fn owner_reads_any_participant_and_cached_replies_skip_the_api() {
    let fixture = fixture().await;
    let (bot, records, server) = echoing_bot().await;
    let owner = context(&fixture, Some(&bot), true, UserId(2), None);
    let stranger = run(&json!({"user_id": 77}), &owner).await;
    assert_eq!(profile_of(&stranger)["user_id"], json!(77));
    let again = run(&json!({"user_id": 77}), &owner).await;
    assert_eq!(profile_of(&again)["user_id"], json!(77));
    assert_eq!(member_requests(&records).await.len(), 1);
    server.abort();
}

#[tokio::test]
async fn profile_without_a_bot_is_unavailable() {
    let fixture = fixture().await;
    let ctx = context(&fixture, None, true, UserId(2), None);
    let answer = run(&json!({"user_id": 77}), &ctx).await;
    assert!(answer.contains("недоступен"), "{answer}");
}
