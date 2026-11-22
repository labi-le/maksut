use crate::attachment_image::{
    prepare, AttachmentErrorKind, AttachmentFetcher, PreparedAttachment,
};
use crate::bounded_fetch::{fetch, FetchCategory};
use crate::media::MediaKind;
use crate::prelude::*;
use crate::research_attachments::ResearchAttachment;

const ACCEPT: &str = "*/*";

pub(crate) struct AttachmentReport {
    pub(crate) sent: usize,
    pub(crate) failed: usize,
}

/// A file this delivery owns: `path` is a temp file removed exactly once, after
/// the upload succeeded and after every failure that got this far.
pub(crate) struct ResolvedAttachment {
    pub(crate) path: PathBuf,
    pub(crate) kind: MediaKind,
    pub(crate) filename: String,
    pub(crate) host: Option<String>,
    /// Request path of the accepted file, never its query string.
    pub(crate) url_path: Option<String>,
    pub(crate) bytes: Option<u64>,
    /// `prepared` when `read_url` already downloaded the file, `delivery` when
    /// the delivery-time detector fetched it, `direct` for the byte-for-byte
    /// non-image path.
    pub(crate) source: &'static str,
}

/// Why one attachment failed: the static fields a log may carry, plus the one
/// sentence a chat participant could read.
pub(crate) struct AttachmentFailure {
    pub(crate) stage: &'static str,
    pub(crate) category: &'static str,
    pub(crate) reason: String,
    pub(crate) host: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) status: Option<u16>,
    pub(crate) content_kind: Option<&'static str>,
    pub(crate) attempt: Option<u32>,
}

impl AttachmentFailure {
    fn new(stage: &'static str, category: &'static str, reason: impl Into<String>) -> Self {
        Self {
            stage,
            category,
            reason: reason.into(),
            host: None,
            path: None,
            status: None,
            content_kind: None,
            attempt: None,
        }
    }
}

/// Removes prepared files when a reply is dropped before delivery (a PASS
/// branch or an error return). Delivery removes its own file after the upload.
pub(crate) async fn cleanup(attachments: &[ResearchAttachment]) {
    for attachment in attachments {
        if let Some(prepared) = &attachment.prepared {
            let _ = tokio::fs::remove_file(&prepared.path).await;
        }
    }
}

/// One file per attachment, each independently bounded and each failure
/// reported: a rejected tracker file must not swallow the rest of the reply.
pub(crate) async fn send(
    bot: &Bot,
    chat_id: ChatId,
    attachments: &[ResearchAttachment],
    fetcher: &dyn AttachmentFetcher,
) -> AttachmentReport {
    let mut report = AttachmentReport { sent: 0, failed: 0 };
    if attachments.is_empty() {
        return report;
    }
    let dir = PathBuf::from(&config().paths.download_dir);
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        tracing::warn!(count = attachments.len(), "attachment dir unavailable");
        report.failed = attachments.len();
        return report;
    }
    tracing::debug!(count = attachments.len(), "attachments started");
    for (index, attachment) in attachments.iter().enumerate() {
        match send_one(bot, chat_id, &dir, index, attachment, fetcher).await {
            Ok(()) => report.sent += 1,
            Err(_) => report.failed += 1,
        }
    }
    report
}

