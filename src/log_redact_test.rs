fn sanitize(line: &str) -> String {
    crate::log_redact::sanitize_line(line, &[])
}

#[test]
fn url_query_strings_become_a_mask() {
    assert_eq!(
        sanitize("read_url GET https://example.com/page?token=abc&x=1 done"),
        "read_url GET https://example.com/page?[redacted] done",
    );
}

#[test]
fn every_url_in_a_line_loses_its_query() {
    assert_eq!(
        sanitize("a https://one.example/x?q=1 b http://two.example/y?z=2"),
        "a https://one.example/x?[redacted] b http://two.example/y?[redacted]",
    );
}

#[test]
fn urls_without_query_stay_intact() {
    assert_eq!(
        sanitize("GET https://example.com/path ok"),
        "GET https://example.com/path ok",
    );
}

#[test]
fn configured_secret_values_are_masked() {
    let secrets = vec!["s3cr3t-token-value".to_owned()];
    assert_eq!(
        crate::log_redact::sanitize_line("auth s3cr3t-token-value used", &secrets),
        "auth [redacted] used",
    );
}

#[test]
fn credential_like_tokens_are_masked() {
    assert_eq!(sanitize("db token=abc123 done"), "db [redacted] done");
    let masked = sanitize("llm error sk-live-1234567890abcdef body");
    assert_eq!(masked, "llm error [redacted] body");
}

#[test]
fn long_lines_are_truncated_to_the_line_cap() {
    let out = sanitize(&"x".repeat(1000));
    assert_eq!(out.chars().count(), crate::log_redact::LINE_MAX_CHARS + 1);
    assert!(out.ends_with('…'), "{out}");
}

#[test]
fn the_sanitized_prefix_is_bounded_before_matching() {
    let line = format!("{} token=abc123", "x".repeat(5000));
    let out = sanitize(&line);
    assert!(!out.contains("token"), "{out}");
    assert!(out.chars().count() <= crate::log_redact::LINE_MAX_CHARS + 1);
}

#[test]
fn plain_lines_survive_unchanged_without_the_trailing_newline() {
    assert_eq!(sanitize("chat reply failed\n"), "chat reply failed");
}
