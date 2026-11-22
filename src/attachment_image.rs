use crate::bounded_fetch::{FetchCategory, FetchError, Fetched};
use crate::media::MediaKind;
use crate::prelude::*;
use crate::read_url::validate_url;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

const ACCEPT_ANY: &str = "*/*";
const MAX_CANDIDATES: usize = 3;
const MAX_IMAGE_FALLBACK: usize = 2;
const MAX_SCAN_NODES: usize = 256;
const HTML_SNIFF_BYTES: usize = 512;

static ATTACHMENT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedAttachment {
    pub(crate) path: PathBuf,
    pub(crate) kind: MediaKind,
    pub(crate) filename: String,
    pub(crate) host: Option<String>,
    /// Request path of the image that was actually accepted, without the query.
    pub(crate) url_path: Option<String>,
    pub(crate) bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttachmentErrorKind {
    Fetch,
    NotImage,
    TooLarge,
    Io,
    NoCandidate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttachmentError {
    pub(crate) kind: AttachmentErrorKind,
    pub(crate) host: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) status: Option<u16>,
    pub(crate) content_kind: Option<&'static str>,
    pub(crate) message: String,
}

impl AttachmentError {
    pub(crate) fn user_reason(&self) -> String {
        match self.kind {
            AttachmentErrorKind::Fetch => match self.status {
                Some(401 | 403) => "сайт требует авторизацию".into(),
                Some(404 | 410) => "файл не найден".into(),
                Some(429) => "сайт ограничил запросы".into(),
                _ if !self.message.is_empty() => self.message.clone(),
                _ => "не удалось скачать изображение".into(),
            },
            AttachmentErrorKind::NotImage => "по ссылке не изображение".into(),
            AttachmentErrorKind::TooLarge => "изображение слишком большое".into(),
            AttachmentErrorKind::Io => "не удалось сохранить файл".into(),
            AttachmentErrorKind::NoCandidate => "на странице не найдено изображение".into(),
        }
    }

    pub(crate) fn category(&self) -> &'static str {
        match self.kind {
            AttachmentErrorKind::Fetch => "fetch",
            AttachmentErrorKind::NotImage => "not_image",
            AttachmentErrorKind::TooLarge => "too_large",
            AttachmentErrorKind::Io => "io",
            AttachmentErrorKind::NoCandidate => "no_candidate",
        }
    }

    fn from_fetch(error: FetchError) -> Self {
        let kind = match error.category {
            FetchCategory::TooLarge => AttachmentErrorKind::TooLarge,
            FetchCategory::Http
            | FetchCategory::Timeout
            | FetchCategory::Transport
            | FetchCategory::Redirect
            | FetchCategory::InvalidUrl => AttachmentErrorKind::Fetch,
        };
        Self {
            kind,
            host: error.host,
            path: error.path,
            status: error.status,
            content_kind: None,
            message: error.message,
        }
    }

    fn not_image(
        host: Option<String>,
        path: Option<String>,
        status: Option<u16>,
        content_type: &str,
    ) -> Self {
        Self {
            kind: AttachmentErrorKind::NotImage,
            host,
            path,
            status,
            content_kind: Some(crate::log_fields::content_kind(content_type)),
            message: String::new(),
        }
    }

    fn timeout() -> Self {
        Self {
            kind: AttachmentErrorKind::Fetch,
            host: None,
            path: None,
            status: None,
            content_kind: None,
            message: "сайт не ответил вовремя".into(),
        }
    }

    fn io(error: std::io::Error) -> Self {
        Self {
            kind: AttachmentErrorKind::Io,
            host: None,
            path: None,
            status: None,
            content_kind: None,
            message: format!("{:?}", error.kind()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageFormat {
    Jpeg,
    Png,
    Webp,
}

impl ImageFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }
}

/// Content sniffing decides; a server label or a file extension is only a hint,
/// and a page can lie about both.
pub(crate) fn sniff_image(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageFormat::Jpeg);
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(ImageFormat::Png);
    }
    let webp_magic = bytes.get(8..12).is_some_and(|magic| magic == b"WEBP");
    if bytes.starts_with(b"RIFF") && webp_magic {
        return Some(ImageFormat::Webp);
    }
    None
}

