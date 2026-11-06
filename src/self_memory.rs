use crate::prelude::*;

pub(crate) async fn record_self_fact(
    pool: &SqlitePool,
    base: &Path,
    topic: &str,
    fact: &str,
    source_url: &str,
) -> Result<(), String> {
    let topic: String = topic
        .trim()
        .replace("[owner]", "[ owner ]")
        .replace('\n', " ")
        .chars()
        .take(config().code.topic_max_chars)
        .collect();
    let raw_fact = fact
        .trim()
        .replace("[owner]", "[ owner ]")
        .replace('\n', " ");
    let prefix = format!("{}:", topic.to_lowercase());
    let stripped_fact = if raw_fact.to_lowercase().starts_with(&prefix) {
        raw_fact
            .get(prefix.len()..)
            .unwrap_or(&raw_fact)
            .trim_start()
    } else {
        raw_fact.as_str()
    };
    let fact: String = stripped_fact
        .chars()
        .take(config().code.fact_max_chars)
        .collect();
    let source_url = source_url.trim();
    if topic.is_empty() {
        return Err("укажи topic".into());
    }
    if fact.is_empty() {
        if !source_url.is_empty() {
            return Err("укажи fact".into());
        }
        return delete_self_fact(pool, &topic).await;
    }
    let mask = crate::memory::FLAG_INJECTION | crate::memory::FLAG_SECRET;
    if source_url.is_empty() {
        let lowered_topic = topic.to_lowercase();
        let lowered_fact = fact.to_lowercase();
        if (crate::memory_guard::detect_flags(&topic) & crate::memory::FLAG_SECRET) != 0
            || (crate::memory_guard::detect_flags(&fact) & crate::memory::FLAG_SECRET) != 0
            || lowered_topic.contains("http://")
            || lowered_topic.contains("https://")
            || lowered_fact.contains("http://")
            || lowered_fact.contains("https://")
        {
            return Err("заметка отклонена фильтром безопасности".into());
        }
    } else {
        if !crate::code_inspect::valid_self_source_url(base, source_url) {
            return Err(
                "source_url должен быть ссылкой https://github.com/labi-le/maxwell/blob/main/<path>[#L<n>]"
                    .into(),
            );
        }
        if (crate::memory_guard::detect_flags(&topic) & mask) != 0
            || (crate::memory_guard::detect_flags(&fact) & mask) != 0
            || crate::memory::validate_summary(&topic).is_err()
            || crate::memory::validate_summary(&fact).is_err()
        {
            return Err("факт отклонён фильтром безопасности".into());
        }
    }
    let now = crate::memory::unix_now();
    sqlx::query(
        "INSERT INTO bot_self_memory (topic, fact, source_url, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT(topic) DO UPDATE SET fact = excluded.fact, source_url = excluded.source_url, updated_at = excluded.updated_at",
    )
    .bind(&topic)
    .bind(&fact)
    .bind(source_url)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|_| "не удалось сохранить факт".to_string())?;
    let _ = sqlx::query(
        "DELETE FROM bot_self_memory WHERE topic NOT IN (SELECT topic FROM bot_self_memory ORDER BY updated_at DESC, topic ASC LIMIT ?)",
    )
    .bind(config().code.memory_max_rows)
    .execute(pool)
    .await;
    Ok(())
}

/// Deletes a topic from `bot_self_memory`. When the exact topic key is absent,
/// falls back to a case-insensitive or stem match over existing topic keys so
/// `remember_self(topic="денис", fact="")` removes `"обращение к денису"`.
async fn delete_self_fact(pool: &SqlitePool, topic: &str) -> Result<(), String> {
    let exact = sqlx::query("DELETE FROM bot_self_memory WHERE topic = ?")
        .bind(topic)
        .execute(pool)
        .await
        .map_err(|_| "не удалось обновить память".to_string())?;
    if exact.rows_affected() > 0 {
        return Ok(());
    }
    let rows: Vec<(String,)> = sqlx::query_as("SELECT topic FROM bot_self_memory")
        .fetch_all(pool)
        .await
        .map_err(|_| "не удалось обновить память".to_string())?;
    let lower_target = topic.to_lowercase();
    let target_stems: Vec<String> = lower_target
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(|w| w.chars().take(4).collect())
        .collect();
    let mut doomed: Vec<String> = Vec::new();
    for (existing,) in rows {
        let lower_existing = existing.to_lowercase();
        let stem_hit = !target_stems.is_empty()
            && lower_existing
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.chars().count() >= 4)
                .any(|w| target_stems.iter().any(|stem| w.starts_with(stem)));
        if lower_existing == lower_target || lower_existing.contains(&lower_target) || stem_hit {
            doomed.push(existing);
        }
    }
    for chunk in doomed.chunks(64) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let query = format!("DELETE FROM bot_self_memory WHERE topic IN ({placeholders})");
        let mut request = sqlx::query(sqlx::AssertSqlSafe(query));
        for t in chunk {
            request = request.bind(t.as_str());
        }
        request
            .execute(pool)
            .await
            .map_err(|_| "не удалось обновить память".to_string())?;
    }
    Ok(())
}

