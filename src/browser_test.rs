use crate::browser::navigation_headers;

fn headers() -> reqwest::header::HeaderMap {
    navigation_headers(reqwest::Client::new().get("https://example.com/"))
        .build()
        .unwrap()
        .headers()
        .clone()
}

#[test]
fn navigation_headers_match_the_firefox_profile() {
    let headers = headers();
    assert_eq!(
        headers[reqwest::header::USER_AGENT],
        "Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0"
    );
    assert_eq!(
        headers[reqwest::header::ACCEPT],
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
    );
    assert_eq!(headers[reqwest::header::ACCEPT_LANGUAGE], "ru,en;q=0.8");
    assert_eq!(headers["sec-fetch-dest"], "document");
    assert_eq!(headers["sec-fetch-mode"], "navigate");
    assert_eq!(headers["sec-fetch-site"], "none");
    assert_eq!(headers["sec-fetch-user"], "?1");
    assert_eq!(headers["upgrade-insecure-requests"], "1");
}
