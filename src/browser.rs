//! Browser identity for the two tools that fetch public HTML on the user's
//! behalf (`read_url` and DuckDuckGo `web_search`). A site that only inspects
//! `User-Agent` otherwise answers the honest `Maksut/0.1` string with a block
//! page. Header emulation stops at the wire format: no cookies, no JS, no TLS
//! fingerprint, so JS/TLS challenges still reject these requests (ADR 019).

use reqwest::header::{HeaderName, HeaderValue};

pub(crate) const USER_AGENT: HeaderValue = HeaderValue::from_static(
    "Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0",
);

const ACCEPT: HeaderValue =
    HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8");
const ACCEPT_LANGUAGE: HeaderValue = HeaderValue::from_static("ru,en;q=0.8");

const SEC_FETCH_DEST: HeaderName = HeaderName::from_static("sec-fetch-dest");
const SEC_FETCH_MODE: HeaderName = HeaderName::from_static("sec-fetch-mode");
const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");
const SEC_FETCH_USER: HeaderName = HeaderName::from_static("sec-fetch-user");
const UPGRADE_INSECURE_REQUESTS: HeaderName = HeaderName::from_static("upgrade-insecure-requests");

const DOCUMENT: HeaderValue = HeaderValue::from_static("document");
const NAVIGATE: HeaderValue = HeaderValue::from_static("navigate");
const NONE: HeaderValue = HeaderValue::from_static("none");
const USER_ACTIVATED: HeaderValue = HeaderValue::from_static("?1");
const ENABLED: HeaderValue = HeaderValue::from_static("1");

/// Headers of a top-level browser navigation. `Upgrade-Insecure-Requests` is
/// unconditional: browsers omit it only for preloadable-HSTS hosts, a set this
/// client cannot know, and send it for every insecure URL.
pub(crate) fn navigation_headers(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, ACCEPT)
        .header(reqwest::header::ACCEPT_LANGUAGE, ACCEPT_LANGUAGE)
        .header(SEC_FETCH_DEST, DOCUMENT)
        .header(SEC_FETCH_MODE, NAVIGATE)
        .header(SEC_FETCH_SITE, NONE)
        .header(SEC_FETCH_USER, USER_ACTIVATED)
        .header(UPGRADE_INSECURE_REQUESTS, ENABLED)
}
