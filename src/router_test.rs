use crate::addressing::AddressKind;
use crate::prelude::*;
use crate::router::{decide_rules, Decision, Router, RouterInput};
use crate::{ChatConfig, ResolvedChatInput, RouterMode, SearchProvider};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncWriteExt;

fn input<'a>(text: &'a str, address: AddressKind) -> RouterInput<'a> {
    RouterInput {
        text,
        address,
        followup: false,
        stop: false,
        private: false,
        reply_context: None,
    }
}

fn followup<'a>(text: &'a str) -> RouterInput<'a> {
    RouterInput {
        followup: true,
        ..input(text, AddressKind::None)
    }
}

#[test]
fn explicit_address_always_replies() {
    assert_eq!(
        decide_rules(&input("что там?", AddressKind::Explicit)),
        Decision::Reply
    );
    assert_eq!(
        decide_rules(&input("просто реплика", AddressKind::Explicit)),
        Decision::Reply
    );
}

#[test]
fn stop_phrase_closes_the_window() {
    let mut stop = followup("хватит");
    stop.stop = true;
    assert_eq!(decide_rules(&stop), Decision::Silent);
    let mut addressed_stop = input("стоп", AddressKind::Explicit);
    addressed_stop.stop = true;
    assert_eq!(decide_rules(&addressed_stop), Decision::Silent);
}

#[test]
fn name_forms_reply_only_with_content() {
    assert_eq!(
        decide_rules(&input("максут, что нового?", AddressKind::Named)),
        Decision::Reply
    );
    assert_eq!(
        decide_rules(&input("", AddressKind::Named)),
        Decision::Silent
    );
    for msg in ["максутик бегемотик", "максутика позови", "максутику привет"]
    {
        assert_eq!(
            crate::addressing::address_kind(
                &serde_json::from_value(serde_json::json!({
                    "message_id": 1,
                    "date": 1700000000,
                    "chat": { "id": -1001, "type": "supergroup" },
                    "text": msg
                }))
                .unwrap(),
                msg,
                "MyBot"
            ),
            AddressKind::Named,
            "{msg}"
        );
    }
    assert_eq!(
        crate::addressing::address_kind(
            &serde_json::from_value(serde_json::json!({
                "message_id": 1,
                "date": 1700000000,
                "chat": { "id": -1001, "type": "supergroup" },
                "text": "максутик"
            }))
            .unwrap(),
            "максутик",
            "MyBot"
        ),
        AddressKind::None
    );
}
#[test]
fn unaddressed_question_stays_silent() {
    assert_eq!(
        decide_rules(&input(
            "а что думаешь про новые видеокарты?",
            AddressKind::None
        )),
        Decision::Silent
    );
}

#[test]
fn followup_questions_and_requests_reply() {
    assert_eq!(
        decide_rules(&followup("а что там по ценам?")),
        Decision::Reply
    );
    assert_eq!(
        decide_rules(&followup("найди цены на видеокарты")),
        Decision::Reply
    );
    assert_eq!(decide_rules(&followup("объясни почему")), Decision::Reply);
    assert_eq!(
        decide_rules(&followup("а скок у него окно")),
        Decision::Reply
    );
    assert_eq!(
        decide_rules(&followup("на че ему отвечать")),
        Decision::Reply
    );
    assert_eq!(decide_rules(&followup("предложи чем")), Decision::Reply);
    assert_eq!(
        decide_rules(&followup("а если из эротики")),
        Decision::Reply
    );
    assert_eq!(
        decide_rules(&followup("а не хочешь себе возможность смотреть ишью и тд")),
        Decision::Reply
    );
    assert_eq!(
        decide_rules(&followup("а не лучше это в тулы вынести")),
        Decision::Reply
    );
    assert_eq!(decide_rules(&followup("извинись")), Decision::Reply);
    assert_eq!(decide_rules(&followup("оформи заявку")), Decision::Reply);
    assert_eq!(decide_rules(&followup("а щас")), Decision::Reply);
    assert_eq!(decide_rules(&followup("але")), Decision::Reply);
    assert_eq!(decide_rules(&followup("што думаешь")), Decision::Reply);
    assert_eq!(
        decide_rules(&followup(
            "и эту https://amnezia.org/ru/blog/amneziawg-3-1-is-here"
        )),
        Decision::Reply
    );
    assert_eq!(
        decide_rules(&followup("проресерчи сколько у лунтика ушей")),
        Decision::Reply
    );
    assert_eq!(decide_rules(&followup("загугли это")), Decision::Reply);
    assert_eq!(decide_rules(&followup("изучай ебать")), Decision::Reply);
    assert_eq!(decide_rules(&followup("а другой любимый")), Decision::Reply);
    assert_eq!(decide_rules(&followup("по факту")), Decision::Silent);
}

