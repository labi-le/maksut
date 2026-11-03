use crate::prelude::*;

#[derive(Clone, Copy, Debug)]
pub(crate) struct FailureDiagnostic {
    pub(crate) stage: &'static str,
    pub(crate) detail: &'static str,
    pub(crate) exit_code: Option<i32>,
}

impl FailureDiagnostic {
    pub(crate) fn new(stage: &'static str, detail: &'static str) -> Self {
        Self {
            stage,
            detail,
            exit_code: None,
        }
    }
}

impl std::fmt::Display for FailureDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.detail)
    }
}

#[derive(Debug)]
pub(crate) struct DownloadFailure {
    pub(crate) reason: String,
    pub(crate) diagnostic: FailureDiagnostic,
    pub(crate) events: Vec<FailureDiagnostic>,
}

impl DownloadFailure {
    pub(crate) fn new(reason: String, stage: &'static str, detail: &'static str) -> Self {
        Self {
            reason,
            diagnostic: FailureDiagnostic::new(stage, detail),
            events: Vec::new(),
        }
    }
}

pub(crate) fn trace_event(events: &mut Vec<FailureDiagnostic>, event: FailureDiagnostic) {
    if events.len() < 12 {
        events.push(event);
    }
}

pub(crate) fn source_user_diagnostic(
    diagnostic: FailureDiagnostic,
    events: &[FailureDiagnostic],
) -> FailureDiagnostic {
    // Keep the video/authentication origin in the journal, but display the actionable failure.
    if diagnostic.stage != "yt-dlp"
        || !matches!(
            diagnostic.detail,
            "Authentication required" | "Cookies expired" | "Download failed"
        )
    {
        return diagnostic;
    }
    events
        .iter()
        .rev()
        .find(|event| {
            matches!(
                event.stage,
                "yt-dlp" | "metadata" | "photo-page" | "photo-cdn"
            ) || (event.stage == "selection" && event.detail == "Size limit")
                || (event.stage == "retry"
                    && matches!(
                        event.detail,
                        "Launch failed" | "Attempt failed" | "Preparation failed"
                    ))
        })
        .copied()
        .unwrap_or(diagnostic)
}
pub(crate) fn format_failure_context(url: &Url, events: &[FailureDiagnostic]) -> String {
    let host = url
        .host_str()
        .unwrap_or("")
        .trim_start_matches("www.")
        .trim_start_matches("m.");
    let mut steps: Vec<String> = Vec::new();
    for ev in events {
        let label = match ev.exit_code {
            Some(code) => format!("{}:{}({})", ev.stage, ev.detail, code),
            None => format!("{}:{}", ev.stage, ev.detail),
        };
        if steps.last() != Some(&label) {
            steps.push(label);
        }
    }
    match (host.is_empty(), steps.len() > 1) {
        (false, true) => format!(" [источник: {host}; цепочка: {}]", steps.join(" -> ")),
        (false, false) => format!(" [источник: {host}]"),
        (true, true) => format!(" [цепочка: {}]", steps.join(" -> ")),
        (true, false) => String::new(),
    }
}

pub(crate) async fn record_failure(pool: &SqlitePool, url: &Url, failure: FailureDiagnostic) {
    record_failure_with_events(pool, url, failure, &[]).await;
}

pub(crate) async fn record_failure_with_events(
    pool: &SqlitePool,
    url: &Url,
    failure: FailureDiagnostic,
    events: &[FailureDiagnostic],
) {
    let result = async {
        let mut transaction = pool.begin().await?;
        let rowid = sqlx::query("INSERT INTO download_failures (url_host, url_path, stage, detail, exit_code) VALUES (?, ?, ?, ?, ?)")
            .bind(url.host_str().unwrap_or(""))
            .bind(url.path())
            .bind(failure.stage)
            .bind(failure.detail)
            .bind(failure.exit_code)
            .execute(&mut *transaction).await?.last_insert_rowid();
        for (index, event) in events.iter().take(12).enumerate() {
            sqlx::query("INSERT INTO download_failure_events (failure_rowid, event_index, stage, detail, exit_code) VALUES (?, ?, ?, ?, ?)")
                .bind(rowid).bind(index as i64 + 1).bind(event.stage).bind(event.detail)
                .bind(event.exit_code).execute(&mut *transaction).await?;
        }
        if events.is_empty() {
            sqlx::query("INSERT INTO download_failure_events (failure_rowid, event_index, stage, detail, exit_code) VALUES (?, 1, ?, ?, ?)")
                .bind(rowid).bind(failure.stage).bind(failure.detail).bind(failure.exit_code)
                .execute(&mut *transaction).await?;
        }
        sqlx::query("DELETE FROM download_failure_events WHERE failure_rowid NOT IN (SELECT rowid FROM download_failures ORDER BY rowid DESC LIMIT 200)")
            .execute(&mut *transaction).await?;
        sqlx::query("DELETE FROM download_failures WHERE rowid NOT IN (SELECT rowid FROM download_failures ORDER BY rowid DESC LIMIT 200)")
            .execute(&mut *transaction).await?;
        transaction.commit().await
    }.await;
    if result.is_err() {
        tracing::error!("Не удалось сохранить диагностику загрузки");
    }
}

pub(crate) async fn report_incomplete_delivery(
    bot: &Bot,
    chat_id: ChatId,
    cfg: &crate::Settings,
    url: &Url,
    result: crate::DownloadResult,
    report: crate::DeliveryReport,
) {
    let skipped = result.selection.skipped;
    let selection_detail = if result.selection.size_skipped != 0 {
        "Size limit"
    } else {
        "Media skipped"
    };
    let diagnostic = if let Some(error) = &report.error {
        error.diagnostic
    } else if let Some(failure) = result.diagnostic {
        failure
    } else if result.twitter_photos_failed {
        FailureDiagnostic::new("metadata", "Photo metadata failed")
    } else {
        FailureDiagnostic::new("selection", selection_detail)
    };
    let user_diagnostic = source_user_diagnostic(diagnostic, &result.events);
    let mut events = result.events;
    if skipped != 0 && diagnostic.stage != "selection" {
        trace_event(
            &mut events,
            FailureDiagnostic::new("selection", selection_detail),
        );
    }
    if report.error.is_some() || events.is_empty() {
        trace_event(&mut events, diagnostic);
    }
    record_failure_with_events(&cfg.database, url, diagnostic, &events).await;
    let mut warning = format!(
        "Не все медиа доставлены (отправлено {} из {}); исходная ссылка сохранена.",
        report.delivered,
        result.selection.files.len() + skipped
    );
    if result.limited {
        warning.push_str(" Достигнут лимит места во временной папке.");
    } else if !result.process_ok {
        warning.push_str(" Загрузка завершилась с ошибкой.");
    }
    if result.twitter_photos_failed {
        warning.push_str(" Фото X не удалось извлечь.");
    }
    if result.selection.size_skipped != 0 {
        warning.push_str(&format!(
            " Пропущено из-за лимита размера: {}.",
            result.selection.size_skipped
        ));
    } else if skipped != 0 {
        warning.push_str(&format!(" Некоторые файлы пропущены ({skipped} шт.)."));
    }
    warning.push(' ');
    if let Some(error) = report.error {
        warning.push_str(&error.reason);
    } else {
        warning.push_str(&crate::user_errors::diagnostic_message(
            user_diagnostic,
            cfg.max_media_bytes,
        ));
    }
    warning.push_str(&format_failure_context(url, &events));
    let _ = bot
        .send_message(chat_id, warning)
        .disable_notification(true)
        .await;
}
