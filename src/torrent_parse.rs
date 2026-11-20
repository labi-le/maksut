use crate::prelude::*;
use crate::torrent::TorrentHit;

const APIBAY_PAGE: &str = "https://thepiratebay.org/description.php?id=";
const RUTRACKER_PAGE: &str = "https://rutracker.org/forum/viewtopic.php?t=";
const RUTRACKER_DOWNLOAD: &str = "https://rutracker.org/forum/dl.php?t=";

/// One apibay JSON page. A missing or malformed array is a provider failure,
/// not "no results": the difference decides whether the dossier admits the
/// tracker pass was incomplete.
pub(crate) fn parse_apibay(body: &str, limit: usize) -> Result<Vec<TorrentHit>, String> {
    let parsed: Value =
        serde_json::from_str(body).map_err(|_| "ответ apibay не разобран".to_string())?;
    let rows = parsed
        .as_array()
        .ok_or_else(|| "ответ apibay не массив".to_string())?;
    let mut hits = Vec::new();
    for row in rows {
        if hits.len() >= limit {
            break;
        }
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id.is_empty() || id == "0" || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let title = row
            .get("name")
            .and_then(Value::as_str)
            .map(clean_title)
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        let size = row
            .get("size")
            .and_then(Value::as_str)
            .and_then(|bytes| bytes.parse::<u64>().ok())
            .map(human_size)
            .unwrap_or_default();
        hits.push(TorrentHit {
            title,
            size,
            seeds: counter(row.get("seeders")),
            leechers: counter(row.get("leechers")),
            page_url: format!("{APIBAY_PAGE}{id}"),
            // apibay serves metadata only; the tracker's own .torrent endpoint
            // is not part of the protocol, so no file is attached for a hit.
            download_url: None,
            provider: "apibay",
        });
    }
    Ok(hits)
}

/// Rutracker search markup has changed class names across redesigns, so the
/// parser keys on the topic id and reads the cells it knows, skipping rows it
/// cannot map instead of failing the whole page.
pub(crate) fn parse_rutracker(html: &str, limit: usize) -> Vec<TorrentHit> {
    // The tracker body is untrusted; reject pathological nesting before the
    // parser walks it, same bound as `read_url`.
    if crate::read_url_html::html_is_too_complex(html) {
        return Vec::new();
    }
    let doc = dom_query::Document::from(html);
    let mut hits = Vec::new();
    for row in doc.select("tr[data-topic_id], tr[id^='trs-tr-']").iter() {
        if hits.len() >= limit {
            break;
        }
        let Some(id) = row_topic_id(
            row.attr("data-topic_id").as_deref(),
            row.attr("id").as_deref(),
        ) else {
            continue;
        };
        let title = clean_title(
            row.select_single(".t-title, a.tLink, .tt-text")
                .text()
                .as_ref(),
        );
        if title.is_empty() {
            continue;
        }
        hits.push(TorrentHit {
            title,
            size: clean_title(row.select_single("td.tor-size").text().as_ref()),
            seeds: counter_text(row.select_single("b.seedmed, .seedmed").text().as_ref()),
            leechers: counter_text(
                row.select_single("span.leechmed, .leechmed")
                    .text()
                    .as_ref(),
            ),
            page_url: format!("{RUTRACKER_PAGE}{id}"),
            download_url: Some(format!("{RUTRACKER_DOWNLOAD}{id}")),
            provider: "rutracker",
        });
    }
    hits
}

fn row_topic_id(topic: Option<&str>, row: Option<&str>) -> Option<String> {
    let id = match topic {
        Some(id) => id,
        None => row?.strip_prefix("trs-tr-")?,
    };
    let id = id.trim();
    (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then(|| id.to_owned())
}

fn counter(value: Option<&Value>) -> u64 {
    value
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .unwrap_or(0)
}

fn counter_text(raw: &str) -> u64 {
    raw.chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

pub(crate) fn clean_title(raw: &str) -> String {
    let capped = crate::chat_text::truncate_text(raw.trim(), config().research.title_chars);
    capped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decimal units with one digit below 10, matching what trackers print.
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let label = UNITS.get(unit).copied().unwrap_or("TB");
    if value < 10.0 {
        format!("{value:.1} {label}")
    } else {
        format!("{value:.0} {label}")
    }
}

/// One dossier line: what the release is, how big, how alive, and where.
pub(crate) fn hit_snippet(hit: &TorrentHit) -> String {
    let size = if hit.size.is_empty() {
        "размер неизвестен".to_string()
    } else {
        hit.size.clone()
    };
    format!(
        "{size}, сиды {}, личи {}, {}",
        hit.seeds, hit.leechers, hit.provider
    )
}
