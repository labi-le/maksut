use crate::attachment_image::{
    image_candidates, looks_like_html, prepare, sniff_image, AttachmentErrorKind, ImageFormat,
};
use crate::bounded_fetch::{FetchCategory, FetchError, Fetched};
use crate::media::MediaKind;
use crate::prelude::*;
use crate::test_support::MockAttachmentFetcher;

fn jpeg() -> Vec<u8> {
    vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01,
    ]
}

fn png() -> Vec<u8> {
    vec![
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
        b'R',
    ]
}

fn webp() -> Vec<u8> {
    b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec()
}

#[test]
fn sniff_image_accepts_jpeg_png_and_webp() {
    assert_eq!(sniff_image(&jpeg()), Some(ImageFormat::Jpeg));
    assert_eq!(sniff_image(&png()), Some(ImageFormat::Png));
    assert_eq!(sniff_image(&webp()), Some(ImageFormat::Webp));
}

#[test]
fn sniff_image_rejects_html_truncated_and_svg() {
    assert_eq!(
        sniff_image(b"<!doctype html><html><body>cat</body></html>"),
        None
    );
    assert_eq!(sniff_image(b""), None);
    assert_eq!(sniff_image(&[0xFF, 0xD8]), None);
    assert_eq!(sniff_image(&png()[..4]), None);
    assert_eq!(sniff_image(b"RIFF\x24\x00\x00\x00WEB"), None);
    assert_eq!(
        sniff_image(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"),
        None
    );
}

#[test]
fn looks_like_html_reads_the_header_and_the_body() {
    assert!(looks_like_html("text/html; charset=utf-8", b""));
    assert!(looks_like_html("application/xhtml+xml", b""));
    assert!(looks_like_html(
        "application/octet-stream",
        b"<!doctype html><html>"
    ));
    assert!(looks_like_html("", b"  \n<HTML><head></head>"));
    assert!(!looks_like_html("image/jpeg", &jpeg()));
    assert!(!looks_like_html(
        "application/octet-stream",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"
    ));
}

#[test]
fn image_candidates_prefers_og_image_over_the_rest() {
    let html = r#"<!doctype html><html><head>
        <title>File:Cat03.jpg</title>
        <meta property="og:image" content="https://upload.wikimedia.org/wikipedia/commons/3/3a/Cat03.jpg">
        <meta name="twitter:image" content="https://upload.wikimedia.org/twitter-card.jpg">
        <link rel="image_src" href="https://upload.wikimedia.org/link.jpg">
        </head><body><img src="/static/logo.png"></body></html>"#;
    let base = Url::parse("https://commons.wikimedia.org/wiki/File:Cat03.jpg").unwrap();
    let candidates = image_candidates(html, &base);
    assert_eq!(candidates.len(), 3);
    assert_eq!(
        candidates[0].as_str(),
        "https://upload.wikimedia.org/wikipedia/commons/3/3a/Cat03.jpg"
    );
    assert_eq!(
        candidates[1].as_str(),
        "https://upload.wikimedia.org/twitter-card.jpg"
    );
    assert_eq!(
        candidates[2].as_str(),
        "https://upload.wikimedia.org/link.jpg"
    );
}

#[test]
fn image_candidates_resolves_relative_urls_against_the_page() {
    let html = r#"<meta property="og:image" content="../images/cat.jpg#fragment">"#;
    let base = Url::parse("https://example.com/wiki/File:Cat03.jpg").unwrap();
    let candidates = image_candidates(html, &base);
    assert_eq!(
        candidates,
        vec![Url::parse("https://example.com/images/cat.jpg").unwrap()]
    );
}

#[test]
fn image_candidates_accepts_an_image_src_link() {
    let html = r#"<link rel="image_src" href="/img/other.png">"#;
    let base = Url::parse("https://example.com/wiki/File:Cat03.jpg").unwrap();
    let candidates = image_candidates(html, &base);
    assert_eq!(
        candidates,
        vec![Url::parse("https://example.com/img/other.png").unwrap()]
    );
}

#[test]
fn image_candidates_falls_back_to_two_extended_img_sources() {
    let html = r#"<img src="/a.jpg"><img src="/b.PNG"><img src="/c.gif">
        <img src="/d.webp"><img src="/e.jpeg">"#;
    let base = Url::parse("https://example.com/page").unwrap();
    let candidates = image_candidates(html, &base);
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].as_str(), "https://example.com/a.jpg");
    assert_eq!(candidates[1].as_str(), "https://example.com/b.PNG");
}

