//! Owner memory: `bot_self_memory` is canonical, Qdrant is a write-through
//! index for code facts only. Owner directives (`source_url` empty) stay fully
//! injected on every turn; ranking them could drop a directive that does not
//! match the current query, which the owner prompt forbids.
//!
//! A Qdrant or embedding failure never loses a write: the row is committed
//! first, the mirror is logged, and the indexer's idempotent backfill repairs
//! the point later.

use crate::qdrant::QdrantClient;
use crate::*;

pub(crate) async fn record_self_fact(
    chat: &ChatConfig,
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
    let normalized_source_url;
    let source_url = match source_url
        .trim()
        .strip_prefix("https://github.com/labi-le/maxwell/blob/main/")
    {
        Some(rest) => {
            normalized_source_url = format!("{}/{rest}", crate::code_inspect::GITHUB_BLOB_BASE);
            normalized_source_url.as_str()
        }
        None => source_url.trim(),
    };
    if topic.is_empty() {
        return Err("укажи topic".into());
    }
    if fact.is_empty() {
        if !source_url.is_empty() {
            return Err("укажи fact".into());
        }
        let deleted = delete_self_fact(pool, &topic).await?;
        if let Some(client) = QdrantClient::from_config() {
            if let Err(reason) = crate::qdrant_index::delete_self_points(&client, &deleted).await {
                tracing::warn!("self memory qdrant delete failed: {reason}");
            }
        }
        return Ok(());
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
                "source_url должен быть ссылкой https://github.com/labi-le/maksut/blob/main/<path>[#L<n>]"
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
    // Eviction is part of the write, so the index must drop the same topics;
    // otherwise a later read could still surface an evicted fact from Qdrant.
    let evicted: Vec<String> = sqlx::query_scalar(
        "DELETE FROM bot_self_memory WHERE topic NOT IN (SELECT topic FROM bot_self_memory ORDER BY updated_at DESC, topic ASC LIMIT ?) RETURNING topic",
    )
    .bind(config().code.memory_max_rows)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    if let Some(client) = QdrantClient::from_config() {
        if !evicted.is_empty() {
            if let Err(reason) = crate::qdrant_index::delete_self_points(&client, &evicted).await {
                tracing::warn!("self memory qdrant eviction failed: {reason}");
            }
        }
        if !source_url.is_empty() {
            match crate::qdrant_index::index_self_fact(chat, &client, &topic, &fact, source_url)
                .await
            {
                Ok(()) => {}
                Err(reason) => tracing::warn!("self memory qdrant write failed: {reason}"),
            }
        }
    }
    Ok(())
}

/// Deletes a topic from `bot_self_memory`. When the exact topic key is absent,
/// falls back to a case-insensitive or stem match over existing topic keys so
/// `remember_self(topic="денис", fact="")` removes `"обращение к денису"`.
async fn delete_self_fact(pool: &SqlitePool, topic: &str) -> Result<Vec<String>, String> {
    let exact = sqlx::query("DELETE FROM bot_self_memory WHERE topic = ?")
        .bind(topic)
        .execute(pool)
        .await
        .map_err(|_| "не удалось обновить память".to_string())?;
    if exact.rows_affected() > 0 {
        return Ok(vec![topic.to_owned()]);
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
    Ok(doomed)
}

fn render_self_memory(rows: impl Iterator<Item = (String, String, String)>) -> Option<String> {
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
    (!out.trim().is_empty()).then_some(out.trim_end().to_owned())
}

pub(crate) async fn load_self_memory(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT topic, fact, source_url FROM bot_self_memory ORDER BY updated_at DESC, topic ASC LIMIT ?",
    )
    .bind(config().code.memory_max_rows)
    .fetch_all(pool)
    .await?;
    Ok(render_self_memory(rows.into_iter()))
}

/// Memory for one chat turn. Owner directives (`source_url` empty) are always
/// injected in full; only code facts are ranked through Qdrant, and the
/// fallback is the complete SQLite set, so a failed index never drops a fact.
pub(crate) async fn load_self_memory_for(
    chat: &ChatConfig,
    pool: &SqlitePool,
    query: &str,
) -> Result<Option<String>, sqlx::Error> {
    let client = QdrantClient::from_config();
    load_self_memory_with(chat, pool, query, client.as_ref()).await
}

/// Same selection with an explicit index client, so a test can exercise the
/// ranked path without installing the process-wide configuration.
pub(crate) async fn load_self_memory_with(
    chat: &ChatConfig,
    pool: &SqlitePool,
    query: &str,
    client: Option<&QdrantClient>,
) -> Result<Option<String>, sqlx::Error> {
    let Some(client) = client else {
        return load_self_memory(pool).await;
    };
    let directives: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT topic, fact, source_url FROM bot_self_memory \
         WHERE trim(source_url) = '' ORDER BY updated_at DESC, topic ASC LIMIT ?",
    )
    .bind(config().code.memory_max_rows)
    .fetch_all(pool)
    .await?;
    let fact_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM bot_self_memory WHERE trim(source_url) <> ''")
            .fetch_one(pool)
            .await?;
    // Ranking only pays for itself when facts exceed what the prompt can hold.
    let facts = if fact_count > 0 {
        if fact_count as usize <= config().memory.self_fact_limit {
            load_self_facts(pool).await?
        } else {
            match ranked_self_facts(chat, client, query, fact_count as usize).await {
                Some(rows) => rows,
                None => load_self_facts(pool).await?,
            }
        }
    } else {
        Vec::new()
    };
    Ok(render_self_memory(directives.into_iter().chain(facts)))
}

