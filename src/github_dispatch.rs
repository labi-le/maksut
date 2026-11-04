use crate::*;

/// GitHub-specific half of the tool dispatcher: per-reply allowance checks run
/// here, before a call can spend a slot, and only then the request goes to
/// `github_tools`.
pub(crate) async fn run(
    kind: ToolKind,
    args: &Value,
    ctx: &ToolContext<'_>,
    state: &mut ToolState,
) -> String {
    match kind {
        ToolKind::GithubIssue => issue(args, ctx, state).await,
        ToolKind::GithubSearch => search(args, ctx, state).await,
        ToolKind::GithubComment => comment(args, ctx, state).await,
        ToolKind::WebSearch
        | ToolKind::Research
        | ToolKind::ResearchStatus
        | ToolKind::ResearchCancel
        | ToolKind::ReadUrl
        | ToolKind::ProposeFeature
        | ToolKind::FeatureStatus
        | ToolKind::FeatureConfirmation
        | ToolKind::InspectCode
        | ToolKind::ReadLogs
        | ToolKind::ChatProfile
        | ToolKind::UsageStats
        | ToolKind::RememberSelf
        | ToolKind::FindStickers
        | ToolKind::SendSticker
        | ToolKind::ChatSearch
        | ToolKind::ChatIndex => "Инструмент недоступен.".to_owned(),
    }
}

/// Models occasionally send a numeric id as a string; both forms address the
/// same issue, so accept either and reject everything else.
fn arg_number(args: &Value) -> Option<u64> {
    args.get("number").and_then(|value| {
        value.as_u64().or_else(|| {
            value
                .as_str()
                .and_then(|raw| raw.trim().parse::<u64>().ok())
        })
    })
}

fn valid_target(args: &Value) -> Option<u64> {
    arg_number(args).filter(|number| crate::github_tools::valid_number(*number))
}

async fn issue(args: &Value, ctx: &ToolContext<'_>, state: &mut ToolState) -> String {
    let Some(number) = valid_target(args) else {
        return "укажи номер issue или PR".to_owned();
    };
    if state.github_calls >= config().chat.github_calls_per_reply {
        return "лимит обращений к GitHub на ответ".to_owned();
    }
    state.github_calls += 1;
    crate::chat_tools::ack_lookup(ctx);
    let include_comments = args
        .get("include_comments")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let token = crate::github_tools::token_from_env();
    crate::github_tools::issue(&config().github, token.as_deref(), number, include_comments).await
}

async fn search(args: &Value, ctx: &ToolContext<'_>, state: &mut ToolState) -> String {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");
    let label = args.get("label").and_then(Value::as_str).unwrap_or("");
    if let Err(reason) = crate::github_tools::validate_search(query, label) {
        return reason;
    }
    if state.github_calls >= config().chat.github_calls_per_reply {
        return "лимит обращений к GitHub на ответ".to_owned();
    }
    state.github_calls += 1;
    crate::chat_tools::ack_lookup(ctx);
    let filters = crate::github_tools::SearchFilters {
        state: args.get("state").and_then(Value::as_str).unwrap_or("all"),
        label,
        kind: args.get("kind").and_then(Value::as_str).unwrap_or("all"),
        limit: args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .clamp(1, 10) as usize,
    };
    let token = crate::github_tools::token_from_env();
    crate::github_tools::search(&config().github, token.as_deref(), query, filters).await
}

async fn comment(args: &Value, ctx: &ToolContext<'_>, state: &mut ToolState) -> String {
    if !ctx.owner {
        return "github_comment доступен только владельцу бота".to_owned();
    }
    let Some(number) = valid_target(args) else {
        return "укажи номер issue или PR".to_owned();
    };
    let body = args.get("body").and_then(Value::as_str).unwrap_or("");
    let token = crate::github_tools::token_from_env();
    if let Err(reason) = crate::github_tools::validate_comment(token.as_deref(), body) {
        return reason;
    }
    if state.github_comment_calls >= config().chat.github_comment_calls_per_reply {
        return "лимит комментариев в GitHub на ответ".to_owned();
    }
    state.github_comment_calls += 1;
    crate::github_tools::comment(&config().github, token.as_deref(), number, body).await
}
