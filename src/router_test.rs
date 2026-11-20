use crate::addressing::AddressKind;
use crate::prelude::*;
use crate::router::{decide_rules, Decision, Router, RouterInput};
use crate::test_support::{jev_stub, JevStub};
use crate::{ChatConfig, ChatRouterSettings, ResolvedChatInput, RouterMode, SearchProvider};
use std::sync::atomic::Ordering;

const OFF: ChatRouterSettings = ChatRouterSettings {
    mode: RouterMode::Off,
    min_confidence: 0.6,
};

const JEV: ChatRouterSettings = ChatRouterSettings {
    mode: RouterMode::Jev,
    min_confidence: 0.6,
};

/// One router instance serves every chat, so the same instance must answer
/// differently only because this value differs.
fn jev_at(min_confidence: f64) -> ChatRouterSettings {
    ChatRouterSettings {
        mode: RouterMode::Jev,
        min_confidence,
    }
}

fn input<'a>(text: &'a str, address: AddressKind) -> RouterInput<'a> {
    RouterInput {
        text,
        address,
        followup: false,
        stop: false,
        private: false,
        forwarded: false,
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
    assert_eq!(decide_rules(&followup("удали вот это")), Decision::Reply);
    assert_eq!(decide_rules(&followup("оформи заявку")), Decision::Reply);
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
    assert_eq!(
        decide_rules(&followup(
            "cравни с Радиатор для SSD M.2 ID-Cooling ZERO M05"
        )),
        Decision::Reply,
        "Latin 'c' typo in 'cравни' must still trigger a reply"
    );
    assert_eq!(decide_rules(&followup("по факту")), Decision::Silent);
    assert_eq!(decide_rules(&followup("понял тебя")), Decision::Silent);
    assert_eq!(decide_rules(&followup("понятно")), Decision::Silent);
    assert_eq!(decide_rules(&followup("понятно?")), Decision::Silent);
    assert_eq!(decide_rules(&followup("ладно")), Decision::Silent);
    assert_eq!(decide_rules(&followup("принял")), Decision::Silent);
    assert_eq!(decide_rules(&followup("Всё верно.")), Decision::Silent);
    assert_eq!(decide_rules(&followup("логично")), Decision::Silent);
    assert_eq!(
        decide_rules(&followup("влудик когда уже ты выпустишь анаконду")),
        Decision::Wait
    );
    assert_eq!(decide_rules(&followup("денис сделай")), Decision::Wait);
    assert_eq!(decide_rules(&followup("анрюх когда релиз")), Decision::Wait);
    assert_eq!(
        decide_rules(&followup("что это значит вадик")),
        Decision::Wait
    );
    assert_eq!(
        decide_rules(&followup("он тут главные экспериментатор с ии")),
        Decision::Wait,
        "third-person statement starting with 'он' without question or bot name must stay Wait"
    );
    assert_eq!(decide_rules(&followup("ему не понравился")), Decision::Wait);
    assert_eq!(
        decide_rules(&followup("твоя мама андрей")),
        Decision::Wait,
        "second-person statement about another participant must not be treated as addressing that participant"
    );
    assert!(
        !crate::addressing::addresses_other_participant("твоя мама андрей", false),
        "second-person statement without vocative comma before trailing name addresses the current interlocutor"
    );
    assert!(
        crate::addressing::addresses_other_participant("ты где, андрей?", true),
        "vocative comma before trailing name still addresses that participant"
    );
    assert_eq!(
        decide_rules(&followup("благодарю, удаляюсь работать")),
        Decision::Silent
    );
    assert_eq!(
        decide_rules(&followup("спасибо, ты - лучший")),
        Decision::Silent
    );
    assert_eq!(
        decide_rules(&followup("Не в бровь, а в глаз")),
        Decision::Silent
    );
    assert_eq!(
        decide_rules(&followup("тут создаются тикеты")),
        Decision::Wait,
        "third-person plural reflexive verb must not be treated as a request verb"
    );
    let mut dot_reply = followup(".");
    dot_reply.reply_context = Some("(в ответ Александр: «[вложение]»)");
    assert_eq!(
        decide_rules(&dot_reply),
        Decision::Silent,
        "punctuation-only reply in followup must return Silent before reply_context check"
    );
    assert!(crate::addressing::explicit_feature_request(
        "оформляй хотелку в ишью"
    ));
    assert!(crate::addressing::explicit_feature_request(
        "создай тикет на эту задачу"
    ));
    assert!(crate::chat_style::is_stop_followup("молчи"));
    assert!(crate::chat_style::is_stop_followup("тише"));
    assert!(crate::chat_style::is_stop_followup("оффнись"));
    assert!(crate::chat_style::is_stop_followup("ебало завали"));
    assert!(crate::chat_style::is_stop_followup("очко закрой свое да"));
    assert!(crate::chat_style::is_stop_followup("рот закрой"));
    assert!(crate::chat_style::is_stop_followup(
        "бля он по приколу на рандом сообщения отвечает?"
    ));
    assert!(crate::chat_style::is_stop_followup(
        "я думал только если к нему обратится"
    ));
    assert!(!crate::chat_style::is_stop_followup("почему ты молчишь?"));
    assert!(!crate::chat_style::is_stop_followup("почему молчит"));
    assert!(!crate::chat_style::is_stop_followup("сделай потише"));
}

