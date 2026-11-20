use crate::prelude::*;

/// One release row as the bot shows it: title, size, swarm, and where to read
/// or fetch it. `download_url` is the tracker's own `.torrent` endpoint, so a
/// hit that carries it can also be attached to the reply as a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TorrentHit {
    pub(crate) title: String,
    pub(crate) size: String,
    pub(crate) seeds: u64,
    pub(crate) leechers: u64,
    pub(crate) page_url: String,
    pub(crate) download_url: Option<String>,
    pub(crate) provider: &'static str,
}

pub(crate) struct TorrentSearch {
    pub(crate) hits: Vec<TorrentHit>,
    /// At least one configured provider was reachable but failed, so the
    /// dossier can say the tracker pass was incomplete instead of pretending
    /// that nothing was found.
    pub(crate) failed: bool,
}

/// Release words that make a tracker pass worth one request. The gate is
/// deliberately a substring match on the whole question: a false positive
/// costs one bounded request, a false negative hides the feature.
const INTENT_WORDS: [&str; 34] = [
    "торрент",
    "torrent",
    "раздач",
    "rutracker",
    "рутрекер",
    "скачать",
    "фильм",
    "movie",
    "film",
    "сериал",
    "series",
    "season",
    "сезон",
    "эпизод",
    "episode",
    "мультфильм",
    "cartoon",
    "аниме",
    "anime",
    "музык",
    "music",
    "альбом",
    "album",
    "audiobook",
    "аудиокниг",
    "blu-ray",
    "bdrip",
    "web-dl",
    "webrip",
    "hdtv",
    "remux",
    "x264",
    "x265",
    "hevc",
];

pub(crate) fn wants_torrents(question: &str, queries: &[String]) -> bool {
    let mut haystack = question.to_lowercase();
    for query in queries {
        haystack.push(' ');
        haystack.push_str(&query.to_lowercase());
    }
    INTENT_WORDS.iter().any(|word| haystack.contains(word))
}

/// One tracker pass for a release query. Providers run in parallel; a provider
/// that fails marks the pass incomplete but never hides the other provider's
/// hits.
pub(crate) async fn search(query: &str, limit: usize) -> TorrentSearch {
    let budget = Duration::from_secs(config().read_url.total_timeout_secs);
    let (apibay, rutracker) = tokio::join!(
        time::timeout(budget, crate::torrent_providers::apibay(query, limit)),
        time::timeout(budget, crate::torrent_providers::rutracker(query, limit)),
    );
    let mut hits = Vec::new();
    let mut failed = false;
    for (provider, result) in [("apibay", apibay), ("rutracker", rutracker)] {
        match result {
            Ok(Ok(found)) => hits.extend(found),
            Ok(Err(reason)) => {
                tracing::info!(provider, reason = %reason, "tracker search failed");
                failed = true;
            }
            Err(_) => {
                tracing::info!(provider, "tracker search timed out");
                failed = true;
            }
        }
    }
    hits.sort_by(|left, right| {
        right
            .seeds
            .cmp(&left.seeds)
            .then(left.title.cmp(&right.title))
    });
    hits.truncate(limit);
    TorrentSearch { hits, failed }
}
