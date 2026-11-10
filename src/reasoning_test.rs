use crate::jev::JevClient;
use crate::test_support::{jev_stub, JevStub};
use crate::*;
use std::sync::atomic::Ordering;

fn policy() -> ReasoningPolicy {
    ReasoningPolicy {
        default: Effort::Low,
        max: Effort::High,
    }
}

fn effort_body(choice: &str, confidence: f64) -> String {
    format!(
        r#"{{"answers":{{"effort":{{"choice":"{choice}","confidence":{confidence},"probabilities":{{"{choice}":{confidence}}}}}}}}}"#
    )
}

fn classifier(stub: &JevStub, timeout: Duration, min_confidence: f64) -> JevClient {
    JevClient::new(
        stub.url.clone(),
        "test-key".into(),
        "jev-latest".into(),
        timeout,
        16 * 1024,
        min_confidence,
    )
    .unwrap()
}

fn judge(
    mode: ReasoningMode,
    classifier: Option<JevClient>,
    slots: Option<Arc<Semaphore>>,
) -> ReasoningJudge {
    ReasoningJudge {
        mode,
        classifier,
        slots,
    }
}

#[test]
fn tiers_parse_case_insensitively_and_strictly() {
    assert_eq!(Effort::parse(" high "), Some(Effort::High));
    assert_eq!(Effort::parse("NONE"), Some(Effort::None));
    assert_eq!(Effort::parse("xhigh"), None);
    assert_eq!(Effort::parse(""), None);
    assert_eq!(Effort::High.as_str(), "high");
}

#[test]
fn policy_clamps_to_the_floor_and_the_ceiling() {
    let policy = policy();
    assert_eq!(policy.resolve(Some(Effort::None)), Effort::Low);
    assert_eq!(policy.resolve(Some(Effort::High)), Effort::High);
    assert_eq!(policy.resolve(None), Effort::Low);

    let capped = ReasoningPolicy {
        default: Effort::High,
        max: Effort::Medium,
    };
    assert_eq!(capped.resolve(Some(Effort::High)), Effort::Medium);
    assert_eq!(capped.resolve(None), Effort::Medium);
}

#[test]
fn reasoning_mode_maps_settings() {
    let shipped = LlmSettings::default();
    assert_eq!(
        shipped.reasoning_effort.as_deref(),
        Some("auto"),
        "the shipped default must classify each turn"
    );
    assert!(matches!(
        ReasoningMode::from_settings(&shipped),
        ReasoningMode::Auto(_)
    ));
    assert!(matches!(
        ReasoningMode::from_settings(&LlmSettings {
            reasoning_effort: Some("AUTO".into()),
            ..Default::default()
        }),
        ReasoningMode::Auto(_)
    ));

    for off in [Some("none"), Some(""), Some("  "), Some(" None "), None] {
        let settings = LlmSettings {
            reasoning_effort: off.map(str::to_owned),
            ..Default::default()
        };
        assert!(
            !ReasoningMode::from_settings(&settings).is_configured(),
            "{off:?}"
        );
    }

    assert!(matches!(
        ReasoningMode::from_settings(&LlmSettings {
            reasoning_effort: Some("minimal".into()),
            ..Default::default()
        }),
        ReasoningMode::Fixed(value) if value == "minimal"
    ));
}

#[test]
fn shipped_mapping_is_valid_and_rejects_bad_tiers() {
    assert!(ReasoningSettings::default().validate().is_ok());
    for broken in [
        ReasoningSettings {
            default: "slow".into(),
            ..Default::default()
        },
        ReasoningSettings {
            max: "max".into(),
            ..Default::default()
        },
        ReasoningSettings {
            default: "none".into(),
            ..Default::default()
        },
        ReasoningSettings {
            max: "none".into(),
            ..Default::default()
        },
        ReasoningSettings {
            min_confidence: 1.5,
            ..Default::default()
        },
        ReasoningSettings {
            url: "file:///etc/passwd".into(),
            ..Default::default()
        },
        ReasoningSettings {
            concurrency: 0,
            ..Default::default()
        },
    ] {
        assert!(broken.validate().is_err(), "{broken:?}");
    }
}