async fn send_one(
    bot: &Bot,
    chat_id: ChatId,
    dir: &Path,
    index: usize,
    attachment: &ResearchAttachment,
    fetcher: &dyn AttachmentFetcher,
) -> Result<(), String> {
    let started = time::Instant::now();
    let kind = attachment.kind();
    let resolved = match resolve_attachment(dir, index, attachment, fetcher).await {
        Ok(resolved) => resolved,
        Err(failure) => {
            let failure = *failure;
            tracing::warn!(
                index,
                kind = ?kind,
                host = failure.host.as_deref(),
                path = failure.path.as_deref(),
                stage = failure.stage,
                category = failure.category,
                reason = failure.reason.as_str(),
                status = failure.status,
                content_kind = failure.content_kind,
                attempt = failure.attempt,
                elapsed_ms = crate::log_fields::elapsed_ms(started),
                bytes = None::<u64>,
                "attachment failed"
            );
            return Err(failure.reason);
        }
    };
    let (_, max_bytes) = byte_cap();
    let sent = crate::delivery::send_single_media(
        bot,
        chat_id,
        InputFile::file(resolved.path.clone()).file_name(resolved.filename.clone()),
        resolved.kind,
        "",
        crate::delivery::SendPolicy {
            max_bytes,
            allow_document_fallback: true,
            allow_retry: true,
            deadline: time::Instant::now()
                + Duration::from_secs(config().telegram.request_timeout_secs),
        },
    )
    .await;
    remove_temp(&resolved.path).await;
    match sent {
        Ok(_) => {
            tracing::debug!(
                index,
                kind = ?resolved.kind,
                host = resolved.host.as_deref(),
                path = resolved.url_path.as_deref(),
                stage = "upload",
                status = "ok",
                source = resolved.source,
                elapsed_ms = crate::log_fields::elapsed_ms(started),
                bytes = resolved.bytes,
                "attachment delivered"
            );
            Ok(())
        }
        Err(error) => {
            tracing::warn!(
                index,
                kind = ?resolved.kind,
                host = resolved.host.as_deref(),
                path = resolved.url_path.as_deref(),
                stage = error.diagnostic.stage,
                category = error.diagnostic.detail,
                reason = error.reason.as_str(),
                status = None::<u16>,
                attempt = Some(1u32),
                source = resolved.source,
                elapsed_ms = crate::log_fields::elapsed_ms(started),
                bytes = resolved.bytes,
                "attachment failed"
            );
            Err("файл не отправлен".to_string())
        }
    }
}

