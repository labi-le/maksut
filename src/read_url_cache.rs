use crate::prelude::*;
use crate::read_url::ReadPage;
use std::sync::{LazyLock, Mutex};

struct CachedPage {
    page: ReadPage,
    at: std::time::Instant,
}

static READ_URL_CACHE: LazyLock<Mutex<HashMap<String, CachedPage>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn cache_get(url: &str) -> Option<ReadPage> {
    let mut cache = READ_URL_CACHE.lock().ok()?;
    let now = std::time::Instant::now();
    let ttl = Duration::from_secs(config().read_url.cache_ttl_secs);
    cache.retain(|_, entry| now.duration_since(entry.at) < ttl);
    cache.get(url).map(|entry| entry.page.clone())
}

pub(crate) fn cache_put(url: String, page: ReadPage) {
    let Ok(mut cache) = READ_URL_CACHE.lock() else {
        return;
    };
    let now = std::time::Instant::now();
    let ttl = Duration::from_secs(config().read_url.cache_ttl_secs);
    cache.retain(|_, entry| now.duration_since(entry.at) < ttl);
    if cache.len() >= config().read_url.cache_entries {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.at)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(url, CachedPage { page, at: now });
}
