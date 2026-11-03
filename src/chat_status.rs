use crate::self_improve::short_task_id;
use serde_json::Value;

fn status_label(status: &str) -> &'static str {
    match status {
        "queued" => "в очереди",
        "proposed" => "ждёт решения владельца",
        "issue_created" => "issue создана",
        "planned" => "план готов",
        "deployed" => "задеплоено",
        "failed" => "провалено",
        "rejected" => "отклонено",
        _ => "неизвестно",
    }
}

fn task_status_label(task: &Value) -> String {
    let status = task.get("status").and_then(Value::as_str).unwrap_or("");
    let approved = task.get("already_decided").and_then(Value::as_str) == Some("approve");
    let has_issue = task
        .get("issue_url")
        .and_then(Value::as_str)
        .is_some_and(|url| !url.trim().is_empty());
    match (status, approved, has_issue) {
        ("proposed", true, _) => "одобрена, создаётся issue".to_owned(),
        ("planned", _, true) => "issue создана".to_owned(),
        _ => status_label(status).to_owned(),
    }
}

fn task_goal_suffix(task: &Value) -> String {
    let goal = task
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if goal.is_empty() {
        String::new()
    } else {
        format!(": {goal}")
    }
}

fn task_issue_url(task: &Value) -> Option<&str> {
    task.get("issue_url")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("https://github.com/"))
}

fn status_details_answer(tasks: &[Value], overview: Option<&Value>) -> String {
    if tasks.is_empty() {
        return "Очередь пуста".to_owned();
    }
    if let [task] = tasks {
        let id = task.get("id").and_then(Value::as_str).unwrap_or("без id");
        let mut line = format!(
            "#{}: {}{}",
            short_task_id(id),
            task_status_label(task),
            task_goal_suffix(task)
        );
        if let Some(url) = task_issue_url(task) {
            line.push('\n');
            line.push_str(url);
        }
        return line;
    }
    let mut lines = vec!["Очередь".to_owned()];
    for task in tasks {
        let Some(status) = task.get("status").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(status, "queued" | "proposed" | "issue_created") {
            continue;
        }
        let id = task.get("id").and_then(Value::as_str).unwrap_or("без id");
        lines.push(format!(
            "• #{} — {}{}",
            short_task_id(id),
            task_status_label(task),
            task_goal_suffix(task)
        ));
        if let Some(url) = task_issue_url(task) {
            lines.push(format!("  {url}"));
        }
    }
    if lines.len() == 1 {
        lines.push("• активных нет".to_owned());
    }
    status_totals(&mut lines, tasks, overview);
    lines.join("\n")
}

fn status_summary_answer(tasks: &[Value], overview: Option<&Value>) -> String {
    let mut lines = vec!["Очередь".to_owned()];
    let mut active = 0;
    for task in tasks {
        let Some(status) = task.get("status").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(status, "queued" | "proposed") {
            continue;
        }
        let id = task.get("id").and_then(Value::as_str).unwrap_or("без id");
        lines.push(format!(
            "• #{} — {}",
            short_task_id(id),
            task_status_label(task)
        ));
        active += 1;
    }
    if active == 0 {
        lines.push("• активных нет".to_owned());
    }
    status_totals(&mut lines, tasks, overview);
    lines.join("\n")
}

fn status_links_answer(tasks: &[Value]) -> String {
    let mut links = Vec::new();
    for task in tasks {
        let Some(status) = task.get("status").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(status, "issue_created" | "planned" | "proposed") {
            continue;
        }
        let Some(url) = task_issue_url(task) else {
            continue;
        };
        let id = task.get("id").and_then(Value::as_str).unwrap_or("без id");
        links.push(format!(
            "#{} — {}{}\n{url}",
            short_task_id(id),
            task_status_label(task),
            task_goal_suffix(task)
        ));
    }
    if links.is_empty() {
        "Ссылок на issue нет.".to_owned()
    } else {
        format!("Ссылки на issue:\n\n{}", links.join("\n\n"))
    }
}

