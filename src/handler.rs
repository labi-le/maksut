use crate::*;

/// Replies with the sticker for the first standalone «да»/«нет» in the message.
/// Only the owner can trigger actions; other participants are read-only.
async fn reply_with_sticker(bot: &Bot, message: &Message, cfg: &Settings, text: &str) -> bool {
    if !message
        .from
        .as_ref()
        .is_some_and(|user| user.id == cfg.owner_id)
    {
        return false;
    }
    // A standalone «да»/«нет» must not hide a pending plan/deploy gate from
    // the model. The read-only probe writes nothing.
    let improve_dir = cfg.self_improve_dir.clone();
    let owner_id = cfg.owner_id;
    let has_pending = tokio::task::spawn_blocking(move || {
        crate::self_improve::has_pending_decision(&improve_dir, owner_id)
    })
    .await
    .unwrap_or(false);
    if has_pending {
        return false;
    }
    let Some(unique_id) = sticker_rule(text) else {
        return false;
    };
    let Some(file_id) = cfg.sticker_replies.file_id(unique_id) else {
        return false;
    };
    if !message.chat.is_private()
        && !group_enabled(&cfg.database, message.chat.id)
            .await
            .is_ok_and(|enabled| enabled)
    {
        return false;
    }
    let request = bot
        .send_sticker(message.chat.id, InputFile::file_id(file_id.clone()))
        .reply_parameters(ReplyParameters::new(message.id).allow_sending_without_reply())
        .disable_notification(true);
    if let Ok(sent) = request.await {
        let _ = crate::memory::record_message(
            &cfg.database,
            RecordMessage::new(message.chat.id, 0, "максут", "assistant", "[стикер]")
                .with_message_id(Some(sent.id.0))
                .with_flags(crate::memory::FLAG_JUNK),
        )
        .await;
    }
    true
}

