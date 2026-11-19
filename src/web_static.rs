//! Embedded Mini App assets. The page is public because Telegram opens it
//! before any JavaScript can read `initData`; every `/api/*` path is gated in
//! `src/web_api.rs`. The CSP allows the Telegram SDK script from `telegram.org`
//! and forbids inline script; inline style is permitted only because the SDK
//! injects its own stylesheet when the app runs inside Telegram Web.

use axum::body::Body;
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

const INDEX: &str = include_str!("web_assets/index.html");
const APP_JS: &str = include_str!("web_assets/app.js");
const STYLE: &str = include_str!("web_assets/style.css");

// `style-src` allows inline style because the Telegram Web SDK injects its
// iframe stylesheet on web.telegram.org; our own markup carries no inline style,
// and script stays limited to our file and the SDK.
const CSP: &str = "default-src 'none'; script-src 'self' https://telegram.org; \
style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; \
base-uri 'none'; form-action 'none'; frame-ancestors 'self' \
https://web.telegram.org https://*.telegram.org";

pub(crate) async fn index() -> Response {
    asset(INDEX, "text/html; charset=utf-8")
}

pub(crate) async fn app_js() -> Response {
    asset(APP_JS, "text/javascript; charset=utf-8")
}

pub(crate) async fn style_css() -> Response {
    asset(STYLE, "text/css; charset=utf-8")
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    let mut response = (StatusCode::OK, Body::from(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