fn status_totals(lines: &mut Vec<String>, tasks: &[Value], overview: Option<&Value>) {
    let mut totals = Vec::new();
    for (status, label) in [
        ("issue_created", "В issue"),
        ("planned", "План готов"),
        ("deployed", "Задеплоено"),
        ("failed", "Провалено"),
        ("rejected", "Отклонено"),
    ] {
        let count = overview
            .and_then(|value| value.get("counts"))
            .and_then(|counts| counts.get(status))
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                tasks
                    .iter()
                    .filter(|task| {
                        let raw = task.get("status").and_then(Value::as_str).unwrap_or("");
                        let has_issue = task
                            .get("issue_url")
                            .and_then(Value::as_str)
                            .is_some_and(|url| !url.trim().is_empty());
                        let effective = if raw == "planned" && has_issue {
                            "issue_created"
                        } else {
                            raw
                        };
                        effective == status
                    })
                    .count() as u64
            });
        if count > 0 {
            totals.push(format!("{label}: {count}"));
        }
    }
    if !totals.is_empty() {
        lines.push(String::new());
        lines.push("Итоги".to_owned());
        lines.push(totals.join(" · "));
    }
}

pub(crate) fn status_answer(snapshot: &Value, overview: Option<&Value>, view: &str) -> String {
    let tasks = snapshot
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if tasks.is_empty() {
        return match snapshot.get("error").and_then(Value::as_str) {
            Some(error) => format!("Не вижу: {error}"),
            None => "Очередь пуста".to_owned(),
        };
    }
    match view {
        "links" => status_links_answer(&tasks),
        "summary" => status_summary_answer(&tasks, overview),
        _ => status_details_answer(&tasks, overview),
    }
}

fn task_matches_line(task: &Value, line: &str) -> bool {
    let Some(id) = task.get("id").and_then(Value::as_str) else {
        return false;
    };
    if line.contains(id) {
        return true;
    }
    let short = short_task_id(id);
    short != id && line.contains(&format!("#{short}"))
}

fn status_claims(line: &str) -> Vec<&'static str> {
    let lower = line.to_lowercase();
    let mut claims = Vec::new();
    for (needle, status) in [
        ("задепло", "deployed"),
        ("провал", "failed"),
        ("отклон", "rejected"),
        ("в очереди", "queued"),
        ("issue создана", "issue_created"),
        ("ишью создана", "issue_created"),
        ("план готов", "planned"),
        ("ждёт решения", "proposed"),
        ("ждет решения", "proposed"),
        ("создаётся issue", "proposed"),
        ("создается issue", "proposed"),
        ("deployed", "deployed"),
        ("failed", "failed"),
        ("rejected", "rejected"),
        ("queued", "queued"),
        ("issue_created", "issue_created"),
        ("planned", "planned"),
        ("proposed", "proposed"),
    ] {
        let Some(index) = lower.find(needle) else {
            continue;
        };
        let Some(prefix) = lower.get(..index) else {
            continue;
        };
        let negated = prefix.ends_with("не ") || prefix.ends_with("not ") || prefix.ends_with("не");
        if !negated && !claims.contains(&status) {
            claims.push(status);
        }
    }
    claims
}

fn status_line_has_unknown_reference(line: &str, tasks: &[Value]) -> bool {
    for raw in line.split_whitespace() {
        let token = raw.trim_matches(|ch: char| {
            matches!(
                ch,
                '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ';' | ':'
            )
        });
        if let Some(rest) = token.strip_prefix('#') {
            let looks_like_id = rest.starts_with("propose-")
                || (rest.len() >= 4 && rest.chars().all(|ch| ch.is_ascii_digit()));
            if looks_like_id {
                let known = tasks.iter().any(|task| {
                    task.get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| short_task_id(id) == rest || id == rest)
                });
                if !known {
                    return true;
                }
            }
        }
        if (token.starts_with("plan-") || token.starts_with("propose-"))
            && !tasks
                .iter()
                .any(|task| task.get("id").and_then(Value::as_str) == Some(token))
        {
            return true;
        }
        if token.starts_with("https://github.com/")
            && !tasks.iter().any(|task| task_issue_url(task) == Some(token))
        {
            return true;
        }
    }
    false
}

