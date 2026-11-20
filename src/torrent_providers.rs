use crate::bounded_fetch::{fetch, Fetched};
use crate::cookies::{page_cookies_async, CookieScope};
use crate::prelude::*;
use crate::torrent::TorrentHit;

const APIBAY_ENDPOINT: &str = "https://apibay.org/q.php";
const RUTRACKER_ENDPOINT: &str = "https://rutracker.org/forum/tracker.php";
const ACCEPT: &str = "text/html,application/json;q=0.9,*/*;q=0.5";

/// Rutracker hands the session back to `tracker.php` only when the jar carries
/// `bb_session`; a jar without it fails closed in `page_cookies_async`.
const RUTRACKER_SCOPE: CookieScope<'static> = CookieScope {
    subdomain_hosts: &["rutracker.org", "www.rutracker.org"],
    exact_hosts: &["rutracker.org", "www.rutracker.org"],
    auth_names: &["bb_session"],
};

fn rutracker_host(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("rutracker.org") || host.eq_ignore_ascii_case("www.rutracker.org")
    })
}

fn rutracker_jar() -> Option<PathBuf> {
    config()
        .paths
        .rutracker_cookies_file
        .as_deref()
        .map(PathBuf::from)
}

/// Cookie header for one tracker host, or `None` for any other host. The
/// `.torrent` download lives on the same tracker and needs the same session,
/// so the attachment path resolves credentials here instead of guessing.
pub(crate) async fn download_cookie(url: &Url) -> Result<Option<String>, String> {
    if !rutracker_host(url) {
        return Ok(None);
    }
    let Some(source) = rutracker_jar() else {
        return Ok(None);
    };
    page_cookies_async(Some(&source), url, RUTRACKER_SCOPE)
        .await
        .map_err(|_| "cookies трекера не подготовлены".to_string())
}

pub(crate) async fn apibay(query: &str, limit: usize) -> Result<Vec<TorrentHit>, String> {
    let mut url = Url::parse(APIBAY_ENDPOINT).map_err(|_| "адрес apibay невалиден".to_string())?;
    url.query_pairs_mut().append_pair("q", query);
    let fetched = fetch(url, ACCEPT, None, config().research.torrent_max_bytes).await?;
    crate::torrent_parse::parse_apibay(&decode(fetched, encoding_rs::UTF_8), limit)
}

/// Skipped, not failed, when no jar is configured: trackers are an optional
/// source, and a bot without tracker credentials is a valid deployment.
pub(crate) async fn rutracker(query: &str, limit: usize) -> Result<Vec<TorrentHit>, String> {
    let Some(source) = rutracker_jar() else {
        return Ok(Vec::new());
    };
    let mut url =
        Url::parse(RUTRACKER_ENDPOINT).map_err(|_| "адрес трекера невалиден".to_string())?;
    url.query_pairs_mut().append_pair("nm", query);
    let cookie = page_cookies_async(Some(&source), &url, RUTRACKER_SCOPE)
        .await
        .map_err(|_| "cookies трекера не подготовлены".to_string())?;
    let fetched = fetch(
        url,
        ACCEPT,
        cookie.as_deref(),
        config().research.torrent_max_bytes,
    )
    .await?;
    // Rutracker serves windows-1251 without a charset header on the tracker page.
    Ok(crate::torrent_parse::parse_rutracker(
        &decode(fetched, encoding_rs::WINDOWS_1251),
        limit,
    ))
}

fn decode(fetched: Fetched, fallback: &'static encoding_rs::Encoding) -> String {
    let encoding = fetched
        .content_type
        .split(';')
        .skip(1)
        .find_map(|part| {
            let (key, value) = part.split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| {
                    encoding_rs::Encoding::for_label(value.trim().trim_matches('"').as_bytes())
                })
                .flatten()
        })
        .unwrap_or(fallback);
    encoding.decode(&fetched.body).0.into_owned()
}