#[test]
fn image_candidates_caps_the_total_at_three() {
    let html = (0..5)
        .map(|index| format!(r#"<meta property="og:image" content="/cat-{index}.jpg">"#))
        .collect::<String>();
    let base = Url::parse("https://example.com/page").unwrap();
    assert_eq!(image_candidates(&html, &base).len(), 3);
}

#[test]
fn image_candidates_rejects_private_and_invalid_hosts() {
    let html = r#"
        <meta property="og:image" content="http://localhost/cat.jpg">
        <meta property="og:image" content="http://127.0.0.1/cat.jpg">
        <meta property="og:image" content="https://user:pass@example.com/cat.jpg">
        <meta property="og:image" content="javascript:alert(1)">
        <meta property="og:image" content="https://cdn.example.com/cat.jpg">"#;
    let base = Url::parse("https://example.com/page").unwrap();
    let candidates = image_candidates(html, &base);
    assert_eq!(
        candidates,
        vec![Url::parse("https://cdn.example.com/cat.jpg").unwrap()]
    );
}

#[test]
fn image_candidates_dedups_the_same_url() {
    let html = r#"
        <meta property="og:image" content="https://cdn.example.com/cat.jpg">
        <meta property="og:image:url" content="https://cdn.example.com/cat.jpg">"#;
    let base = Url::parse("https://example.com/page").unwrap();
    assert_eq!(image_candidates(html, &base).len(), 1);
}

fn fetched(body: Vec<u8>, content_type: &str, host: &str, path: &str) -> Fetched {
    Fetched {
        body,
        content_type: content_type.to_owned(),
        host: Some(host.to_owned()),
        path: Some(path.to_owned()),
    }
}

fn fetch_error(category: FetchCategory, status: Option<u16>, host: &str) -> FetchError {
    FetchError {
        category,
        status,
        host: Some(host.to_owned()),
        path: Some("/blocked".to_owned()),
        message: "хост ответил ошибкой".to_owned(),
    }
}

fn deadline() -> time::Instant {
    time::Instant::now() + Duration::from_secs(10)
}

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "maksut-attach-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn prepare_follows_og_image_from_a_html_page() {
    let dir = temp_dir("og");
    let fetcher = MockAttachmentFetcher::new();
    let page = r#"<!doctype html><html><head>
        <meta property="og:image" content="https://upload.wikimedia.org/wikipedia/commons/3/3a/Cat03.jpg">
        </head><body>File:Cat03.jpg</body></html>"#;
    fetcher.push(Ok(fetched(
        page.as_bytes().to_vec(),
        "text/html; charset=utf-8",
        "commons.wikimedia.org",
        "/wiki/File:Cat03.jpg",
    )));
    let image = jpeg();
    fetcher.push(Ok(fetched(
        image.clone(),
        "image/jpeg",
        "upload.wikimedia.org",
        "/wikipedia/commons/3/3a/Cat03.jpg",
    )));
    let url = Url::parse("https://commons.wikimedia.org/wiki/File:Cat03.jpg").unwrap();
    let prepared = prepare(&fetcher, url, 1_048_576, &dir, deadline())
        .await
        .unwrap();
    assert_eq!(prepared.kind, MediaKind::Photo);
    assert_eq!(prepared.host.as_deref(), Some("upload.wikimedia.org"));
    assert_eq!(
        prepared.url_path.as_deref(),
        Some("/wikipedia/commons/3/3a/Cat03.jpg")
    );
    assert_eq!(prepared.bytes, image.len() as u64);
    assert!(prepared.filename.starts_with("attach-"));
    assert!(prepared.filename.ends_with(".jpg"));
    assert_eq!(
        prepared.path.file_name().unwrap(),
        prepared.filename.as_str()
    );
    assert_eq!(std::fs::read(&prepared.path).unwrap(), image);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn prepare_trusts_sniffing_over_the_content_type() {
    let dir = temp_dir("sniff");
    let fetcher = MockAttachmentFetcher::new();
    let image = png();
    fetcher.push(Ok(fetched(
        image.clone(),
        "application/octet-stream",
        "cdn.example.com",
        "/cat.png",
    )));
    let url = Url::parse("https://cdn.example.com/cat.png").unwrap();
    let prepared = prepare(&fetcher, url, 1_048_576, &dir, deadline())
        .await
        .unwrap();
    assert_eq!(prepared.kind, MediaKind::Photo);
    assert_eq!(prepared.filename.rsplit('.').next(), Some("png"));
    assert_eq!(std::fs::read(&prepared.path).unwrap(), image);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn prepare_propagates_a_429_without_its_own_retry() {
    let dir = temp_dir("429");
    let fetcher = MockAttachmentFetcher::new();
    fetcher.push(Err(fetch_error(
        FetchCategory::Http,
        Some(429),
        "cdn.example.com",
    )));
    // A queued success proves the detector did not consume it: the retry
    // budget belongs to `bounded_fetch`, not to this layer.
    fetcher.push(Ok(fetched(
        webp(),
        "image/webp",
        "cdn.example.com",
        "/cat.webp",
    )));
    let url = Url::parse("https://cdn.example.com/cat.jpg").unwrap();
    let error = prepare(&fetcher, url, 1_048_576, &dir, deadline())
        .await
        .unwrap_err();
    assert_eq!(error.kind, AttachmentErrorKind::Fetch);
    assert_eq!(error.status, Some(429));
    assert_eq!(error.category(), "fetch");
    assert!(error.user_reason().contains("ограничил"));
    assert_eq!(fetcher.calls(), 1);
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn prepare_reports_a_page_without_an_image() {
    let dir = temp_dir("none");
    let fetcher = MockAttachmentFetcher::new();
    fetcher.push(Ok(fetched(
        b"<!doctype html><html><body><p>no cat here</p></body></html>".to_vec(),
        "text/html",
        "example.com",
        "/page",
    )));
    let url = Url::parse("https://example.com/page").unwrap();
    let error = prepare(&fetcher, url, 1_048_576, &dir, deadline())
        .await
        .unwrap_err();
    assert_eq!(error.kind, AttachmentErrorKind::NoCandidate);
    assert_eq!(error.host.as_deref(), Some("example.com"));
    assert_eq!(error.path.as_deref(), Some("/page"));
    assert_eq!(error.category(), "no_candidate");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn prepare_reports_a_candidate_that_is_not_an_image() {
    let dir = temp_dir("notimage");
    let fetcher = MockAttachmentFetcher::new();
    fetcher.push(Ok(fetched(
        br#"<meta property="og:image" content="https://cdn.example.com/cat.jpg">"#.to_vec(),
        "text/html",
        "example.com",
        "/page",
    )));
    fetcher.push(Ok(fetched(
        b"<html>login required</html>".to_vec(),
        "text/html",
        "cdn.example.com",
        "/cat.jpg",
    )));
    let url = Url::parse("https://example.com/page").unwrap();
    let error = prepare(&fetcher, url, 1_048_576, &dir, deadline())
        .await
        .unwrap_err();
    assert_eq!(error.kind, AttachmentErrorKind::NotImage);
    assert_eq!(error.category(), "not_image");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn prepare_rejects_a_body_over_the_cap_before_writing() {
    let dir = temp_dir("cap");
    let fetcher = MockAttachmentFetcher::new();
    let mut body = jpeg();
    body.extend_from_slice(&[0u8; 32]);
    fetcher.push(Ok(fetched(
        body,
        "image/jpeg",
        "cdn.example.com",
        "/cat.jpg",
    )));
    let url = Url::parse("https://cdn.example.com/cat.jpg").unwrap();
    let error = prepare(&fetcher, url, 16, &dir, deadline())
        .await
        .unwrap_err();
    assert_eq!(error.kind, AttachmentErrorKind::TooLarge);
    assert_eq!(error.category(), "too_large");
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn prepare_maps_a_fetcher_too_large_error() {
    let dir = temp_dir("fetchcap");
    let fetcher = MockAttachmentFetcher::new();
    fetcher.push(Err(fetch_error(
        FetchCategory::TooLarge,
        None,
        "cdn.example.com",
    )));
    let url = Url::parse("https://cdn.example.com/cat.jpg").unwrap();
    let error = prepare(&fetcher, url, 16, &dir, deadline())
        .await
        .unwrap_err();
    assert_eq!(error.kind, AttachmentErrorKind::TooLarge);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn prepare_refuses_an_html_page_over_the_reader_byte_cap() {
    let dir = temp_dir("oversized-page");
    let fetcher = MockAttachmentFetcher::new();
    let body = format!(
        "<html>{}</html>",
        "x".repeat(config().read_url.max_bytes + 1)
    );
    fetcher.push(Ok(Fetched {
        body: body.into_bytes(),
        content_type: "text/html".to_owned(),
        host: Some("example.com".to_owned()),
        path: Some("/page".to_owned()),
    }));
    let url = Url::parse("https://example.com/page.html").unwrap();
    let error = prepare(&fetcher, url, 20 * 1024 * 1024, &dir, deadline())
        .await
        .unwrap_err();
    assert_eq!(error.kind, AttachmentErrorKind::NoCandidate);
    assert_eq!(error.content_kind, Some("html"));
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn image_candidates_refuses_a_complex_dom() {
    let html = "<div>".repeat(config().read_url.max_html_elements + 1);
    let base = Url::parse("https://example.com/").unwrap();
    assert!(image_candidates(&html, &base).is_empty());
}