#[tokio::test]
async fn classifier_reads_the_effort_answer_and_sends_the_ladder() {
    let stub = jev_stub("200 OK", effort_body("high", 0.9), Duration::ZERO).await;
    let client = classifier(&stub, Duration::from_secs(2), 0.6);
    assert_eq!(
        classify(&client, "объясни, почему это падает").await,
        Some(Effort::High)
    );

    let bodies = stub.bodies.lock().unwrap();
    let body = bodies.first().expect("captured classifier body");
    assert!(body.contains("\"effort\""), "{body}");
    assert!(body.contains("\"low\"") && body.contains("\"medium\"") && body.contains("\"high\""));
    assert!(
        !body.contains("\"max\""),
        "the ladder must not offer a tier the policy never returns: {body}"
    );
    assert!(
        body.contains("\"state\":\"объясни, почему это падает\""),
        "{body}"
    );
}

#[tokio::test]
async fn classifier_rejects_unsure_and_unknown_answers() {
    let unsure = jev_stub("200 OK", effort_body("high", 0.2), Duration::ZERO).await;
    let client = classifier(&unsure, Duration::from_secs(2), 0.6);
    assert_eq!(classify(&client, "привет").await, None);

    let unknown = jev_stub("200 OK", effort_body("max", 0.99), Duration::ZERO).await;
    let client = classifier(&unknown, Duration::from_secs(2), 0.6);
    assert_eq!(classify(&client, "привет").await, None);
}

#[tokio::test]
async fn classifier_failures_and_timeouts_fall_back_to_the_default() {
    let failing = jev_stub("500 Internal Server Error", "{}".into(), Duration::ZERO).await;
    let client = classifier(&failing, Duration::from_secs(2), 0.6);
    assert_eq!(
        judge(ReasoningMode::Auto(policy()), Some(client), None)
            .resolve("объясни")
            .await,
        Some(Effort::Low)
    );

    let slow = jev_stub(
        "200 OK",
        effort_body("high", 0.99),
        Duration::from_millis(300),
    )
    .await;
    let client = classifier(&slow, Duration::from_millis(50), 0.6);
    assert_eq!(
        judge(ReasoningMode::Auto(policy()), Some(client), None)
            .resolve("объясни")
            .await,
        Some(Effort::Low)
    );
}

#[tokio::test]
async fn busy_classifier_slots_skip_the_request() {
    let stub = jev_stub("200 OK", effort_body("high", 0.9), Duration::ZERO).await;
    let client = classifier(&stub, Duration::from_secs(2), 0.6);
    let slots = Arc::new(Semaphore::new(1));
    let held = slots.clone().try_acquire_owned().unwrap();
    assert_eq!(
        judge(
            ReasoningMode::Auto(policy()),
            Some(client.clone()),
            Some(slots.clone())
        )
        .resolve("объясни")
        .await,
        Some(Effort::Low)
    );
    assert_eq!(stub.requests.load(Ordering::SeqCst), 0);
    drop(held);
    assert_eq!(
        judge(ReasoningMode::Auto(policy()), Some(client), Some(slots))
            .resolve("объясни")
            .await,
        Some(Effort::High)
    );
}

#[tokio::test]
async fn a_pinned_ceiling_skips_the_classifier_request() {
    let stub = jev_stub("200 OK", effort_body("high", 0.9), Duration::ZERO).await;
    let client = classifier(&stub, Duration::from_secs(2), 0.6);
    let pinned = ReasoningPolicy {
        default: Effort::Low,
        max: Effort::Low,
    };
    assert_eq!(
        judge(ReasoningMode::Auto(pinned), Some(client), None)
            .resolve("объясни")
            .await,
        Some(Effort::Low)
    );
    assert_eq!(stub.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fixed_and_off_modes_do_not_classify() {
    assert_eq!(
        judge(ReasoningMode::Fixed("high".into()), None, None)
            .resolve("x")
            .await,
        None
    );
    assert_eq!(
        judge(ReasoningMode::Off, None, None).resolve("x").await,
        None
    );
    assert_eq!(
        judge(ReasoningMode::Auto(policy()), None, None)
            .resolve("x")
            .await,
        Some(Effort::Low),
        "auto without a reachable classifier keeps the turn alive on the default tier"
    );
}

#[tokio::test]
async fn an_attached_classifier_reaches_the_chat_config() {
    let stub = jev_stub("200 OK", effort_body("high", 0.9), Duration::ZERO).await;
    let client = classifier(&stub, Duration::from_secs(2), 0.6);
    let base = Url::parse("http://127.0.0.1:1/v1").unwrap();
    let chat = ChatConfig::new(
        base.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        base,
        SearchProvider::DuckDuckGo,
        None,
    )
    .with_classifier(Some(client), Some(Arc::new(Semaphore::new(4))));
    assert_eq!(chat.judge().resolve("объясни").await, Some(Effort::High));
    assert_eq!(stub.requests.load(Ordering::SeqCst), 1);
}
