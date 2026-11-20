use crate::media::MediaKind;
use crate::research::ResearchSource;
use crate::research_attachments::{attachment_kind, select};
use crate::torrent::TorrentHit;

fn source(url: &str) -> ResearchSource {
    ResearchSource {
        id: 1,
        title: "source".into(),
        url: url.into(),
        domain: "example.com".into(),
        snippet: String::new(),
        queries: vec![1],
        best_position: 0,
    }
}

fn hit(page: &str, download: Option<&str>, title: &str) -> TorrentHit {
    TorrentHit {
        title: title.into(),
        size: "1.4 GB".into(),
        seeds: 10,
        leechers: 2,
        page_url: page.into(),
        download_url: download.map(str::to_owned),
        provider: "rutracker",
    }
}

#[test]
fn torrent_files_come_first_then_direct_links() {
    let hits = vec![hit(
        "https://rutracker.org/forum/viewtopic.php?t=1",
        Some("https://rutracker.org/forum/dl.php?t=1"),
        "Дюна 2024",
    )];
    let sources = vec![
        source("https://example.com/page.html"),
        source("https://example.com/book.pdf"),
    ];
    let attachments = select(&sources, &hits, 3);
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[0].filename, "Дюна 2024.torrent");
    assert_eq!(attachments[1].filename, "book.pdf");
}

#[test]
fn limit_and_deduplication_hold() {
    let hits = vec![
        hit(
            "https://rutracker.org/forum/viewtopic.php?t=1",
            Some("https://rutracker.org/forum/dl.php?t=1"),
            "one",
        ),
        hit(
            "https://rutracker.org/forum/viewtopic.php?t=1",
            Some("https://rutracker.org/forum/dl.php?t=1"),
            "one again",
        ),
    ];
    assert_eq!(select(&[], &hits, 5).len(), 1);
    assert_eq!(select(&[], &hits, 0).len(), 0);
}

#[test]
fn torrent_filename_is_display_safe() {
    let hits = vec![hit(
        "https://rutracker.org/forum/viewtopic.php?t=7",
        Some("https://rutracker.org/forum/dl.php?t=7"),
        "a/b\\c:d*e?f\"g<h>i|j",
    )];
    let attachments = select(&[], &hits, 1);
    assert_eq!(attachments[0].filename, "a_b_c_d_e_f_g_h_i_j.torrent");
    assert!(!attachments[0].filename.contains('/'));
}

#[test]
fn empty_title_falls_back_to_a_plain_name() {
    let hits = vec![hit(
        "https://rutracker.org/forum/viewtopic.php?t=8",
        Some("https://rutracker.org/forum/dl.php?t=8"),
        "///",
    )];
    let attachments = select(&[], &hits, 1);
    assert_eq!(attachments[0].filename, "release.torrent");
}

#[test]
fn page_urls_are_not_files() {
    let sources = vec![
        source("https://example.com/article"),
        source("https://example.com/download?file=doc.pdf"),
        source("https://example.com/archive.zip"),
    ];
    let attachments = select(&sources, &[], 3);
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].filename, "archive.zip");
}

#[test]
fn extension_selects_the_telegram_kind() {
    assert_eq!(attachment_kind("x.torrent"), Some(MediaKind::Document));
    assert_eq!(attachment_kind("x.PDF"), Some(MediaKind::Document));
    assert_eq!(attachment_kind("x.jpg"), Some(MediaKind::Photo));
    assert_eq!(attachment_kind("x.mp4"), Some(MediaKind::Document));
    assert_eq!(attachment_kind("x.mp3"), Some(MediaKind::Document));
    assert_eq!(attachment_kind("x.html"), None);
    assert_eq!(attachment_kind("noext"), None);
}

#[test]
fn bidi_and_zero_width_characters_cannot_spoof_a_filename() {
    let hits = vec![hit(
        "https://rutracker.org/forum/viewtopic.php?t=9",
        Some("https://rutracker.org/forum/dl.php?t=9"),
        "film\u{202e}gpj.exe\u{200b}",
    )];
    let attachments = select(&[], &hits, 1);
    let name = &attachments[0].filename;
    assert!(!name.contains('\u{202e}'), "{name}");
    assert!(!name.contains('\u{200b}'), "{name}");
    assert!(name.ends_with(".torrent"), "{name}");
}
