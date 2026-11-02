//! Browser identity for the tools that fetch public HTML on the user's behalf
//! (`read_url` and the keyless HTML search engines: DuckDuckGo, Startpage,
//! Mojeek, Ecosia). A site that only inspects `User-Agent` otherwise answers
//! the honest `Maksut/0.1` string with a block page. Header emulation stops at
//! the wire format: no cookies, no JS, no TLS fingerprint, so JS/TLS challenges
//! still reject these requests (ADR 019).

use crate::prelude::*;
use reqwest::header::{HeaderName, HeaderValue};

pub(crate) const USER_AGENT: HeaderValue = HeaderValue::from_static(
    "Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0",
);

/// What a Firefox navigation sends for an HTML page.
pub(crate) const HTML_ACCEPT: HeaderValue =
    HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8");
const ACCEPT_LANGUAGE: HeaderValue = HeaderValue::from_static("ru,en;q=0.8");

const SEC_FETCH_DEST: HeaderName = HeaderName::from_static("sec-fetch-dest");
const SEC_FETCH_MODE: HeaderName = HeaderName::from_static("sec-fetch-mode");
const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");
const SEC_FETCH_USER: HeaderName = HeaderName::from_static("sec-fetch-user");
const UPGRADE_INSECURE_REQUESTS: HeaderName = HeaderName::from_static("upgrade-insecure-requests");

const DOCUMENT: HeaderValue = HeaderValue::from_static("document");
const NAVIGATE: HeaderValue = HeaderValue::from_static("navigate");
const USER_ACTIVATED: HeaderValue = HeaderValue::from_static("?1");
const ENABLED: HeaderValue = HeaderValue::from_static("1");

/// A typed or bookmarked URL: the request has no initiator origin.
pub(crate) const NONE_SITE: HeaderValue = HeaderValue::from_static("none");
const SAME_ORIGIN_SITE: HeaderValue = HeaderValue::from_static("same-origin");
const SAME_SITE: HeaderValue = HeaderValue::from_static("same-site");
const CROSS_SITE: HeaderValue = HeaderValue::from_static("cross-site");

/// Headers of a top-level browser navigation. `accept` and `site` stay
/// parameters because `read_url` negotiates Markdown ahead of HTML and has no
/// referer. `site` is `None` for a plain-http target, where browsers append no
/// Fetch metadata. `Upgrade-Insecure-Requests` is unconditional: browsers omit
/// it only for preloadable-HSTS hosts, a set this client cannot know, and send
/// it for every insecure URL.
pub(crate) fn navigation_headers(
    request: reqwest::RequestBuilder,
    accept: HeaderValue,
    site: Option<HeaderValue>,
) -> reqwest::RequestBuilder {
    let request = request
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, accept)
        .header(reqwest::header::ACCEPT_LANGUAGE, ACCEPT_LANGUAGE)
        .header(UPGRADE_INSECURE_REQUESTS, ENABLED);
    let Some(site) = site else {
        return request;
    };
    request
        .header(SEC_FETCH_DEST, DOCUMENT)
        .header(SEC_FETCH_MODE, NAVIGATE)
        .header(SEC_FETCH_SITE, site)
        .header(SEC_FETCH_USER, USER_ACTIVATED)
}

/// `Sec-Fetch-Site` for a request that carries `referer`. A request with a
/// referer cannot be `none`; the search engines refer to their own origin.
/// Same-host comparison is conservative: a sibling subdomain is reported as
/// `cross-site` because only a public-suffix list could call it `same-site`.
pub(crate) fn referer_site(url: &Url, referer: &str) -> HeaderValue {
    let Ok(referer) = Url::parse(referer) else {
        return CROSS_SITE;
    };
    let same_scheme = referer.scheme() == url.scheme();
    let same_host = referer
        .host_str()
        .zip(url.host_str())
        .is_some_and(|(from, target)| from.eq_ignore_ascii_case(target));
    // `Origin` equality without `Url::origin()`, which allocates scheme and
    // host strings on both sides. Only http/https targets reach this helper.
    if same_scheme && same_host && referer.port_or_known_default() == url.port_or_known_default() {
        return SAME_ORIGIN_SITE;
    }
    if same_scheme && same_host {
        SAME_SITE
    } else {
        CROSS_SITE
    }
}