pub(crate) async fn load_self_memory(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT topic, fact, source_url FROM bot_self_memory ORDER BY updated_at DESC, topic ASC LIMIT ?",
    )
    .bind(config().code.memory_max_rows)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut out = String::new();
    for (topic, fact, source_url) in rows {
        let topic = topic.replace("[owner]", "[ owner ]");
        let fact = fact.replace("[owner]", "[ owner ]");
        let line = if source_url.trim().is_empty() {
            format!("- [owner] {topic}: {fact}\n")
        } else {
            format!("- {topic}: {fact} ({source_url})\n")
        };
        if out.chars().count() + line.chars().count() > config().code.memory_max_chars
            && !out.is_empty()
        {
            break;
        }
        out.push_str(&line);
    }
    Ok((!out.trim().is_empty()).then_some(out.trim_end().to_owned()))
}

fn extract_at_handles(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|w| !w.contains("://") && !w.contains('/'))
        .flat_map(|w| w.split(|c: char| !(c.is_alphanumeric() || c == '@' || c == '_')))
        .filter(|tok| {
            tok.strip_prefix('@')
                .is_some_and(|rest| !rest.is_empty() && !rest.contains('@'))
        })
        .map(str::to_lowercase)
        .collect()
}

fn speaker_handles(sender_name: &str, username: Option<&str>) -> Vec<String> {
    let mut handles = extract_at_handles(sender_name);
    if let Some(u) = username
        .map(|u| u.trim_start_matches('@').trim())
        .filter(|u| !u.is_empty())
    {
        let h = format!("@{}", u.to_lowercase());
        if !handles.contains(&h) {
            handles.push(h);
        }
    }
    handles
}

fn speaker_name_stems(sender_name: &str, owner: bool) -> Vec<String> {
    let head = sender_name.split('(').next().unwrap_or(sender_name);
    let mut stems: Vec<String> = head
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|w| {
            let lower = w.to_lowercase();
            (lower.chars().count() >= 4).then(|| lower.chars().take(4).collect())
        })
        .collect();
    if owner {
        let d = "дени".to_owned();
        if !stems.contains(&d) {
            stems.push(d);
        }
    }
    stems
}

fn directive_topic(line: &str) -> Option<&str> {
    line.strip_prefix("- [owner] ")?
        .split_once(':')
        .map(|(t, _)| t.trim())
}

fn topic_matches_speaker(line: &str, stems: &[String]) -> bool {
    let Some(topic) = directive_topic(line) else {
        return false;
    };
    topic
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .any(|w| {
            let lower = w.to_lowercase();
            stems.iter().any(|s| lower.starts_with(s))
        })
}

fn line_matches_speaker(line: &str, speaker_handles: &[String], speaker_stems: &[String]) -> bool {
    let line_handles = extract_at_handles(line);
    line_handles.is_empty()
        || line_handles
            .iter()
            .any(|lh| speaker_handles.iter().any(|sh| sh == lh))
        || topic_matches_speaker(line, speaker_stems)
}

/// Keeps code-verified facts and, for non-owners, only per-user `- [owner]`
/// directives whose `@handle` or topic name explicitly matches the current
/// speaker (all rows stay visible on owner turns so the owner sees every rule).
pub(crate) fn filter_self_memory(
    self_memory: &str,
    sender_name: &str,
    username: Option<&str>,
    owner: bool,
) -> String {
    if owner {
        return self_memory.to_owned();
    }
    let handles = speaker_handles(sender_name, username);
    let stems = speaker_name_stems(sender_name, owner);
    self_memory
        .lines()
        .filter(|l| {
            !l.starts_with("- [owner]")
                || extract_at_handles(l)
                    .iter()
                    .any(|lh| handles.iter().any(|sh| sh == lh))
                || topic_matches_speaker(l, &stems)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Retains `- [owner]` directives that are general or whose `@handle` or topic
/// name matches the current speaker.
pub(crate) fn speaker_owner_directives(
    self_memory: &str,
    sender_name: &str,
    username: Option<&str>,
    owner: bool,
    _reply_context: Option<&str>,
) -> String {
    let handles = speaker_handles(sender_name, username);
    let stems = speaker_name_stems(sender_name, owner);
    self_memory
        .lines()
        .filter(|l| l.starts_with("- [owner]") && line_matches_speaker(l, &handles, &stems))
        .collect::<Vec<_>>()
        .join("\n")
}
