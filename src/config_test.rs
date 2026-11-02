use crate::*;

#[test]
fn defaults_validate() {
    Config::default().validate().unwrap();
}

#[test]
fn example_config_matches_defaults_and_validates() {
    let parsed = Config::parse(include_str!("../example.config.toml")).unwrap();
    parsed.validate().unwrap();
    assert_eq!(parsed.chat.tool_rounds, Config::default().chat.tool_rounds);
    assert_eq!(
        parsed.read_url.max_bytes,
        Config::default().read_url.max_bytes
    );
    assert_eq!(
        parsed.retry.search.attempts,
        Config::default().retry.search.attempts
    );
}

#[test]
fn partial_file_keeps_other_defaults() {
    let parsed = Config::parse("[chat]\ntool_rounds = 3\n").unwrap();
    assert_eq!(parsed.chat.tool_rounds, 3);
    assert_eq!(parsed.chat.max_tokens, Config::default().chat.max_tokens);
}

#[test]
fn unknown_key_is_rejected() {
    let error = Config::parse("[chat]\nunknown_key = 1\n").unwrap_err();
    assert!(error.contains("unknown_key"), "{error}");
}

#[test]
fn zero_retry_attempts_are_rejected() {
    let mut cfg = Config::default();
    cfg.retry.llm.attempts = 0;
    let error = cfg.validate().unwrap_err();
    assert!(error.contains("retry.llm.attempts"), "{error}");
}

#[test]
fn media_bytes_must_fit_the_telegram_limit() {
    let mut cfg = Config::default();
    cfg.media.max_bytes = cfg.telegram.file_limit_bytes + 1;
    assert!(cfg.validate().is_err());
}

#[test]
fn html_bounds_cannot_be_disabled() {
    let mut cfg = Config::default();
    cfg.read_url.max_html_elements = 10;
    assert!(cfg.validate().is_err());
    cfg.read_url.max_html_elements = Config::default().read_url.max_html_elements;
    cfg.read_url.max_redirects = 0;
    assert!(cfg.validate().is_err());
}

#[tokio::test]
async fn explicit_missing_config_path_is_an_error() {
    std::env::set_var("MAKSUT_CONFIG", "/nonexistent/maksut-config.toml");
    let error = Config::load().await.unwrap_err();
    std::env::remove_var("MAKSUT_CONFIG");
    assert!(error.contains("maksut-config.toml"), "{error}");
}

#[test]
fn partial_retry_table_is_rejected() {
    let error = Config::parse("[retry.search]\nattempts = 3\n").unwrap_err();
    assert!(error.contains("base_backoff_ms"), "{error}");
}

#[test]
fn redirect_cap_cannot_widen_the_documented_bound() {
    let mut cfg = Config::default();
    cfg.read_url.max_redirects = 6;
    assert!(cfg.validate().is_err());
}

#[test]
fn search_deadlines_stay_ordered() {
    let mut cfg = Config::default();
    cfg.search.public_soft_deadline_secs = cfg.search.public_hard_deadline_secs + 1;
    assert!(cfg.validate().is_err());
}

#[test]
fn provider_aliases_parse() {
    assert_eq!(
        SearchProvider::from_name("ddg"),
        Some(SearchProvider::DuckDuckGo)
    );
    assert_eq!(
        SearchProvider::from_name("duckduckgo"),
        Some(SearchProvider::DuckDuckGo)
    );
    assert_eq!(SearchProvider::from_name("nope"), None);
    let parsed = Config::parse("[search]\nprovider = \"exa\"\n").unwrap();
    assert_eq!(parsed.search.provider, SearchProvider::Exa);
    assert!(Config::parse("[search]\nprovider = \"nope\"\n").is_err());
}

#[test]
fn retry_backoff_grows_then_caps() {
    let policy = RetryPolicy {
        attempts: 5,
        base_backoff_ms: 100,
        max_backoff_ms: 250,
        retry_statuses: vec![503],
        retry_transport: true,
    };
    assert_eq!(policy.delay(0), Duration::from_millis(100));
    assert_eq!(policy.delay(1), Duration::from_millis(200));
    assert_eq!(policy.delay(2), Duration::from_millis(250));
    assert!(policy.should_retry(0));
    assert!(policy.should_retry(3));
    assert!(!policy.should_retry(4));
    assert!(policy.retry_status(503));
    assert!(!policy.retry_status(404));
}

#[test]
fn backoff_must_not_start_above_its_cap() {
    let mut cfg = Config::default();
    cfg.retry.read_url.base_backoff_ms = 5000;
    cfg.retry.read_url.max_backoff_ms = 100;
    let error = cfg.validate().unwrap_err();
    assert!(error.contains("retry.read_url.max_backoff_ms"), "{error}");
}

#[test]
fn per_attempt_timeout_must_fit_the_total() {
    let mut cfg = Config::default();
    cfg.read_url.timeout_secs = cfg.read_url.total_timeout_secs + 1;
    assert!(cfg.validate().is_err());
}
