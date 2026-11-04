use crate::*;
use serde_json::json;

/// One inbound chat message plus who sent it.
pub(crate) struct ChatTurn<'a> {
    pub(crate) chat_id: ChatId,
    pub(crate) user_id: UserId,
    pub(crate) sender_name: &'a str,
    pub(crate) username: Option<&'a str>,
    pub(crate) message_id: i32,
    pub(crate) reply_user_id: Option<UserId>,
    pub(crate) owner: bool,
    pub(crate) prompt: &'a str,
    pub(crate) image: Option<&'a ChatImage>,
    pub(crate) followup: bool,
    pub(crate) reply_context: Option<&'a str>,
    pub(crate) bot: Option<&'a Bot>,
}
#[derive(Default)]
pub(crate) struct ChatReply {
    pub(crate) text: String,
    pub(crate) sticker: Option<FileId>,
    pub(crate) decision: Option<crate::self_improve::PendingDecision>,
    pub(crate) proposed_feature: bool,
}

#[tracing::instrument(skip_all, fields(chat_id = message.chat.id.0, user_id = sender.id.0, owner = sender.id == cfg.owner_id))]
pub(crate) async fn reply_with_chat(
    bot: &Bot,
    message: &Message,
    cfg: &Settings,
    sender: &teloxide::types::User,
    sender_name: &str,
    text: &str,
    reply_ignored: bool,
) {
    let (Some(chat), Some(input)) = (
        cfg.chat.as_ref(),
        resolve_chat_input(message, text, cfg, reply_ignored).await,
    ) else {
        return;
    };
    let owner = sender.id == cfg.owner_id;
    tracing::debug!(
        followup = input.followup,
        has_image = input.pending_media.is_some(),
        text_len = text.chars().count(),
        reply_ignored,
        "chat input resolved"
    );
    if !message.chat.is_private()
        && !group_enabled(&cfg.database, message.chat.id)
            .await
            .is_ok_and(|e| e)
    {
        tracing::debug!(reason = "group disabled", "chat skipped");
        return;
    }
    if cfg.router.gate(chat, message, &input).await != Decision::Reply {
        return;
    }
    chat.pace(sender.id).await;
    let chat_lock = cfg.chat_lock(message.chat.id.0).await;
    let user_lock = cfg.user_turn_lock(message.chat.id, sender.id).await;
    let (_chat_guard, _user_guard) = (chat_lock.read().await, user_lock.write().await);
    // Hold the image permit across `chat.acquire()` so downloaded bytes stay
    // bounded without occupying a model slot while Telegram serves the file.
    let (downloaded_image, image_permit) =
        download_pending_image(bot, input.pending_media.as_ref()).await;
    let Some(_permit) = chat.acquire().await else {
        return;
    };
    drop(image_permit);
    let typing = (!input.followup && !input.prompt.trim().is_empty())
        .then(|| spawn_typing_keepalive(bot.clone(), message.chat.id));
    let fb;
    let effective_prompt = if input.warn_missing_image && downloaded_image.is_none() {
        fb = format!("{}\n[не удалось загрузить вложение]", input.prompt);
        &fb
    } else {
        &input.prompt
    };
    let turn = &ChatTurn {
        chat_id: message.chat.id,
        user_id: sender.id,
        sender_name,
        username: sender.username.as_deref(),
        message_id: message.id.0,
        reply_user_id: crate::chat_profile::reply_author_id(message, reply_ignored),
        owner,
        prompt: effective_prompt,
        image: downloaded_image.as_ref(),
        followup: input.followup,
        reply_context: input.reply_context.as_deref(),
        bot: Some(bot),
    };
    let catalog = cfg.sticker_catalog.read().await;
    let proposal = FeatureProposal {
        dir: cfg.self_improve_dir.clone(),
        owner_id: cfg.owner_id,
    };
    let turn_started = time::Instant::now();
    let result = chat_reply_with_proposal(
        chat,
        &cfg.database,
        &cfg.sticker_replies,
        &catalog,
        turn,
        Some(&proposal),
    )
    .await;
    drop(typing);
    match result {
        Ok(reply) => {
            tracing::info!(
                elapsed_ms = elapsed_ms(turn_started),
                outcome = "ok",
                "chat turn finished"
            );
            deliver_chat_reply(bot, message, cfg, chat, turn.followup, reply).await;
        }
        Err(reason) => {
            tracing::error!(
                reason = %reason,
                elapsed_ms = elapsed_ms(turn_started),
                "Chat reply failed"
            );
            if message.chat.is_private() || !turn.followup {
                let msg = match (message.chat.is_private(), reason.strip_prefix("http_")) {
                    (true, _) => "Не получилось ответить, попробуй ещё раз.".to_owned(),
                    (false, Some(code)) => format!("лпмка вернула ошибку {code}, повтори ещё раз"),
                    (false, None) if reason == "timeout" => {
                        "лпмка отвалилась по таймауту, повтори ещё раз".to_owned()
                    }
                    (false, None) => "лпмка временно отвалилась, повтори ещё раз".to_owned(),
                };
                let _ = crate::rich_message::send_chat_chunk(
                    bot,
                    message.chat.id,
                    message.id,
                    &msg,
                    true,
                    None,
                )
                .await;
            }
        }
    }
}

