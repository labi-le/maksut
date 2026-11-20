use crate::bounded_fetch::fetch;
use crate::prelude::*;
use crate::research_attachments::ResearchAttachment;

const ACCEPT: &str = "*/*";

pub(crate) struct AttachmentReport {
    pub(crate) sent: usize,
    pub(crate) failed: usize,
}

/// One file per attachment, each independently bounded and each failure
/// reported: a rejected tracker file must not swallow the rest of the reply.
pub(crate) async fn send(
    bot: &Bot,
    chat_id: ChatId,
    attachments: &[ResearchAttachment],
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
    for (index, attachment) in attachments.iter().enumerate() {
        match send_one(bot, chat_id, &dir, index, attachment).await {
            Ok(()) => report.sent += 1,
            Err(reason) => {
                tracing::warn!(%reason, kind = ?attachment.kind(), "attachment not delivered");
                report.failed += 1;
            }
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
) -> Result<(), String> {
    let max_bytes = config()
        .research
        .attachment_max_bytes
        .min(config().telegram.file_limit_bytes);
    let cap = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut url = Url::parse(&attachment.url).map_err(|_| "ссылка невалидна".to_string())?;
    url.set_fragment(None);
    // A `.torrent` lives on the tracker that produced the hit and needs that
    // tracker's session; any other host resolves to no cookie.
    let cookie = crate::torrent_providers::download_cookie(&url).await?;
    let fetched = fetch(url, ACCEPT, cookie.as_deref(), cap).await?;
    reject_markup(&fetched.content_type)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let path = dir.join(format!("attach-{stamp}-{index}-{}", attachment.filename));
    if tokio::fs::write(&path, &fetched.body).await.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
        return Err("файл не сохранён".to_string());
    }
    let sent = crate::delivery::send_single_media(
        bot,
        chat_id,
        InputFile::file(path.clone()),
        attachment.kind(),
        "",
        max_bytes,
        true,
    )
    .await;
    let _ = tokio::fs::remove_file(&path).await;
    sent.map(|_| ())
        .map_err(|_| "файл не отправлен".to_string())
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