pub(crate) fn looks_like_html(content_type: &str, bytes: &[u8]) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if mime.contains("html") {
        return true;
    }
    let head = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let window = head.get(..head.len().min(HTML_SNIFF_BYTES)).unwrap_or(head);
    let lowered = String::from_utf8_lossy(window).to_ascii_lowercase();
    let trimmed = lowered.trim_start();
    trimmed.starts_with("<!doctype html")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<head")
}

/// Metadata first, then `image_src`, then a bounded `img[src]` fallback: a page
/// with neither metadata nor a recognisable image URL yields nothing, because
/// guessing further turns a page fetch into uncontrolled downloads.
pub(crate) fn image_candidates(html: &str, base: &Url) -> Vec<Url> {
    // An attachment fetch may legally be much larger than a page read; parsing
    // tens of MiB of hostile markup is CPU work, so the reader's page bound
    // stays the ceiling before the DOM is built.
    if html.len() > config().read_url.max_bytes {
        return Vec::new();
    }
    if crate::read_url_html::html_is_too_complex(html) {
        return Vec::new();
    }
    let document = dom_query::Document::from(html);
    let max_chars = config().read_url.max_link_chars;
    let mut ranked: Vec<(u8, String)> = Vec::new();
    for node in document.select("meta").iter().take(MAX_SCAN_NODES) {
        let key = node
            .attr("property")
            .or_else(|| node.attr("name"))
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let rank = match key.as_str() {
            "og:image" | "og:image:url" | "og:image:secure_url" => 0u8,
            "twitter:image" | "twitter:image:src" => 1,
            _ => continue,
        };
        if let Some(content) = node.attr("content") {
            ranked.push((rank, content.to_string()));
        }
    }
    ranked.sort_by_key(|(rank, _)| *rank);
    let mut candidates: Vec<Url> = Vec::new();
    for (_, raw) in ranked {
        if let Some(url) = resolve_candidate(&raw, base, max_chars) {
            push_candidate(&mut candidates, url);
        }
        if candidates.len() >= MAX_CANDIDATES {
            return candidates;
        }
    }
    for node in document.select("link").iter().take(MAX_SCAN_NODES) {
        let is_image_src = node
            .attr("rel")
            .unwrap_or_default()
            .split_whitespace()
            .any(|value| value.eq_ignore_ascii_case("image_src"));
        if !is_image_src {
            continue;
        }
        if let Some(href) = node.attr("href") {
            if let Some(url) = resolve_candidate(&href, base, max_chars) {
                push_candidate(&mut candidates, url);
            }
        }
        if candidates.len() >= MAX_CANDIDATES {
            return candidates;
        }
    }
    if !candidates.is_empty() {
        return candidates;
    }
    for node in document.select("img[src]").iter().take(MAX_SCAN_NODES) {
        let Some(src) = node.attr("src") else {
            continue;
        };
        let Some(url) = resolve_candidate(&src, base, max_chars) else {
            continue;
        };
        if !has_image_extension(url.path()) {
            continue;
        }
        push_candidate(&mut candidates, url);
        if candidates.len() >= MAX_IMAGE_FALLBACK {
            break;
        }
    }
    candidates
}

fn resolve_candidate(raw: &str, base: &Url, max_chars: usize) -> Option<Url> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let Ok(mut url) = base.join(raw) else {
        return None;
    };
    url.set_fragment(None);
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.as_str().len() > max_chars
        || validate_url(&url).is_err()
    {
        return None;
    }
    Some(url)
}

fn has_image_extension(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".webp"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

fn push_candidate(candidates: &mut Vec<Url>, url: Url) {
    if candidates.len() < MAX_CANDIDATES && !candidates.contains(&url) {
        candidates.push(url);
    }
}

pub(crate) trait AttachmentFetcher: Send + Sync {
    fn fetch<'a>(
        &'a self,
        url: Url,
        accept: &'static str,
        cap: usize,
        deadline: time::Instant,
    ) -> Pin<Box<dyn Future<Output = Result<Fetched, FetchError>> + Send + 'a>>;
}