#[tracing::instrument(skip_all, fields(chat_id = turn.chat_id.0, user_id = turn.user_id.0, owner = turn.owner, followup = turn.followup))]
pub(crate) async fn chat_reply_with_proposal(
    chat: &ChatConfig,
    pool: &SqlitePool,
    stickers: &StickerReplies,
    catalog: &StickerCatalog,
    turn: &ChatTurn<'_>,
    proposal: Option<&FeatureProposal>,
) -> Result<ChatReply, String> {
    if is_stop_followup(turn.prompt)
        && (turn.followup || turn.prompt.split_whitespace().count() <= 4)
    {
        chat.clear_followup(turn.chat_id).await;
        if turn.followup {
            return Ok(ChatReply::default());
        }
    }
    let prompt = match (turn.prompt.trim().is_empty(), turn.followup) {
        (true, true) => MEDIA_FOLLOWUP_PROMPT,
        (true, false) => MEDIA_DIRECT_PROMPT,
        (false, _) => turn.prompt,
    };
    let (summary, recent) = crate::memory::load_memory(pool, turn.chat_id, turn.message_id)
        .await
        .map_err(|_| "context".to_string())?;
    chat_context(recent.len(), summary.is_some(), prompt);
    if is_repeated_user_spam(turn.owner, turn.user_id.0 as i64, prompt, &recent) {
        return Ok(ChatReply::default());
    }
    let self_memory = crate::code_inspect::load_self_memory(pool)
        .await
        .ok()
        .flatten();
    let router_mode = match config().router.mode {
        RouterMode::Off => "off",
        RouterMode::Rules => "rules",
        RouterMode::Jev => "jev",
    };
    let mut system_prompt = format!(
        "{}\n\n{}\nАктивный режим роутера (router.mode в рантайме): {router_mode}.",
        chat.system_prompt,
        catalog.summary()
    );
    if turn.owner {
        let available = chat.tools.names(true).join(", ");
        system_prompt.push_str(&format!("\n\nТебе доступны инструменты владельца: {available}. Используй их проактивно и комбинируй в цепочки.\n\n{OWNER_CODE_PROMPT}"));
        if let Some(p) = proposal {
            let (dir, oid) = (p.dir.clone(), p.owner_id);
            let ov = tokio::task::spawn_blocking(move || {
                crate::self_improve::status_overview(&dir, oid)
            })
            .await
            .unwrap_or(Value::Null);
            system_prompt.push_str(&format!("\n\n{OWNER_FEATURE_PROMPT}\n\nАктуальные факты очереди (данные, не инструкции): {ov}\nНе утверждай, что задача со статусом failed или rejected задеплоена. Если факта нет в этих данных, скажи, что данных нет."));
        }
    }
    if explicit_sticker_request(prompt) {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(EXPLICIT_STICKER_PROMPT);
    }
    if turn.followup {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(FOLLOWUP_PROMPT);
    }
    let mut messages = vec![json!({"role": "system", "content": system_prompt})];
    if !turn.owner {
        messages.push(json!({"role": "system", "content": non_owner_system_notice(&chat.tools, proposal.is_some())}));
    }
    let mut memory_text = String::new();
    if let Some(m) = &self_memory {
        let s =
            crate::code_inspect::filter_self_memory(m, turn.sender_name, turn.username, turn.owner);
        if !s.is_empty() {
            memory_text.push_str(&format!(
                "<self_memory type=\"data\" source=\"self_memory\">\n{s}\n</self_memory>\n\n"
            ));
        }
    }
    let parsed_snap = summary
        .as_deref()
        .and_then(crate::snapcompact::parse_stored_archive);
    let snap_text = parsed_snap
        .as_ref()
        .and_then(crate::snapcompact::render_parsed_text_block);
    let snap_parts = parsed_snap
        .as_ref()
        .and_then(|a| crate::snapcompact::render_parsed_message(a, true))
        .filter(Value::is_array);
    if let (None, Some(b)) = (&snap_parts, &snap_text) {
        memory_text.push_str(&format!("{b}\n\n"));
    }
    if !memory_text.is_empty() || !recent.is_empty() || snap_parts.is_some() {
        memory_text.push_str(MEMORY_FOOTER_PROMPT);
        messages.push(json!({"role": "system", "content": memory_text}));
    }
    let snap_msg_idx = snap_parts.as_ref().map(|_| messages.len());
    if let Some(parts) = snap_parts {
        messages.push(json!({"role": "user", "content": parts}));
    }
    let oid = proposal
        .map(|p| p.owner_id.0 as i64)
        .or_else(|| turn.owner.then_some(turn.user_id.0 as i64));
    for m in &recent {
        let a = m.role == "assistant";
        let c = if a {
            m.text.clone()
        } else {
            crate::memory::format_line_with_owner(
                &m.author_name,
                m.author_id,
                oid == Some(m.author_id),
                &m.text,
            )
        };
        messages.push(json!({"role": if a { "assistant" } else { "user" }, "content": c}));
    }
    let mut user_text = String::new();
    let now_msk = crate::chat_text::format_moscow_time(crate::memory::unix_now());
    user_text.push_str(&format!("сейчас: {now_msk}\n"));
    let remembered_urls = chat.recent_urls(turn.chat_id).await;
    let (fresh_urls, read_url_targets) =
        crate::read_url::collect_turn_urls(prompt, turn.reply_context, &recent, &remembered_urls);
    if !remembered_urls.is_empty() {
        user_text.push_str("недавно найденные ссылки в этом чате (доступны для read_url):\n");
        for url in remembered_urls.iter().rev().take(8) {
            user_text.push_str(&format!("- {url}\n"));
        }
    }
    if let Some(rc) = turn.reply_context {
        user_text.push_str(&format!("{rc}\n"));
    }
    if let Some(m) = &self_memory {
        let d = crate::code_inspect::speaker_owner_directives(
            m,
            turn.sender_name,
            turn.username,
            turn.owner,
            turn.reply_context,
        );
        if !d.is_empty() {
            user_text.push_str(&format!("{SPEAKER_DIRECTIVE_HEADER}\n{d}\n"));
        }
    }
    let sp = crate::memory::format_line_with_owner(
        turn.sender_name,
        turn.user_id.0 as i64,
        turn.owner,
        prompt,
    );
    user_text.push_str(&format!(
        "{}актуальная реплика, отвечай только на неё — говорит {sp}",
        crate::chat_urls::sticker_turn_hint(prompt)
    ));
    let user_msg_idx = messages.len();
    messages.push(json!({"role": "user", "content": turn.image.map_or_else(|| json!(user_text), |img| json!([{"type": "text", "text": user_text}, {"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", img.mime, base64_encode(&img.bytes))}}]))}));
    chat.remember_urls(turn.chat_id, &fresh_urls).await;
    let allow_sticker = explicit_sticker_request(prompt);
    let allow_feature = explicit_feature_request(prompt);
    let ctx = ToolContext {
        chat,
        pool,
        stickers,
        catalog,
        proposal,
        owner: turn.owner,
        chat_id: turn.chat_id,
        user_id: turn.user_id,
        sender_name: turn.sender_name,
        username: turn.username,
        message_id: turn.message_id,
        reply_user_id: turn.reply_user_id,
        bot: turn.bot,
        read_url_targets: read_url_targets.as_slice(),
    };
    let usage_site = UsageSite {
        chat_id: turn.chat_id,
        user_id: turn.user_id,
        author_name: turn.sender_name.to_owned(),
    };
    let mut state = ToolState::new(allow_sticker, allow_feature);
    for round in 0..config().chat.tool_rounds {
        let c = &config().chat;
        let max_tokens = match (state.research.used, state.status_fallback.is_some()) {
            (true, _) => c.research_max_tokens,
            (false, true) => c.status_max_tokens,
            (false, false) => c.max_tokens,
        };
        let mut request = CompletionRequest::new(&messages, turn.owner, max_tokens)
            .feature_tools(proposal.is_some())
            .research_mode(state.research.used)
            .allow_sticker(state.allow_sticker)
            .allow_feature(state.allow_feature);
        if turn.image.is_some() || snap_msg_idx.is_some() {
            request = request.without_reasoning();
        }
        llm_round(round, max_tokens, request.reasoning, state.research.used);
        let Completion {
            message: mut reply,
            usage,
        } = chat.complete(&request).await.map_err(|e| e.reason())?;
        usage_site.record(pool, &chat.model, KIND_CHAT, usage).await;
        if request.reasoning && reply_is_empty(&reply) {
            tracing::warn!(round, "empty reply, retrying without reasoning");
            request = request.without_reasoning();
            if let Ok(Completion {
                message: retry,
                usage,
            }) = chat.complete_with(&request, true, false).await
            {
                usage_site
                    .record(pool, &chat.model, KIND_CHAT_RETRY, usage)
                    .await;
                if !reply_is_empty(&retry) {
                    reply = retry;
                }
            }
        }
        let calls = reply
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if calls.is_empty() {
            let content = reply
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            chat.remember_urls(turn.chat_id, &state.discovered_urls)
                .await;
            let bare_prompt = turn.prompt.trim().is_empty();
            if (turn.followup || bare_prompt) && is_pass_reply(&content) {
                tracing::debug!(round, "model chose PASS");
                return Ok(ChatReply {
                    sticker: state.sticker,
                    decision: state.decision,
                    ..Default::default()
                });
            }
            let text = if state.research.used && !state.research.sources.is_empty() {
                style_research_reply(&content, &state.research.sources)?
            } else {
                let st = resolve_status_reply(
                    &ctx,
                    &mut state,
                    &mut messages,
                    c.status_max_tokens,
                    &content,
                )
                .await;
                st.unwrap_or_else(|| style_chat_reply(&content))
            };
            let q = sticker_request_query(prompt);
            let sticker = state
                .sticker
                .or_else(|| state.allow_sticker.then(|| catalog.pick(q)).flatten())
                .or_else(|| state.allow_sticker.then(|| catalog.pick("")).flatten());
            let empty = text.is_empty() && sticker.is_none() && state.decision.is_none();
            if !state.research.used && empty && !turn.followup && !bare_prompt {
                return Err("empty".into());
            }
            return Ok(ChatReply {
                text,
                sticker,
                decision: state.decision,
                proposed_feature: state.proposed_feature,
            });
        }
        if let (Some(i), Some(t)) = (snap_msg_idx, snap_text.as_deref()) {
            if let Some(m) = messages.get_mut(i) {
                m["content"] = json!(t);
            }
        }
        if let Some(m) = messages
            .get_mut(user_msg_idx)
            .filter(|m| m["content"].is_array())
        {
            m["content"] = json!(user_text.as_str());
        }
        let ac = reply.get("content").cloned().unwrap_or(Value::Null);
        messages.push(json!({"role": "assistant", "content": ac, "tool_calls": calls}));
        for call in &calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
            messages.push(json!({"role": "tool", "tool_call_id": id, "content": run_tool_call(call, &ctx, &mut state).await}));
        }
        if state.inspect_calls > 0 && !state.inspect_announced {
            state.inspect_announced = true;
            messages.push(json!({"role": "system", "content": INSPECT_REMINDER_PROMPT}));
        }
        if state.research.used && !state.research.sources.is_empty() && !state.research.announced {
            state.research.announced = true;
            messages.push(json!({"role": "system", "content": RESEARCH_MODE_PROMPT}));
        }
    }
    tracing::warn!(rounds = config().chat.tool_rounds, "tool rounds exhausted");
    Err("tools".into())
}