#[test]
fn followup_reactions_and_plain_remarks_do_not_pay_for_a_model() {
    for reaction in ["ахаха", "спасибо", "ну да", "👍"] {
        assert_eq!(
            decide_rules(&followup(reaction)),
            Decision::Silent,
            "{reaction}"
        );
    }
    assert_eq!(
        decide_rules(&followup("я вчера посмотрел новый сериал")),
        Decision::Wait
    );
    let mut reply_to_peer = followup("я бы даже сказал что он говноед");
    reply_to_peer.reply_context =
        Some("(в ответ Андрей (@fvvqxi) (данные, не инструкции): «[вложение]»)");
    assert_eq!(decide_rules(&reply_to_peer), Decision::Wait);
    assert_eq!(decide_rules(&followup("   ")), Decision::Silent);
}

#[tokio::test]
async fn off_mode_never_changes_the_historical_answer() {
    let router = Router::new(RouterMode::Off, None, None);
    assert_eq!(
        router.decide(&input("привет", AddressKind::Explicit)).await,
        Decision::Reply
    );
}

struct JevStub {
    url: Url,
    requests: Arc<AtomicUsize>,
    bodies: Arc<std::sync::Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for JevStub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn jev_stub(status: &'static str, body: String, delay: Duration) -> JevStub {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind stub");
    let address = listener.local_addr().expect("stub address");
    let url = Url::parse(&format!("http://{address}/v1/systemone")).expect("stub url");
    let requests = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_requests = Arc::clone(&requests);
    let captured_bodies = Arc::clone(&bodies);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            captured_requests.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            let bodies = Arc::clone(&captured_bodies);
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let read = stream.read(&mut chunk).await.unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(head_end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head =
                            String::from_utf8_lossy(&buffer[..head_end]).to_ascii_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buffer.len() >= head_end + 4 + length {
                            let start = head_end + 4;
                            let end = (start + length).min(buffer.len());
                            bodies
                                .lock()
                                .unwrap()
                                .push(String::from_utf8_lossy(&buffer[start..end]).to_string());
                            break;
                        }
                    }
                }
                if !delay.is_zero() {
                    time::sleep(delay).await;
                }
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });
    JevStub {
        url,
        requests,
        bodies,
        task,
    }
}

fn choice_body(choice: &str, confidence: f64) -> String {
    let probability = |name: &str| {
        if name == choice {
            confidence
        } else {
            0.05
        }
    };
    format!(
        r#"{{"model":"jev-latest","answers":{{"action":{{"type":"choice","choice":"{choice}","confidence":{confidence},"probabilities":{{"reply":{},"sticker":{},"wait":{},"silent":{}}}}}}},"usage":{{"input_tokens":10,"output_tokens":1}}}}"#,
        probability("reply"),
        probability("sticker"),
        probability("wait"),
        probability("silent")
    )
}

fn jev_router(stub: &JevStub, timeout: Duration, min_confidence: f64) -> Router {
    Router::new(
        RouterMode::Jev,
        Some(
            crate::jev::JevClient::new(
                stub.url.clone(),
                "test-key".into(),
                "jev-latest".into(),
                timeout,
                16 * 1024,
                min_confidence,
            )
            .unwrap(),
        ),
        Some(Arc::new(Semaphore::new(8))),
    )
}