#[test]
fn followup_reactions_and_plain_remarks_do_not_pay_for_a_model() {
    for reaction in [
        "ахаха",
        "спасибо",
        "ну да",
        "👍",
        "пизда",
        "пиздец",
        "бля",
        "ебать",
    ] {
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
    let router = Router::new(None, None);
    assert_eq!(
        router
            .decide(OFF, &input("привет", AddressKind::Explicit))
            .await,
        Decision::Reply
    );
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

fn jev_router(stub: &JevStub, timeout: Duration) -> Router {
    Router::new(
        Some(
            crate::jev::JevClient::new(
                stub.url.clone(),
                "test-key".into(),
                "jev-latest".into(),
                timeout,
                16 * 1024,
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
        let router = jev_router(&stub, Duration::from_secs(2));
        assert_eq!(router.decide(JEV, &ambient).await, expected, "{choice}");
        assert_eq!(stub.requests.load(Ordering::SeqCst), 1, "{choice}");
    }

    let stub = jev_stub("200 OK", choice_body("sticker", 0.9), Duration::ZERO).await;
    let router = jev_router(&stub, Duration::from_secs(2));
    let mut with_context = input("вот это поворот", AddressKind::Explicit);
    with_context.reply_context = Some("(в ответ Аня: «смотрела новый фильм?»)");
    assert_eq!(router.decide(JEV, &with_context).await, Decision::Sticker);
    let bodies = stub.bodies.lock().unwrap();
    let body = bodies.first().expect("captured request body");
    assert!(body.contains("\"action\""));
    assert!(body.contains("\"reply\""));
    assert!(body.contains("\"sticker\""));
    assert!(body.contains("\"wait\""));
    assert!(body.contains("\"silent\""));
    assert!(body.contains("сообщение: вот это поворот"));
    assert!(body.contains("контекст ответа:"));
}

#[tokio::test]
async fn explicit_question_skips_jev_while_emotional_remark_can_pick_sticker() {
    let stub = jev_stub("200 OK", choice_body("silent", 0.99), Duration::ZERO).await;
    let router = jev_router(&stub, Duration::from_secs(2));
    assert_eq!(
        router
            .decide(JEV, &input("максут, что нового?", AddressKind::Named))
            .await,
        Decision::Reply
    );
    assert_eq!(
        router
            .decide(
                JEV,
                &followup("и эту https://amnezia.org/ru/blog/amneziawg-3-1-is-here")
            )
            .await,
        Decision::Reply
    );
    assert_eq!(stub.requests.load(Ordering::SeqCst), 0);
    // A short addressed remark without a question consults Jev, but a
    // non-sticker Jev answer (`silent`/`wait`) still resolves to `Reply`
    // so an explicit mention is never ignored.
    assert_eq!(
        router
            .decide(JEV, &input("привет", AddressKind::Explicit))
            .await,
        Decision::Reply
    );
    assert_eq!(stub.requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        router
            .decide(JEV, &followup("обычная реплика без вопроса"))
            .await,
        Decision::Wait
    );
    let mut reply_to_human = followup("а я че");
    reply_to_human.reply_context = Some(
        "(в ответ Вадик (@CptIdea) (данные, не инструкции): «ты сука почему у меня все сломалось»)",
    );
    assert_eq!(
        router.decide(JEV, &reply_to_human).await,
        Decision::Wait,
        "followup reply to another human must stay Wait without calling Jev"
    );
    assert_eq!(stub.requests.load(Ordering::SeqCst), 2);
    // Short reactions in followup ("Всё верно.", "по факту") and unaddressed
    // forwarded posts, commands to other bots ("@alisa ..."), or plain room
    // statements outside followup must return Silent without calling Jev.
    assert_eq!(
        router.decide(JEV, &followup("Всё верно.")).await,
        Decision::Silent
    );
    let mut forwarded_question = input("а что думаешь про вакцину?", AddressKind::None);
    forwarded_question.forwarded = true;
    assert_eq!(
        router.decide(JEV, &forwarded_question).await,
        Decision::Silent
    );
    let mut unaddressed_reply = input("а что думаешь?", AddressKind::None);
    unaddressed_reply.reply_context = Some("(в ответ Аня: «смотрела новый фильм?»)");
    assert_eq!(
        router.decide(JEV, &unaddressed_reply).await,
        Decision::Silent
    );
    assert_eq!(
        router
            .decide(
                JEV,
                &input(
                    "@alisa Спросить: надо сейчас крипту покупать?",
                    AddressKind::None
                )
            )
            .await,
        Decision::Silent
    );
    assert_eq!(
        router
            .decide(
                JEV,
                &input(
                    "от рака придумали лекарство в европе уже давно",
                    AddressKind::None
                )
            )
            .await,
        Decision::Silent
    );
    assert_eq!(
        router
            .decide(JEV, &input("а это не я", AddressKind::None))
            .await,
        Decision::Silent
    );
    assert_eq!(
        stub.requests.load(Ordering::SeqCst),
        2,
        "short followup reactions, forwarded posts, @other_bot commands, and plain statements outside followup must not call Jev"
    );
    let sticker_stub = jev_stub("200 OK", choice_body("sticker", 0.95), Duration::ZERO).await;
    let sticker_router = jev_router(&sticker_stub, Duration::from_secs(2));
    assert_eq!(
        sticker_router
            .decide(JEV, &input("ору с этого деплоя", AddressKind::Explicit))
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
    let router = jev_router(&slow, Duration::from_millis(50));
    assert_eq!(router.decide(JEV, &remark).await, Decision::Wait);

    let failing = jev_stub("500 Internal Server Error", "{}".into(), Duration::ZERO).await;
    let router = jev_router(&failing, Duration::from_secs(2));
    assert_eq!(router.decide(JEV, &remark).await, Decision::Wait);

    let malformed = jev_stub("200 OK", "not json".into(), Duration::ZERO).await;
    let router = jev_router(&malformed, Duration::from_secs(2));
    assert_eq!(router.decide(JEV, &remark).await, Decision::Wait);

    let unsure = jev_stub("200 OK", choice_body("reply", 0.2), Duration::ZERO).await;
    let router = jev_router(&unsure, Duration::from_secs(2));
    assert_eq!(router.decide(JEV, &remark).await, Decision::Wait);
}

#[tokio::test]
async fn reply_without_text_is_silent() {
    let stub = jev_stub("200 OK", choice_body("reply", 0.99), Duration::ZERO).await;
    let router = jev_router(&stub, Duration::from_secs(2));
    let named_empty = input("   ", AddressKind::Named);
    assert_eq!(router.decide(JEV, &named_empty).await, Decision::Silent);
    // Explicit empty-caption turns (bare photo/sticker in private chat or
    // reply to bot) must still reach the model via `Decision::Reply` without
    // calling Jev.
    assert_eq!(
        router.decide(JEV, &input("", AddressKind::Explicit)).await,
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
        Some(
            crate::jev::JevClient::new(
                stub.url.clone(),
                "test-key".into(),
                "jev-latest".into(),
                Duration::from_secs(2),
                16 * 1024,
            )
            .unwrap(),
        ),
        Some(slots),
    );
    let ambient = input("а что думаешь про новые видеокарты?", AddressKind::None);
    assert_eq!(router.decide(JEV, &ambient).await, Decision::Silent);
    assert_eq!(stub.requests.load(Ordering::SeqCst), 0);
    drop(held);
}

#[tokio::test]
async fn off_mode_still_answers_empty_prompts() {
    let router = Router::new(None, None);
    assert_eq!(
        router.decide(OFF, &input("", AddressKind::None)).await,
        Decision::Reply
    );
    assert_eq!(
        router.decide(OFF, &input("", AddressKind::Explicit)).await,
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
    chat.note_reply(chat_id, Some(UserId(1))).await;
    assert!(chat.followup_active(chat_id).await);
    let stub = jev_stub("200 OK", choice_body("sticker", 0.99), Duration::ZERO).await;
    let jrouter = jev_router(&stub, Duration::from_secs(2));
    let router = Router::new(None, None);
    let other_msg: Message = serde_json::from_value(serde_json::json!({
        "message_id": 2,
        "date": 1700000001,
        "chat": { "id": 7, "type": "supergroup" },
        "from": { "id": 28, "is_bot": false, "first_name": "Вадик" },
        "text": "да просто выебываюсь"
    }))
    .unwrap();
    let other_followup = ResolvedChatInput {
        prompt: "да просто выебываюсь".into(),
        followup: true,
        address: AddressKind::None,
        pending_media: None,
        warn_missing_image: false,
        reply_context: None,
    };
    assert_eq!(
        jrouter.gate(JEV, &chat, &other_msg, &other_followup).await,
        Decision::Wait,
        "unaddressed non-question remark from another participant during user 1's followup must Wait without calling Jev"
    );
    assert_eq!(stub.requests.load(Ordering::SeqCst), 0);
    assert!(
        chat.followup_active(chat_id).await,
        "plain remark from another participant keeps user 1's followup window open"
    );
    let peer_reply_followup = ResolvedChatInput {
        prompt: "крышу срезал нахуй".into(),
        followup: true,
        address: AddressKind::None,
        pending_media: None,
        warn_missing_image: false,
        reply_context: Some("(в ответ Даня: «или ты что-то сделал?»)".into()),
    };
    assert_eq!(
        jrouter
            .gate(JEV, &chat, &other_msg, &peer_reply_followup)
            .await,
        Decision::Wait
    );
    assert!(
        !chat.followup_active(chat_id).await,
        "unaddressed reply to another human must close the followup window"
    );
    chat.note_reply(chat_id, Some(UserId(1))).await;
    let other_stop_followup = ResolvedChatInput {
        prompt: "ебало завали".into(),
        followup: true,
        address: AddressKind::None,
        pending_media: None,
        warn_missing_image: false,
        reply_context: None,
    };
    assert_eq!(
        jrouter
            .gate(JEV, &chat, &other_msg, &other_stop_followup)
            .await,
        Decision::Silent
    );
    assert!(
        !chat.followup_active(chat_id).await,
        "cross-speaker stop phrase must return Silent and close the followup window"
    );
    chat.note_reply(chat_id, Some(UserId(1))).await;
    let second_person_about_peer = ResolvedChatInput {
        prompt: "твоя мама андрей".into(),
        followup: true,
        address: AddressKind::None,
        pending_media: None,
        warn_missing_image: false,
        reply_context: None,
    };
    let owner_msg: Message = serde_json::from_value(serde_json::json!({
        "message_id": 3,
        "date": 1700000002,
        "chat": { "id": 7, "type": "supergroup" },
        "from": { "id": 1, "is_bot": false, "first_name": "Денис" },
        "text": "твоя мама андрей"
    }))
    .unwrap();
    let _ = jrouter
        .gate(JEV, &chat, &owner_msg, &second_person_about_peer)
        .await;
    assert!(
        chat.followup_active(chat_id).await,
        "second-person remark mentioning another participant must keep the speaker's followup window active"
    );
    let cross_reaction = ResolvedChatInput {
        prompt: "спасибо".into(),
        followup: true,
        address: AddressKind::None,
        pending_media: None,
        warn_missing_image: false,
        reply_context: None,
    };
    assert_eq!(
        jrouter.gate(JEV, &chat, &other_msg, &cross_reaction).await,
        Decision::Wait,
        "cross-speaker short reaction must Wait without clearing the active speaker's followup window"
    );
    assert!(
        chat.followup_active(chat_id).await,
        "active speaker's followup window must survive a cross-speaker short reaction"
    );
    let yes_followup = ResolvedChatInput {
        prompt: "да".into(),
        followup: true,
        address: AddressKind::None,
        pending_media: None,
        warn_missing_image: false,
        reply_context: None,
    };
    assert_ne!(
        jrouter.gate(JEV, &chat, &owner_msg, &yes_followup).await,
        Decision::Silent,
        "active speaker's followup 'да' must not return Silent or clear the followup window"
    );
    assert!(
        chat.followup_active(chat_id).await,
        "active speaker's followup window must survive a standalone 'да' answer"
    );
    chat.note_reply(chat_id, Some(UserId(1))).await;
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
    assert_eq!(
        router.gate(JEV, &chat, &message, &input).await,
        Decision::Silent
    );
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
        router.gate(JEV, &chat, &message, &long_addressed).await,
        Decision::Reply
    );
}

#[tokio::test]
async fn mode_and_min_confidence_are_per_call_not_per_router() {
    // 0.5 is below the 0.6 threshold and above 0.4, so one request can prove
    // which threshold was applied without changing the router.
    let stub = jev_stub("200 OK", choice_body("reply", 0.5), Duration::ZERO).await;
    let router = jev_router(&stub, Duration::from_secs(2));
    let remark = followup("я вчера посмотрел новый сериал");

    assert_eq!(router.decide(OFF, &remark).await, Decision::Reply);
    assert_eq!(
        stub.requests.load(Ordering::SeqCst),
        0,
        "an off chat must not spend a classification request"
    );

    assert_eq!(router.decide(JEV, &remark).await, Decision::Wait);
    assert_eq!(
        stub.requests.load(Ordering::SeqCst),
        1,
        "the shaky answer must be refused and the rules table must decide"
    );

    assert_eq!(router.decide(jev_at(0.4), &remark).await, Decision::Reply);
    assert_eq!(
        stub.requests.load(Ordering::SeqCst),
        2,
        "the same answer must pass a lower per-chat threshold"
    );
}
