use crate::bounded_fetch::{fetch_until, retryable, FetchCategory, FetchError};
use crate::prelude::*;
use crate::retry::RetryPolicy;

fn error(category: FetchCategory, status: Option<u16>) -> FetchError {
    FetchError {
        category,
        status,
        host: None,
        path: None,
        message: String::new(),
    }
}

#[test]
fn display_and_string_conversion_carry_only_the_user_text() {
    let failure = FetchError {
        category: FetchCategory::Http,
        status: Some(503),
        host: Some("example.com".into()),
        path: Some("/feed".into()),
        message: "хост ответил HTTP 503".into(),
    };
    assert_eq!(failure.to_string(), "хост ответил HTTP 503");
    assert_eq!(failure.user_reason(), failure.to_string());
    let text: String = failure.clone().into();
    assert_eq!(text, "хост ответил HTTP 503");
}

#[test]
fn locating_an_error_keeps_host_and_path_but_drops_the_query() {
    let url = Url::parse("https://example.com/search?q=term&token=bar").unwrap();
    let failure = FetchError::new(FetchCategory::Http, "хост ответил HTTP 500").at(&url);
    assert_eq!(failure.host.as_deref(), Some("example.com"));
    assert_eq!(failure.path.as_deref(), Some("/search"));
    assert!(!failure.to_string().contains("term"));
    assert!(!failure.path.unwrap_or_default().contains('?'));
}

#[test]
fn retry_matches_policy_statuses_and_the_transport_flag() {
    let policy = RetryPolicy {
        attempts: 3,
        base_backoff_ms: 1,
        max_backoff_ms: 1,
        retry_statuses: vec![429, 500],
        retry_transport: true,
    };
    for status in [429, 500] {
        assert!(
            retryable(&policy, &error(FetchCategory::Http, Some(status))),
            "listed status {status} must retry"
        );
    }
    for status in [404, 503] {
        assert!(
            !retryable(&policy, &error(FetchCategory::Http, Some(status))),
            "unlisted status {status} must not retry"
        );
    }
    assert!(!retryable(&policy, &error(FetchCategory::Http, None)));
    assert!(retryable(&policy, &error(FetchCategory::Transport, None)));
    let no_transport_retry = RetryPolicy {
        retry_transport: false,
        ..policy.clone()
    };
    assert!(!retryable(
        &no_transport_retry,
        &error(FetchCategory::Transport, None)
    ));
    for category in [
        FetchCategory::Timeout,
        FetchCategory::TooLarge,
        FetchCategory::Redirect,
        FetchCategory::InvalidUrl,
    ] {
        assert!(
            !retryable(&policy, &error(category, None)),
            "{category:?} must never retry"
        );
    }
}

#[tokio::test]
async fn an_exhausted_deadline_fails_before_touching_the_network() {
    let url = Url::parse("https://example.com/search?q=term").unwrap();
    let failure = fetch_until(url, "*/*", None, 1024, time::Instant::now())
        .await
        .err()
        .expect("expired deadline must fail");
    assert_eq!(failure.category, FetchCategory::Timeout);
    assert_eq!(failure.user_reason(), "таймаут");
    assert_eq!(failure.host.as_deref(), Some("example.com"));
    assert_eq!(failure.path.as_deref(), Some("/search"));
    assert_eq!(failure.status, None);
}

fn retry_policy() -> RetryPolicy {
    RetryPolicy {
        attempts: 2,
        base_backoff_ms: 1,
        max_backoff_ms: 1,
        retry_statuses: vec![429],
        retry_transport: false,
    }
}

#[tokio::test]
async fn retry_attempts_retries_a_429_then_succeeds() {
    let calls = std::cell::Cell::new(0u32);
    let deadline = time::Instant::now() + Duration::from_secs(1);
    let result: Result<u8, FetchError> =
        crate::bounded_fetch::retry_attempts(&retry_policy(), deadline, || {
            let call = calls.get();
            calls.set(call + 1);
            async move {
                if call == 0 {
                    Err(error(FetchCategory::Http, Some(429)))
                } else {
                    Ok(7)
                }
            }
        })
        .await;
    assert_eq!(result.expect("second attempt must succeed"), 7);
    assert_eq!(calls.get(), 2);
}

#[tokio::test]
async fn retry_attempts_stops_after_the_policy_budget() {
    let calls = std::cell::Cell::new(0u32);
    let deadline = time::Instant::now() + Duration::from_secs(1);
    let result: Result<u8, FetchError> =
        crate::bounded_fetch::retry_attempts(&retry_policy(), deadline, || {
            calls.set(calls.get() + 1);
            async move { Err(error(FetchCategory::Http, Some(429))) }
        })
        .await;
    assert_eq!(
        result.expect_err("both attempts must fail").status,
        Some(429)
    );
    assert_eq!(calls.get(), 2);
}

#[tokio::test]
async fn retry_attempts_does_not_retry_a_404() {
    let calls = std::cell::Cell::new(0u32);
    let deadline = time::Instant::now() + Duration::from_secs(1);
    let result: Result<u8, FetchError> =
        crate::bounded_fetch::retry_attempts(&retry_policy(), deadline, || {
            calls.set(calls.get() + 1);
            async move { Err(error(FetchCategory::Http, Some(404))) }
        })
        .await;
    assert_eq!(result.expect_err("404 must fail").status, Some(404));
    assert_eq!(calls.get(), 1);
}