async fn load_self_facts(pool: &SqlitePool) -> Result<Vec<(String, String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT topic, fact, source_url FROM bot_self_memory \
         WHERE trim(source_url) <> '' ORDER BY updated_at DESC, topic ASC LIMIT ?",
    )
    .bind(config().code.memory_max_rows)
    .fetch_all(pool)
    .await
}

async fn ranked_self_facts(
    chat: &ChatConfig,
    client: &QdrantClient,
    query: &str,
    fact_count: usize,
) -> Option<Vec<(String, String, String)>> {
    let limit = config().memory.self_fact_limit.min(fact_count.max(1));
    match crate::qdrant_index::search_self_facts(chat, client, query, limit).await {
        Ok(facts) if !facts.is_empty() => Some(
            facts
                .into_iter()
                .map(|fact| (fact.topic, fact.fact, fact.source_url))
                .collect(),
        ),
        Ok(_) => None,
        Err(reason) => {
            tracing::warn!("self memory qdrant search failed: {reason}");
            None
        }
    }
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

const KNOWN_PARTICIPANT_STEMS: &[&str] = &[
    "дени", "вади", "даня", "дани", "ваня", "вани", "влад", "кири", "андр", "олег", "венд", "игор",
];

fn directive_topic(line: &str) -> Option<&str> {
    line.strip_prefix("- [owner] ")?
        .split_once(':')
        .map(|(t, _)| t.trim())
}

fn topic_names_other_participant(line: &str, speaker_stems: &[String]) -> bool {
    let Some(topic) = directive_topic(line) else {
        return false;
    };
    let topic_hit = topic
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .any(|w| {
            let lower = w.to_lowercase();
            KNOWN_PARTICIPANT_STEMS.iter().any(|p| lower.starts_with(p))
                && !speaker_stems.iter().any(|s| lower.starts_with(s))
        });
    if topic_hit {
        return true;
    }
    line.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .any(|w| {
            let lower = w.to_lowercase();
            KNOWN_PARTICIPANT_STEMS
                .iter()
                .any(|p| *p != "дени" && lower.starts_with(p))
                && !speaker_stems.iter().any(|s| lower.starts_with(s))
        })
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
    if !line_handles.is_empty() {
        return line_handles
            .iter()
            .any(|lh| speaker_handles.iter().any(|sh| sh == lh))
            || topic_matches_speaker(line, speaker_stems);
    }
    topic_matches_speaker(line, speaker_stems)
        || !topic_names_other_participant(line, speaker_stems)
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

fn prompt_inspects_directives(prompt: &str) -> bool {
    const STEMS: &str = "памят директив запомн забуд удал список помниш обща общени обраща обращени называе отвечае разговарива";
    const WORDS: &str = "правило правила правил правилам правилах правилами кого кому";
    prompt
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(|w| {
            WORDS.split_ascii_whitespace().any(|k| k == w)
                || STEMS.split_ascii_whitespace().any(|s| w.starts_with(s))
        })
}

pub(crate) fn filter_self_memory_for_prompt(
    self_memory: &str,
    sender_name: &str,
    username: Option<&str>,
    owner: bool,
    prompt: &str,
) -> String {
    if owner && !prompt_inspects_directives(prompt) {
        let handles = speaker_handles(sender_name, username);
        let stems = speaker_name_stems(sender_name, owner);
        return self_memory
            .lines()
            .filter(|l| {
                !l.starts_with("- [owner]")
                    || line_matches_speaker(l, &handles, &stems)
                    || directive_topic(l).is_some_and(|t| {
                        let tl = t.to_lowercase();
                        !tl.is_empty() && prompt.to_lowercase().contains(&tl)
                    })
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    filter_self_memory(self_memory, sender_name, username, owner)
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
