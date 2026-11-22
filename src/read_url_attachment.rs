use crate::attachment_image::prepare;
use crate::chat_tools::{ack_lookup, ToolContext, ToolState};
use crate::media::MediaKind;
use crate::prelude::*;
use crate::research_attachments::{push_bounded, ResearchAttachment};

/// Resolves one `read_url` attachment for the reply. A photo is downloaded and
/// sniffed inside the turn so the answer can say whether it will actually
/// arrive; every other kind stays a promise the send step keeps or reports,
/// because only an image gives the model a reason it can act on right away.
pub(crate) async fn queue(
    ctx: &ToolContext<'_>,
    state: &mut ToolState,
    mut attachment: ResearchAttachment,
    limit: usize,
) -> String {
    state.read_url_calls += 1;
    ack_lookup(ctx, state);
    let Ok(url) = Url::parse(&attachment.url) else {
        return "не отправлено: ссылка невалидна; попробуй другую ссылку из поиска".to_string();
    };
    if attachment.kind() != MediaKind::Photo {
        let filename = attachment.filename.clone();
        let _ = push_bounded(&mut state.research.attachments, attachment, limit);
        return format!("файл {filename} будет отправлен в чат вместе с ответом");
    }
    // A second call for the same image would download it again and then be
    // dropped by `push_bounded`, leaving the saved file behind.
    if state
        .research
        .attachments
        .iter()
        .any(|queued| queued.url == attachment.url)
    {
        return format!("картинка {} уже в очереди на отправку", attachment.filename);
    }
    let settings = config();
    // Beyond either cap the file can never be delivered, so the fetch must not
    // spend bandwidth on it.
    let max_bytes = settings
        .research
        .attachment_max_bytes
        .min(settings.telegram.file_limit_bytes);
    let cap = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let dir = PathBuf::from(&settings.paths.download_dir);
    // Only time spent downloading counts against the turn budget: a slow LLM
    // round between two `read_url` calls must not expire the second attempt.
    let allowed = *state
        .attachment_budget
        .get_or_insert(Duration::from_secs(settings.read_url.total_timeout_secs));
    if allowed.is_zero() {
        tracing::warn!(
            host = url.host_str(),
            path = url.path(),
            "attachment budget exhausted"
        );
        return "не отправлено: лимит времени на вложения; попробуй другую ссылку из поиска"
            .to_string();
    }
    let started = time::Instant::now();
    let deadline = started + allowed;
    let result = prepare(
        ctx.chat.attachment_fetcher.as_ref(),
        url,
        cap,
        &dir,
        deadline,
    )
    .await;
    state.attachment_budget = Some(allowed.saturating_sub(started.elapsed()));
    match result {
        Ok(prepared) => {
            let filename = attachment.filename.clone();
            let path = prepared.path.clone();
            attachment.prepared = Some(prepared);
            if !push_bounded(&mut state.research.attachments, attachment, limit) {
                let _ = tokio::fs::remove_file(&path).await;
                return "лимит вложений на ответ".to_string();
            }
            format!("картинка {filename} готова к отправке")
        }
        Err(error) => {
            tracing::warn!(
                category = error.category(),
                reason = %error.user_reason(),
                host = error.host.as_deref(),
                path = error.path.as_deref(),
                status = error.status,
                content_kind = error.content_kind,
                "attachment rejected"
            );
            format!(
                "не отправлено: {}; попробуй другую ссылку из поиска",
                error.user_reason()
            )
        }
    }
}