/// Everything that happens before the upload. A prepared attachment already
/// paid for its download in the image detector, so it must not be fetched again;
/// a photo the detector never resolved is the only kind that may turn into a
/// prepared one here, because every other format is delivered byte-for-byte from
/// the source URL.
pub(crate) async fn resolve_attachment(
    dir: &Path,
    index: usize,
    attachment: &ResearchAttachment,
    fetcher: &dyn AttachmentFetcher,
) -> Result<ResolvedAttachment, Box<AttachmentFailure>> {
    if let Some(prepared) = &attachment.prepared {
        return Ok(from_prepared(prepared, &attachment.filename, "prepared"));
    }
    let mut url = Url::parse(&attachment.url).map_err(|_| {
        Box::new(AttachmentFailure::new(
            "validate",
            "invalid_url",
            "ссылка невалидна",
        ))
    })?;
    url.set_fragment(None);
    let (cap, _) = byte_cap();
    if attachment.kind() == MediaKind::Photo {
        let deadline =
            time::Instant::now() + Duration::from_secs(config().read_url.total_timeout_secs);
        let prepared = prepare(fetcher, url, cap, dir, deadline)
            .await
            .map_err(|error| {
                Box::new(AttachmentFailure {
                    stage: match error.kind {
                        AttachmentErrorKind::Io => "write",
                        AttachmentErrorKind::Fetch
                        | AttachmentErrorKind::NotImage
                        | AttachmentErrorKind::TooLarge
                        | AttachmentErrorKind::NoCandidate => "fetch",
                    },
                    category: error.category(),
                    reason: error.user_reason(),
                    host: error.host,
                    path: error.path,
                    status: error.status,
                    content_kind: error.content_kind,
                    // The retry budget lives in `bounded_fetch`, so the caller
                    // cannot count the requests the detector made.
                    attempt: None,
                })
            })?;
        return Ok(from_prepared(&prepared, &attachment.filename, "delivery"));
    }
    let host = url.host_str().map(str::to_owned);
    let url_path = Some(url.path().to_owned());
    // A `.torrent` lives on the tracker that produced the hit and needs that
    // tracker's session; any other host resolves to no cookie.
    let cookie = crate::torrent_providers::download_cookie(&url)
        .await
        .map_err(|reason| {
            Box::new(AttachmentFailure {
                stage: "fetch",
                category: "cookie",
                reason,
                host: host.clone(),
                path: url_path.clone(),
                status: None,
                content_kind: None,
                attempt: Some(1),
            })
        })?;
    let fetched = fetch(url, ACCEPT, cookie.as_deref(), cap)
        .await
        .map_err(|error| {
            let reason = error.user_reason();
            Box::new(AttachmentFailure {
                stage: "fetch",
                category: fetch_category(error.category),
                reason,
                host: error.host.or(host),
                path: error.path.or(url_path),
                status: error.status,
                content_kind: None,
                // The retry budget of `fetch` is internal, so the request count
                // of a failed fetch is not observable from here.
                attempt: None,
            })
        })?;
    reject_markup(&fetched.content_type).map_err(|reason| {
        Box::new(AttachmentFailure {
            stage: "validate",
            category: "markup",
            reason,
            host: fetched.host.clone(),
            path: fetched.path.clone(),
            status: None,
            content_kind: Some(crate::log_fields::content_kind(&fetched.content_type)),
            attempt: Some(1),
        })
    })?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let path = dir.join(format!("attach-{stamp}-{index}-{}", attachment.filename));
    if tokio::fs::write(&path, &fetched.body).await.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(Box::new(AttachmentFailure {
            stage: "write",
            category: "io",
            reason: "файл не сохранён".to_string(),
            host: fetched.host,
            path: fetched.path,
            status: None,
            content_kind: None,
            attempt: Some(1),
        }));
    }
    Ok(ResolvedAttachment {
        path,
        kind: attachment.kind(),
        filename: attachment.filename.clone(),
        host: fetched.host,
        url_path: fetched.path,
        bytes: u64::try_from(fetched.body.len()).ok(),
        source: "direct",
    })
}

fn from_prepared(
    prepared: &PreparedAttachment,
    display_name: &str,
    source: &'static str,
) -> ResolvedAttachment {
    ResolvedAttachment {
        path: prepared.path.clone(),
        kind: prepared.kind,
        filename: display_name.to_owned(),
        host: prepared.host.clone(),
        url_path: prepared.url_path.clone(),
        bytes: Some(prepared.bytes),
        source,
    }
}

/// The temp file is owned by this delivery alone, so exactly one call site
/// removes it. A failed unlink leaks one file; it must not turn an attachment
/// that was already delivered into a failure.
async fn remove_temp(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(kind = ?error.kind(), "attachment temp file not removed");
        }
    }
}

/// One byte cap for the download and the upload: the smaller of the configured
/// attachment cap and what the Bot API accepts.
fn byte_cap() -> (usize, u64) {
    let max_bytes = config()
        .research
        .attachment_max_bytes
        .min(config().telegram.file_limit_bytes);
    (usize::try_from(max_bytes).unwrap_or(usize::MAX), max_bytes)
}

fn fetch_category(category: FetchCategory) -> &'static str {
    match category {
        FetchCategory::Http => "http",
        FetchCategory::Timeout => "timeout",
        FetchCategory::Transport => "transport",
        FetchCategory::TooLarge => "too_large",
        FetchCategory::Redirect => "redirect",
        FetchCategory::InvalidUrl => "invalid_url",
    }
}

/// A login page, a Cloudflare challenge, or an error page is not the file the
/// release promised: uploading it as `name.torrent` misleads the user.
pub(crate) fn reject_markup(content_type: &str) -> Result<(), String> {
    let kind = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if kind.starts_with("text/") || kind == "application/xhtml+xml" {
        return Err("получена страница вместо файла".to_string());
    }
    Ok(())
}
