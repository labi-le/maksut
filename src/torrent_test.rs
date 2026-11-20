use crate::config;
use crate::torrent::wants_torrents;
use crate::torrent_parse::{clean_title, hit_snippet, human_size, parse_apibay, parse_rutracker};

fn apibay_body() -> String {
    serde_json::json!([
        {
            "id": "12345",
            "name": "Dune Part Two 2024 1080p WEB-DL",
            "info_hash": "abc",
            "leechers": "12",
            "seeders": "340",
            "size": "1468006400",
            "num_files": "1",
            "status": "vip"
        },
        {
            "id": "0",
            "name": "No results returned",
            "info_hash": "0000",
            "leechers": "0",
            "seeders": "0",
            "size": "0",
            "num_files": "0",
            "status": ""
        }
    ])
    .to_string()
}

#[test]
fn apibay_rows_become_hits() {
    let hits = parse_apibay(&apibay_body(), 5).unwrap();
    assert_eq!(hits.len(), 1, "the id-0 sentinel is not a release");
    let hit = &hits[0];
    assert_eq!(hit.title, "Dune Part Two 2024 1080p WEB-DL");
    assert_eq!(hit.seeds, 340);
    assert_eq!(hit.leechers, 12);
    assert_eq!(hit.size, "1.4 GB");
    assert_eq!(
        hit.page_url,
        "https://thepiratebay.org/description.php?id=12345"
    );
    assert!(hit.download_url.is_none());
    assert_eq!(hit.provider, "apibay");
}

#[test]
fn apibay_failure_and_empty_are_different_outcomes() {
    assert!(parse_apibay("not json", 5).is_err());
    assert!(parse_apibay("{}", 5).is_err());
    assert!(parse_apibay("[]", 5).unwrap().is_empty());
}

#[test]
fn apibay_respects_the_row_limit() {
    let rows: Vec<serde_json::Value> = (1..=5)
        .map(|id| {
            serde_json::json!({
                "id": id.to_string(),
                "name": format!("release {id}"),
                "leechers": "1",
                "seeders": "2",
                "size": "1024",
                "status": "vip"
            })
        })
        .collect();
    let body = serde_json::Value::Array(rows).to_string();
    assert_eq!(parse_apibay(&body, 2).unwrap().len(), 2);
}

fn rutracker_html() -> &'static str {
    r#"
    <table id="tor-tbl">
      <tr id="trs-tr-6240001" data-topic_id="6240001">
        <td class="t-title"><a class="tLink" href="viewtopic.php?t=6240001">Дюна: Часть вторая / Dune: Part Two (2024)</a></td>
        <td class="tor-size"><a href="dl.php?t=6240001">1.46 GB</a></td>
        <td><b class="seedmed">340</b></td>
        <td><span class="leechmed">12</span></td>
      </tr>
      <tr data-topic_id="6240002">
        <td class="t-title"><a class="tLink">Дюна (2021) BDRip</a></td>
        <td class="tor-size">2.10 GB</td>
        <td><b class="seedmed">7</b></td>
        <td><span class="leechmed">1</span></td>
      </tr>
      <tr><td>row without a topic</td></tr>
    </table>
    "#
}

#[test]
fn rutracker_rows_carry_page_and_download_urls() {
    let hits = parse_rutracker(rutracker_html(), 5);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].title, "Дюна: Часть вторая / Dune: Part Two (2024)");
    assert_eq!(hits[0].size, "1.46 GB");
    assert_eq!(hits[0].seeds, 340);
    assert_eq!(hits[0].leechers, 12);
    assert_eq!(
        hits[0].page_url,
        "https://rutracker.org/forum/viewtopic.php?t=6240001"
    );
    assert_eq!(
        hits[0].download_url.as_deref(),
        Some("https://rutracker.org/forum/dl.php?t=6240001")
    );
    assert_eq!(hits[1].seeds, 7);
}

#[test]
fn rutracker_row_limit_is_enforced() {
    assert_eq!(parse_rutracker(rutracker_html(), 1).len(), 1);
}

#[test]
fn human_size_switches_units() {
    assert_eq!(human_size(512), "512 B");
    assert_eq!(human_size(1536), "1.5 KB");
    assert_eq!(human_size(1048576), "1.0 MB");
    assert_eq!(human_size(1468006400), "1.4 GB");
    assert_eq!(human_size(2 * 1024_u64.pow(4)), "2.0 TB");
}

#[test]
fn tracker_intent_gate_matches_release_words_only() {
    assert!(wants_torrents("найди фильм Дюна", &[]));
    assert!(wants_torrents("найди симс 3", &[]));
    assert!(wants_torrents("найди репак игры", &[]));
    assert!(wants_torrents(
        "что нового",
        &["сериал 2024 смотреть".into()]
    ));
    assert!(wants_torrents("Dune 2024 WEB-DL", &[]));
    assert!(!wants_torrents("как устроен borrow checker", &[]));
    assert!(!wants_torrents(
        "что такое lifetimes",
        &["rust lifetimes".into()]
    ));
    assert!(
        !wants_torrents("как настроить миграцию базы", &[]),
        "'миграцию' must not trigger on the 'игр' prefix stem"
    );
    assert!(!wants_torrents("тигр в зоопарке", &[]));
}

#[test]
fn title_is_collapsed_and_capped() {
    assert_eq!(clean_title("  a \n b  "), "a b");
    let long = "x".repeat(10_000);
    assert!(clean_title(&long).chars().count() <= config().research.title_chars);
}

#[test]
fn snippet_states_size_and_swarm() {
    let hit = parse_rutracker(rutracker_html(), 1).remove(0);
    let snippet = hit_snippet(&hit);
    assert!(snippet.contains("1.46 GB"), "{snippet}");
    assert!(snippet.contains("сиды 340"), "{snippet}");
    assert!(snippet.contains("личи 12"), "{snippet}");
    assert!(snippet.contains("rutracker"), "{snippet}");
}
