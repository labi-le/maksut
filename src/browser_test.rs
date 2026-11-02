use crate::browser::{navigation_headers, referer_site, HTML_ACCEPT, NONE_SITE};
use crate::prelude::*;

fn headers(url: &str, site: Option<reqwest::header::HeaderValue>) -> reqwest::header::HeaderMap {
    navigation_headers(reqwest::Client::new().get(url), HTML_ACCEPT, site)
        .build()
        .unwrap()
        .headers()
        .clone()
}

#[test]
fn navigation_headers_match_the_firefox_profile() {
    let headers = headers("https://example.com/", Some(NONE_SITE));
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

#[test]
fn insecure_navigation_skips_fetch_metadata() {
    let headers = headers("http://example.com/", None);
    assert!(!headers.contains_key("sec-fetch-dest"));
    assert!(!headers.contains_key("sec-fetch-mode"));
    assert!(!headers.contains_key("sec-fetch-site"));
    assert!(!headers.contains_key("sec-fetch-user"));
    assert_eq!(headers["upgrade-insecure-requests"], "1");
}

#[test]
fn referer_site_follows_the_initiator_origin() {
    let target = Url::parse("https://html.duckduckgo.com/html/").unwrap();
    assert_eq!(
        referer_site(&target, "https://html.duckduckgo.com/"),
        "same-origin"
    );
    assert_eq!(
        referer_site(&target, "https://www.startpage.com/"),
        "cross-site"
    );
    assert_eq!(
        referer_site(&target, "http://html.duckduckgo.com/"),
        "cross-site"
    );
    let port = Url::parse("https://html.duckduckgo.com:8443/html/").unwrap();
    assert_eq!(
        referer_site(&port, "https://html.duckduckgo.com/"),
        "same-site"
    );
    assert_eq!(referer_site(&target, "not a url"), "cross-site");
}
