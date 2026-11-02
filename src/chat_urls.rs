use crate::chat_client::ChatConfig;
use crate::prelude::*;

pub(crate) struct RecentUrls {
    updated_at: time::Instant,
    urls: Vec<Url>,
}

impl ChatConfig {
    pub(crate) async fn remember_urls(&self, chat_id: ChatId, urls: &[Url]) {
        if urls.is_empty() {
            return;
        }
        let mut recent = self.recent_urls.lock().await;
        let now = time::Instant::now();
        let ttl = Duration::from_secs(config().chat.discovered_url_ttl_secs);
        let limit = config().chat.max_active_chats;
        let per_chat = config().chat.discovered_urls_per_chat;
        if recent.len() >= limit && !recent.contains_key(&chat_id) {
            recent.retain(|_, entry| now.duration_since(entry.updated_at) < ttl);
            if recent.len() >= limit {
                if let Some(oldest) = recent
                    .iter()
                    .min_by_key(|(_, entry)| entry.updated_at)
                    .map(|(chat, _)| *chat)
                {
                    recent.remove(&oldest);
                }
            }
        }
        let entry = recent.entry(chat_id).or_insert_with(|| RecentUrls {
            updated_at: now,
            urls: Vec::with_capacity(urls.len().min(per_chat)),
        });
        if now.duration_since(entry.updated_at) >= ttl {
            entry.urls.clear();
        }
        entry.updated_at = now;
        for url in urls {
            if let Some(pos) = entry.urls.iter().position(|existing| existing == url) {
                entry.urls.remove(pos);
            }
            if entry.urls.len() >= per_chat {
                entry.urls.remove(0);
            }
            entry.urls.push(url.clone());
        }
    }

    pub(crate) async fn recent_urls(&self, chat_id: ChatId) -> Vec<Url> {
        let mut recent = self.recent_urls.lock().await;
        let now = time::Instant::now();
        let ttl = Duration::from_secs(config().chat.discovered_url_ttl_secs);
        match recent.get(&chat_id) {
            Some(entry) if now.duration_since(entry.updated_at) < ttl => entry.urls.clone(),
            Some(_) => {
                recent.remove(&chat_id);
                Vec::new()
            }
            None => Vec::new(),
        }
    }
}