fn status_line_claims_emptiness(line: &str) -> bool {
    if line.contains('?') {
        return false;
    }
    let lower = line.to_lowercase();
    [
        "очередь пуста",
        "пустая очередь",
        "активных нет",
        "нет активных",
        "ничего нет",
        "нет задач",
        "no active",
        "empty",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// The model may phrase a status answer freely, but a claim attached to a task
/// must match the snapshot. This rejects the classic "everything is deployed"
/// hallucination while leaving room for explanations and clarifying questions.
pub(crate) fn status_answer_grounded(text: &str, snapshot: Option<&Value>, view: &str) -> bool {
    let Some(snapshot) = snapshot else {
        return false;
    };
    let tasks = snapshot
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if text.trim().is_empty() {
        return false;
    }
    let mut has_url = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if status_line_has_unknown_reference(line, &tasks)
            || (!tasks.is_empty() && status_line_claims_emptiness(line))
        {
            return false;
        }
        has_url |= line.contains("https://github.com/");
        let matched: Vec<&Value> = tasks
            .iter()
            .filter(|task| task_matches_line(task, line))
            .collect();
        let claims = status_claims(line);
        if claims.is_empty() {
            continue;
        }
        if matched.is_empty() {
            if line.contains('?') {
                continue;
            }
            return false;
        }
        let actual: Vec<&str> = matched
            .iter()
            .filter_map(|task| {
                let raw = task.get("status").and_then(Value::as_str)?;
                let has_issue = task
                    .get("issue_url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| !url.trim().is_empty());
                Some(if raw == "planned" && has_issue {
                    "issue_created"
                } else {
                    raw
                })
            })
            .collect();
        let Some(primary) = actual.first() else {
            return false;
        };
        if actual.iter().any(|status| status != primary) {
            return false;
        }
        if claims.iter().any(|claim| claim != primary) {
            return false;
        }
    }
    if view == "links" && tasks.iter().any(|task| task_issue_url(task).is_some()) && !has_url {
        return false;
    }
    true
}

/// Detailed answers keep their own lines and URLs; the one-line chat style
/// would cut issue links at the 280-character boundary.
pub(crate) fn style_status_reply(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !lines.is_empty() && lines.last().is_some_and(|line: &String| !line.is_empty()) {
                lines.push(String::new());
            }
            continue;
        }
        lines.push(line.trim_end_matches(['.', '…']).to_owned());
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

pub(crate) async fn resolve_status_reply(
    ctx: &crate::ToolContext<'_>,
    state: &mut crate::ToolState,
    messages: &mut Vec<Value>,
    max_tokens: u32,
    content: &str,
) -> Option<String> {
    let fallback = state.status_fallback.take()?;
    let snapshot = state.status_snapshot.as_ref();
    let view = state.status_view.as_str();
    if status_answer_grounded(content, snapshot, view) {
        return Some(style_status_reply(&crate::self_improve::redact_secret(
            content,
            &ctx.chat.api_key,
        )));
    }
    // Let the model correct itself once with the snapshot in context. If it
    // still contradicts the data, deliver the deterministic fallback.
    let repair = format!(
        "Твой предыдущий ответ не сходится с данными feature_status. \
         Ответь ещё раз по этим данным; если данных не хватает, задай один \
         короткий уточняющий вопрос. Данные: {}",
        snapshot.map(Value::to_string).unwrap_or_default(),
    );
    messages.push(serde_json::json!({"role": "system", "content": repair}));
    let repair_request = crate::CompletionRequest::new(messages, ctx.owner, max_tokens)
        .feature_tools(ctx.proposal.is_some());
    let repaired = ctx
        .chat
        .complete_with(&repair_request, false, false)
        .await
        .ok()
        .and_then(|reply| {
            reply
                .get("content")
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_owned)
        });
    Some(match repaired {
        Some(repaired) if status_answer_grounded(&repaired, snapshot, view) => style_status_reply(
            &crate::self_improve::redact_secret(&repaired, &ctx.chat.api_key),
        ),
        _ => fallback,
    })
}