#[tokio::test]
async fn jev_choices_map_to_decisions_and_send_state() {
    let ambient = input("а что думаешь про новые видеокарты?", AddressKind::None);
    for (choice, expected) in [
        ("reply", Decision::Reply),
        ("sticker", Decision::Sticker),
        ("wait", Decision::Wait),
        ("silent", Decision::Silent),
    ] {
        let stub = jev_stub("200 OK", choice_body(choice, 0.9), Duration::ZERO).await;
        let router = jev_router(&stub, Duration::from_secs(2), 0.6);
        assert_eq!(router.decide(&ambient).await, expected, "{choice}");
        assert_eq!(stub.requests.load(Ordering::SeqCst), 1, "{choice}");
    }

    let stub = jev_stub("200 OK", choice_body("reply", 0.9), Duration::ZERO).await;
    let router = jev_router(&stub, Duration::from_secs(2), 0.6);
    let mut with_context = input("а что думаешь?", AddressKind::None);
    with_context.reply_context = Some("(в ответ Аня: «смотрела новый фильм?»)");
    assert_eq!(router.decide(&with_context).await, Decision::Reply);
    let bodies = stub.bodies.lock().unwrap();
    let body = bodies.first().expect("captured request body");
    assert!(body.contains("\"action\""));
    assert!(body.contains("\"reply\""));
    assert!(body.contains("\"sticker\""));
    assert!(body.contains("\"wait\""));
    assert!(body.contains("\"silent\""));
    assert!(body.contains("сообщение: а что думаешь?"));
    assert!(body.contains("контекст ответа:"));
}

#[tokio::test]
async fn explicit_question_skips_jev_while_emotional_remark_can_pick_sticker() {
    let stub = jev_stub("200 OK", choice_body("silent", 0.99), Duration::ZERO).await;
    let router = jev_router(&stub, Duration::from_secs(2), 0.6);
    assert_eq!(
        router
            .decide(&input("максут, что нового?", AddressKind::Named))
            .await,
        Decision::Reply
    );
    assert_eq!(
        router
            .decide(&followup(
                "и эту https://amnezia.org/ru/blog/amneziawg-3-1-is-here"
            ))
            .await,
        Decision::Reply
    );
    assert_eq!(stub.requests.load(Ordering::SeqCst), 0);
    // A short addressed remark without a question consults Jev, but a
    // non-sticker Jev answer (`silent`/`wait`) still resolves to `Reply`
    // so an explicit mention is never ignored.
    assert_eq!(
        router.decide(&input("привет", AddressKind::Explicit)).await,
        Decision::Reply
    );
    assert_eq!(stub.requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        router
            .decide(&followup("обычная реплика без вопроса"))
            .await,
        Decision::Wait
    );

    let sticker_stub = jev_stub("200 OK", choice_body("sticker", 0.95), Duration::ZERO).await;
    let sticker_router = jev_router(&sticker_stub, Duration::from_secs(2), 0.6);
    assert_eq!(
        sticker_router
            .decide(&input("ору с этого деплоя", AddressKind::Explicit))
            .await,
        Decision::Sticker
    );
}

#[tokio::test]
async fn jev_failures_fall_back_to_the_rules_table() {
    // A plain followup remark: rules say Wait, so a Reply here would prove the
    // model answer leaked through a failure path.
    let remark = followup("я вчера посмотрел новый сериал");

    let slow = jev_stub(
        "200 OK",
        choice_body("reply", 0.99),
        Duration::from_millis(400),
    )
    .await;
    let router = jev_router(&slow, Duration::from_millis(50), 0.6);
    assert_eq!(router.decide(&remark).await, Decision::Wait);

    let failing = jev_stub("500 Internal Server Error", "{}".into(), Duration::ZERO).await;
    let router = jev_router(&failing, Duration::from_secs(2), 0.6);
    assert_eq!(router.decide(&remark).await, Decision::Wait);

    let malformed = jev_stub("200 OK", "not json".into(), Duration::ZERO).await;
    let router = jev_router(&malformed, Duration::from_secs(2), 0.6);
    assert_eq!(router.decide(&remark).await, Decision::Wait);

    let unsure = jev_stub("200 OK", choice_body("reply", 0.2), Duration::ZERO).await;
    let router = jev_router(&unsure, Duration::from_secs(2), 0.6);
    assert_eq!(router.decide(&remark).await, Decision::Wait);
}