pub(crate) struct HttpAttachmentFetcher;

impl AttachmentFetcher for HttpAttachmentFetcher {
    fn fetch<'a>(
        &'a self,
        url: Url,
        accept: &'static str,
        cap: usize,
        deadline: time::Instant,
    ) -> Pin<Box<dyn Future<Output = Result<Fetched, FetchError>> + Send + 'a>> {
        Box::pin(async move {
            crate::bounded_fetch::fetch_until(url, accept, None, cap, deadline).await
        })
    }
}

pub(crate) async fn prepare(
    fetcher: &dyn AttachmentFetcher,
    url: Url,
    cap: usize,
    dir: &Path,
    deadline: time::Instant,
) -> Result<PreparedAttachment, AttachmentError> {
    let page = fetcher
        .fetch(url.clone(), ACCEPT_ANY, cap, deadline)
        .await
        .map_err(AttachmentError::from_fetch)?;
    if let Some(format) = sniff_image(&page.body) {
        return save(&page, format, cap, dir).await;
    }
    let page_host = page.host.clone();
    let page_path = page.path.clone();
    if !looks_like_html(&page.content_type, &page.body) {
        return Err(AttachmentError::not_image(
            page_host,
            page_path,
            None,
            &page.content_type,
        ));
    }
    // A page larger than the reader's own byte cap would be parsed only to be
    // discarded by `image_candidates`; refuse it before the lossy copy and the
    // DOM build.
    if page.body.len() > config().read_url.max_bytes {
        return Err(AttachmentError {
            kind: AttachmentErrorKind::NoCandidate,
            host: page_host,
            path: page_path,
            status: None,
            content_kind: Some("html"),
            message: String::new(),
        });
    }
    let html = String::from_utf8_lossy(&page.body);
    let candidates = image_candidates(&html, &url);
    drop(html);
    drop(page);
    if candidates.is_empty() {
        return Err(AttachmentError {
            kind: AttachmentErrorKind::NoCandidate,
            host: page_host,
            path: page_path,
            status: None,
            content_kind: Some("html"),
            message: String::new(),
        });
    }
    let mut last = None;
    for candidate in candidates {
        if time::Instant::now() >= deadline {
            last = Some(AttachmentError::timeout());
            break;
        }
        match fetcher.fetch(candidate, ACCEPT_ANY, cap, deadline).await {
            Ok(fetched) => match sniff_image(&fetched.body) {
                Some(format) => return save(&fetched, format, cap, dir).await,
                None => {
                    last = Some(AttachmentError::not_image(
                        fetched.host,
                        fetched.path,
                        None,
                        &fetched.content_type,
                    ))
                }
            },
            Err(error) => last = Some(AttachmentError::from_fetch(error)),
        }
    }
    Err(last.unwrap_or_else(|| AttachmentError {
        kind: AttachmentErrorKind::NoCandidate,
        host: page_host,
        path: page_path,
        status: None,
        content_kind: Some("html"),
        message: String::new(),
    }))
}

async fn save(
    fetched: &Fetched,
    format: ImageFormat,
    cap: usize,
    dir: &Path,
) -> Result<PreparedAttachment, AttachmentError> {
    if fetched.body.len() > cap {
        return Err(AttachmentError {
            kind: AttachmentErrorKind::TooLarge,
            host: fetched.host.clone(),
            path: fetched.path.clone(),
            status: None,
            content_kind: None,
            message: "ответ слишком большой".into(),
        });
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(AttachmentError::io)?;
    let filename = attachment_filename(format);
    let path = dir.join(&filename);
    if let Err(error) = tokio::fs::write(&path, &fetched.body).await {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(AttachmentError::io(error));
    }
    Ok(PreparedAttachment {
        path,
        kind: MediaKind::Photo,
        filename,
        host: fetched.host.clone(),
        url_path: fetched.path.clone(),
        bytes: fetched.body.len() as u64,
    })
}

fn attachment_filename(format: ImageFormat) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let sequence = ATTACHMENT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("attach-{millis}-{sequence}.{}", format.extension())
}