#[tracing::instrument(skip_all, fields(chat_id = message.chat.id.0, message_id = message.id.0, user_id = message.from.as_ref().map(|user| user.id.0), owner = message.from.as_ref().is_some_and(|user| user.id == cfg.owner_id)))]
pub(crate) async fn handle_message(
    bot: Bot,
    message: Message,
    cfg: Arc<Settings>,
) -> ResponseResult<()> {
    let text = message.text().or_else(|| message.caption()).unwrap_or("");
    let current_media = message_image(&message);
    let reply_media = message.reply_to_message().and_then(message_image);
    let has_sticker = message.sticker().is_some();
    let has_voice = crate::voice::pending(&message);
    let has_doc = crate::chat_doc::pending(&message);
    if text.is_empty()
        && current_media.is_none()
        && reply_media.is_none()
        && !has_sticker
        && !has_voice
        && !has_doc
    {
        tracing::debug!("skip: no text, media, sticker, voice or doc");
        return Ok(());
    }
    let Some(sender) = message.from.as_ref().filter(|user| !user.is_bot) else {
        tracing::debug!("skip: sender is a bot");
        return Ok(());
    };
    let owner = sender.id == cfg.owner_id;
    let sender_name = display_author_name(sender);
    let forwarded = message.forward_origin().is_some();
    let now = crate::memory::unix_now();
    let reply_ignored =
        crate::memory::reply_ignored(&cfg.database, &message, cfg.owner_id, now).await;
    // Ignored senders and non-owner private chats never reach the model, so
    // paying for a download and an STT call before those gates would be waste.
    let sender_ignored = crate::memory::sender_ignored(&cfg.database, sender.id, owner, now).await;
    // The transcript or document body is untrusted user input: it becomes the
    // message text before `detect_flags` and `injection_attempt` run.
    let typed = text;
    let voice = crate::voice::turn(
        &bot,
        &message,
        &cfg,
        typed,
        owner,
        sender_ignored,
        reply_ignored,
    )
    .await;
    let after_voice = voice.text.as_deref().unwrap_or(typed);
    let doc = crate::chat_doc::turn(
        &bot,
        &message,
        &cfg,
        after_voice,
        owner,
        sender_ignored,
        reply_ignored,
    )
    .await;
    let text = doc.text.as_deref().unwrap_or(after_voice);
    tracing::debug!(
        voice = voice.text.is_some(),
        doc = doc.text.is_some(),
        quoted = voice.quoted || doc.quoted,
        has_media = current_media.is_some() || reply_media.is_some(),
        has_sticker,
        text_len = text.chars().count(),
        forwarded,
        "message received"
    );
    let injection_text = crate::chat_doc::select_injection_text(
        typed,
        voice.text.as_deref(),
        voice.quoted,
        doc.text.as_deref(),
        doc.quoted,
        text,
    );
    let raw_log_text = if text.trim().is_empty() {
        "[вложение]"
    } else {
        text
    };
    let mut flags = crate::memory_guard::detect_flags(raw_log_text);
    if doc.text.is_some()
        && (flags & (crate::memory::FLAG_INJECTION | crate::memory::FLAG_SECRET) != 0)
        && (!owner || (flags & crate::memory::FLAG_SECRET != 0))
    {
        crate::chat_doc::clear_recent_doc(message.chat.id).await;
    }
    if text.trim().is_empty() && (current_media.is_some() || reply_media.is_some() || has_sticker) {
        flags |= crate::memory::FLAG_JUNK;
    }
    let log_body;
    let log_text = if forwarded {
        log_body = format!("[переслано] {raw_log_text}");
        log_body.as_str()
    } else {
        raw_log_text
    };
    let record_user = |fl: i64| {
        let rec = user_record(&message, sender.id.0 as i64, &sender_name, log_text, fl);
        crate::memory::record_message(&cfg.database, rec)
    };
    if sender_ignored {
        tracing::info!(reason = "ignored", "message skipped");
        let _ = record_user(flags | crate::memory::FLAG_IGNORED).await;
        return Ok(());
    }
    if !owner {
        let group = message.chat.is_group() || message.chat.is_supergroup();
        if group
            && crate::memory_guard::injection_attempt(&injection_text)
            && group_enabled(&cfg.database, message.chat.id)
                .await
                .is_ok_and(|enabled| enabled)
        {
            if let Err(error) = crate::memory::ignore_user(
                &cfg.database,
                sender.id,
                now,
                message.chat.id,
                message.id.0,
            )
            .await
            {
                tracing::error!("Ignore write failed: {error}");
            }
            tracing::info!(reason = "injection", "user ignored");
            let _ =
                record_user(flags | crate::memory::FLAG_INJECTION | crate::memory::FLAG_IGNORED)
                    .await;
            return Ok(());
        }
    }
    let media = current_media.or_else(|| (!reply_ignored).then_some(reply_media).flatten());
    if text.is_empty() && media.is_none() && !has_sticker {
        tracing::debug!(reason = "empty after reply filter", "message skipped");
        return Ok(());
    }
    if typed.trim_start().starts_with('/') {
        tracing::debug!("routing: command");
        flags |= crate::memory::FLAG_JUNK;
        let _ = record_user(flags).await;
        if crate::self_improve::handle_improve(&bot, &message, &cfg, typed).await {
            return Ok(());
        }
        if let Some(command) = maksut_command(typed, &cfg.bot_username) {
            handle_command(&bot, &message, &cfg, command).await;
        }
        return Ok(());
    }
    if message.chat.is_private() && sender.id != cfg.owner_id {
        tracing::info!(reason = "private non-owner", "message skipped");
        let _ = record_user(flags).await;
        return Ok(());
    }
    if !message.chat.is_private() && !message.chat.is_group() && !message.chat.is_supergroup() {
        tracing::debug!(reason = "unsupported chat kind", "message skipped");
        return Ok(());
    }
    let _ = record_user(flags).await;
    if owner
        && message.chat.is_private()
        && crate::self_improve::handle_owner_text(&bot, &message, &cfg, typed).await
    {
        return Ok(());
    }
    let Some(url) = first_link(typed) else {
        if reply_with_sticker(&bot, &message, &cfg, typed).await {
            tracing::debug!(reason = "owner sticker", "message handled");
            return Ok(());
        }
        tracing::debug!(followup = reply_ignored, "routing: chat turn");
        reply_with_chat(
            &bot,
            &message,
            &cfg,
            sender,
            sender_name.as_str(),
            text,
            reply_ignored,
        )
        .await;
        return Ok(());
    };
    let user_semaphore = cfg.user_rate_limit(sender.id).await;
    let Ok(_user_permit) = user_semaphore.try_acquire_owned() else {
        tracing::warn!(reason = "rate limit", "download rejected");
        record_failure(
            &cfg.database,
            &url,
            FailureDiagnostic::new("queue", "Rate limit"),
        )
        .await;
        return Ok(());
    };
    if !message.chat.is_private() {
        match group_enabled(&cfg.database, message.chat.id).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(reason = "group disabled", "download skipped");
                return Ok(());
            }
            Err(_) => {
                record_failure(
                    &cfg.database,
                    &url,
                    FailureDiagnostic::new("settings", "Database unavailable"),
                )
                .await;
                return Ok(());
            }
        }
    }
    let Ok(_permit) = cfg.downloads.acquire().await else {
        tracing::warn!(reason = "download slot closed", "download rejected");
        record_failure(
            &cfg.database,
            &url,
            FailureDiagnostic::new("queue", "Download unavailable"),
        )
        .await;
        return Ok(());
    };
    // Recheck after queueing: an administrator may have disabled the group while we waited.
    if !message.chat.is_private() {
        match group_enabled(&cfg.database, message.chat.id).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(reason = "group disabled", "download skipped");
                return Ok(());
            }
            Err(_) => {
                record_failure(
                    &cfg.database,
                    &url,
                    FailureDiagnostic::new("settings", "Database unavailable"),
                )
                .await;
                return Ok(());
            }
        }
    }
    let key = cache_key(bot.token(), &url);
    if let Ok(Some(cached)) = lookup_cache(&cfg.database, message.chat.id.0, &key).await {
        tracing::debug!(items = cached.len(), "media cache hit");
        let mut items = Vec::with_capacity(cached.len());
        for item in &cached {
            let caption = match media_caption(sender, &url, item.description.as_deref()) {
                Ok(caption) => caption,
                Err(reason) => {
                    record_failure(
                        &cfg.database,
                        &url,
                        FailureDiagnostic::new("caption", "Caption too long"),
                    )
                    .await;
                    let _ = bot
                        .send_message(message.chat.id, reason)
                        .disable_notification(true)
                        .await;
                    return Ok(());
                }
            };
            items.push(DeliveryItem {
                input: Some(InputFile::file_id(FileId(item.file_id.clone()))),
                kind: item.kind,
                caption,
                description: item.description.as_deref(),
            });
        }
        let report = deliver_items(&bot, message.chat.id, items, cfg.max_media_bytes, false).await;
        match report.error {
            None => {
                record_media_delivery(&cfg.database, message.chat.id, &url, &report).await;
                delete_source(&bot, &message).await;
                return Ok(());
            }
            Some(error) => {
                if error.invalid_cached_file {
                    let _ = invalidate_cache(&cfg.database, message.chat.id.0, &key).await;
                }
                if !error.invalid_cached_file || report.delivered != 0 {
                    record_failure(&cfg.database, &url, error.diagnostic).await;
                    let warning = format!(
                        "Не все медиа доставлены; исходная ссылка сохранена. {}",
                        error.reason
                    );
                    let _ = bot
                        .send_message(message.chat.id, warning)
                        .disable_notification(true)
                        .await;
                    return Ok(());
                }
                // Definitively invalid IDs before any delivery permit one fresh attempt below.
            }
        }
    }
    let dir = match tempfile::Builder::new()
        .prefix("post-")
        .tempdir_in(&cfg.download_dir)
    {
        Ok(dir) => dir,
        Err(_) => {
            record_failure(
                &cfg.database,
                &url,
                FailureDiagnostic::new("storage", "Download directory unavailable"),
            )
            .await;
            let _ = bot
                .send_message(message.chat.id, "Не удалось создать папку загрузки.")
                .disable_notification(true)
                .await;
            return Ok(());
        }
    };
    let deadline = time::Instant::now() + Duration::from_secs(config().media.download_timeout_secs);
    let download_started = time::Instant::now();
    let result = download_with_timeout(
        &url,
        &cfg,
        dir.path(),
        deadline.saturating_duration_since(time::Instant::now()),
    )
    .await;
    let result = match result {
        Ok(result) => result,
        Err(failure) => {
            download_failed(
                failure.diagnostic.stage,
                failure.diagnostic.detail,
                download_started,
            );
            record_failure_with_events(&cfg.database, &url, failure.diagnostic, &failure.events)
                .await;
            let mut reply = if failure.diagnostic.detail == "Download failed"
                && failure.diagnostic.stage != "metadata"
            {
                failure.reason
            } else {
                user_errors::diagnostic_message(
                    source_user_diagnostic(failure.diagnostic, &failure.events),
                    cfg.max_media_bytes,
                )
            };
            reply.push_str(&format_failure_context(&url, &failure.events));
            let _ = bot
                .send_message(message.chat.id, reply)
                .disable_notification(true)
                .await;
            return Ok(());
        }
    };
    if result.selection.files.is_empty() {
        let diagnostic = result.diagnostic.unwrap_or_else(|| {
            FailureDiagnostic::new(
                "selection",
                if result.selection.size_skipped != 0 {
                    "Size limit"
                } else if result.selection.skipped != 0 {
                    "Media skipped"
                } else {
                    "No sendable media"
                },
            )
        });
        let mut events = result.events;
        media_not_selected(diagnostic.stage, diagnostic.detail);
        if events.is_empty() {
            trace_event(&mut events, diagnostic);
        }
        record_failure_with_events(&cfg.database, &url, diagnostic, &events).await;
        let mut reply = user_errors::diagnostic_message(
            source_user_diagnostic(diagnostic, &events),
            cfg.max_media_bytes,
        );
        reply.push_str(&format_failure_context(&url, &events));
        let _ = bot
            .send_message(message.chat.id, reply)
            .disable_notification(true)
            .await;
        return Ok(());
    }
    let report = deliver_fresh(
        &bot,
        message.chat.id,
        &result,
        sender,
        &url,
        cfg.max_media_bytes,
        deadline,
    )
    .await;
    if report.delivered > 0 {
        record_media_delivery(&cfg.database, message.chat.id, &url, &report).await;
    }
    let found = result.selection.files.len();
    let skipped = result.selection.skipped;
    media_finished(found, skipped, report.delivered, download_started);
    let complete = result.process_ok
        && !result.limited
        && !result.twitter_photos_failed
        && result.diagnostic.is_none()
        && skipped == 0
        && report.delivered == found
        && report.error.is_none();
    if complete {
        if let Some(items) = report.cached_items {
            let _ = store_cache(&cfg.database, message.chat.id.0, &key, &items).await;
        }
        delete_source(&bot, &message).await;
        return Ok(());
    }
    report_incomplete_delivery(&bot, message.chat.id, &cfg, &url, result, report).await;
    Ok(())
}

#[tracing::instrument(skip_all, fields(chat_id = query.message.as_ref().map(|message| message.chat().id.0), user_id = query.from.id.0, callback_id = ?query.id))]
pub(crate) async fn handle_callback_query(
    bot: Bot,
    query: teloxide::types::CallbackQuery,
    cfg: Arc<Settings>,
) -> ResponseResult<()> {
    if query.from.id != cfg.owner_id
        && crate::memory::ignore_active(&cfg.database, query.from.id, crate::memory::unix_now())
            .await
            .is_ok_and(|active| active)
    {
        tracing::info!(reason = "ignored", "callback skipped");
        return Ok(());
    }
    tracing::debug!("routing: callback");
    crate::self_improve::handle_callback(&bot, &query, &cfg).await;
    Ok(())
}