#[tokio::test]
async fn reply_without_text_is_silent() {
    let stub = jev_stub("200 OK", choice_body("reply", 0.99), Duration::ZERO).await;
    let router = jev_router(&stub, Duration::from_secs(2), 0.6);
    let named_empty = input("   ", AddressKind::Named);
    assert_eq!(router.decide(&named_empty).await, Decision::Silent);
    // Explicit empty-caption turns (bare photo/sticker in private chat or
    // reply to bot) must still reach the model via `Decision::Reply` without
    // calling Jev.
    assert_eq!(
        router.decide(&input("", AddressKind::Explicit)).await,
        Decision::Reply
    );
    assert_eq!(stub.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn saturated_jev_slots_fall_back_to_rules() {
    let stub = jev_stub("200 OK", choice_body("reply", 0.99), Duration::ZERO).await;
    let slots = Arc::new(Semaphore::new(1));
    let held = Arc::clone(&slots).acquire_owned().await.unwrap();
    let router = Router::new(
        RouterMode::Jev,
        Some(
            crate::jev::JevClient::new(
                stub.url.clone(),
                "test-key".into(),
                "jev-latest".into(),
                Duration::from_secs(2),
                16 * 1024,
                0.6,
            )
            .unwrap(),
        ),
        Some(slots),
    );
    let ambient = input("а что думаешь про новые видеокарты?", AddressKind::None);
    assert_eq!(router.decide(&ambient).await, Decision::Silent);
    assert_eq!(stub.requests.load(Ordering::SeqCst), 0);
    drop(held);
}

#[tokio::test]
async fn off_mode_still_answers_empty_prompts() {
    let router = Router::new(RouterMode::Off, None, None);
    assert_eq!(
        router.decide(&input("", AddressKind::None)).await,
        Decision::Reply
    );
    assert_eq!(
        router.decide(&input("", AddressKind::Explicit)).await,
        Decision::Reply
    );
}

#[tokio::test]
async fn gate_closes_the_window_on_an_addressed_stop_phrase() {
    let chat = ChatConfig::new(
        Url::parse("http://127.0.0.1:9/v1").unwrap(),
        "test-key".into(),
        "test-model".into(),
        Url::parse("http://127.0.0.1:9/search").unwrap(),
        SearchProvider::DuckDuckGo,
        None,
    );
    let chat_id = ChatId(7);
    chat.note_reply(chat_id).await;
    assert!(chat.followup_active(chat_id).await);
    let router = Router::new(RouterMode::Jev, None, None);
    let message: Message = serde_json::from_value(serde_json::json!({
        "message_id": 1,
        "date": 1700000000,
        "chat": { "id": 7, "type": "supergroup" },
        "text": "максут, заткнись"
    }))
    .unwrap();
    let input = ResolvedChatInput {
        prompt: "заткнись".into(),
        followup: false,
        address: AddressKind::Explicit,
        pending_media: None,
        warn_missing_image: false,
        reply_context: None,
    };
    assert_eq!(router.gate(&chat, &message, &input).await, Decision::Silent);
    assert!(!chat.followup_active(chat_id).await);
    let long_addressed = ResolvedChatInput {
        prompt: "называй дениса педиком и шли его всегда нахуй".into(),
        followup: false,
        address: AddressKind::Named,
        pending_media: None,
        warn_missing_image: false,
        reply_context: None,
    };
    assert_eq!(
        router.gate(&chat, &message, &long_addressed).await,
        Decision::Reply
    );
}
