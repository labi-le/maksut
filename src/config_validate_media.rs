use crate::config_validate::{count, signed, usize_count};
use crate::*;

impl ResearchSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        count("research.cooldown_secs", self.cooldown_secs, 0, 3600)?;
        usize_count("research.min_queries", self.min_queries, 1, 10)?;
        usize_count("research.max_queries", self.max_queries, 1, 10)?;
        if self.max_queries < self.min_queries {
            return Err("research.max_queries must be at least research.min_queries".into());
        }
        usize_count("research.results_per_query", self.results_per_query, 1, 20)?;
        usize_count("research.max_sources", self.max_sources, 1, 50)?;
        usize_count("research.title_chars", self.title_chars, 10, 1000)?;
        usize_count("research.snippet_chars", self.snippet_chars, 10, 5000)?;
        usize_count("research.dossier_chars", self.dossier_chars, 100, 100_000)?;
        count("research.timeout_secs", self.timeout_secs, 1, 300)?;
        usize_count("research.query_min_chars", self.query_min_chars, 1, 100)?;
        usize_count("research.query_max_chars", self.query_max_chars, 1, 1000)?;
        if self.query_max_chars < self.query_min_chars {
            return Err(
                "research.query_max_chars must be at least research.query_min_chars".into(),
            );
        }
        usize_count(
            "research.question_max_chars",
            self.question_max_chars,
            10,
            4000,
        )?;
        usize_count("research.url_max_chars", self.url_max_chars, 32, 8192)
    }
}

impl ReadUrlSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        usize_count("read_url.max_bytes", self.max_bytes, 1024, 16 * 1024 * 1024)?;
        usize_count("read_url.max_chars", self.max_chars, 100, 500_000)?;
        usize_count("read_url.title_chars", self.title_chars, 10, 2000)?;
        count("read_url.timeout_secs", self.timeout_secs, 1, 120)?;
        count(
            "read_url.total_timeout_secs",
            self.total_timeout_secs,
            1,
            300,
        )?;
        if self.total_timeout_secs < self.timeout_secs {
            return Err(
                "read_url.total_timeout_secs must be at least read_url.timeout_secs".into(),
            );
        }
        // AGENTS.md bounds the manual redirect chain at five hops; configuration
        // may lower it but not widen it.
        usize_count("read_url.max_redirects", self.max_redirects, 1, 5)?;
        count("read_url.cache_ttl_secs", self.cache_ttl_secs, 1, 86_400)?;
        usize_count("read_url.cache_entries", self.cache_entries, 1, 1024)?;
        usize_count("read_url.cache_chars", self.cache_chars, 1024, 1024 * 1024)?;
        usize_count(
            "read_url.max_discovered_links",
            self.max_discovered_links,
            1,
            1024,
        )?;
        usize_count("read_url.max_link_chars", self.max_link_chars, 32, 8192)?;
        usize_count(
            "read_url.footer_budget_chars",
            self.footer_budget_chars,
            0,
            100_000,
        )?;
        usize_count(
            "read_url.max_html_elements",
            self.max_html_elements,
            100,
            200_000,
        )?;
        usize_count("read_url.max_html_depth", self.max_html_depth, 8, 512)
    }
}

impl MediaSettings {
    pub(crate) fn validate(&self, telegram: &TelegramSettings) -> Result<(), String> {
        if self.max_bytes == 0 || self.max_bytes > telegram.file_limit_bytes {
            return Err("media.max_bytes must be between 1 and telegram.file_limit_bytes".into());
        }
        usize_count("media.max_files", self.max_files, 1, 50)?;
        usize_count("media.max_tiktok_photos", self.max_tiktok_photos, 1, 35)?;
        usize_count("media.user_download_limit", self.user_download_limit, 1, 64)?;
        usize_count(
            "media.concurrent_downloads",
            self.concurrent_downloads,
            1,
            16,
        )?;
        count(
            "media.download_timeout_secs",
            self.download_timeout_secs,
            1,
            3600,
        )?;
        count(
            "media.max_video_duration_secs",
            self.max_video_duration_secs,
            1,
            86_400,
        )?;
        count(
            "media.youtube_probe_timeout_secs",
            self.youtube_probe_timeout_secs,
            1,
            600,
        )?;
        count(
            "media.max_metadata_bytes",
            self.max_metadata_bytes,
            1024,
            64 * 1024 * 1024,
        )?;
        count(
            "media.mp4_mux_headroom",
            self.mp4_mux_headroom,
            0,
            1024 * 1024,
        )?;
        count(
            "media.max_twitter_json_bytes",
            self.max_twitter_json_bytes,
            1024,
            1024 * 1024,
        )?;
        count(
            "media.max_video_dimension",
            u64::from(self.max_video_dimension),
            16,
            7680,
        )?;
        count(
            "media.max_video_pixels",
            u64::from(self.max_video_pixels),
            1,
            33_177_600,
        )?;
        count(
            "media.cookie_refresh_timeout_secs",
            self.cookie_refresh_timeout_secs,
            1,
            600,
        )
    }
}

impl MemorySettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        signed("memory.ignore_secs", self.ignore_secs, 1, 2_592_000)?;
        usize_count(
            "memory.summary_max_chars",
            self.summary_max_chars,
            100,
            20_000,
        )?;
        usize_count("memory.recent_messages", self.recent_messages, 1, 1000)?;
        usize_count(
            "memory.recent_max_chars",
            self.recent_max_chars,
            100,
            1_000_000,
        )?;
        usize_count("memory.keep_newest", self.keep_newest, 1, 1000)?;
        usize_count("memory.keep_fresh", self.keep_fresh, 1, 5000)?;
        if self.keep_fresh < self.keep_newest {
            return Err("memory.keep_fresh must be at least memory.keep_newest".into());
        }
        signed(
            "memory.compact_after_secs",
            self.compact_after_secs,
            60,
            2_592_000,
        )?;
        usize_count(
            "memory.compact_batch_messages",
            self.compact_batch_messages,
            1,
            10_000,
        )?;
        usize_count(
            "memory.compact_batch_chars",
            self.compact_batch_chars,
            100,
            1_000_000,
        )?;
        usize_count(
            "memory.compact_message_chars",
            self.compact_message_chars,
            10,
            100_000,
        )?;
        usize_count("memory.compact_passes", self.compact_passes, 1, 10)?;
        count(
            "memory.compact_interval_secs",
            self.compact_interval_secs,
            1,
            86_400,
        )?;
        signed("memory.load_limit", self.load_limit, 1, 100_000)?;
        if self.embedding_model.chars().count() > 100 {
            return Err("memory.embedding_model must be at most 100 characters".into());
        }
        usize_count("memory.embedding_dim", self.embedding_dim, 0, 8192)?;
        usize_count("memory.embedding_batch", self.embedding_batch, 1, 128)?;
        usize_count(
            "memory.embedding_max_chars",
            self.embedding_max_chars,
            50,
            4000,
        )?;
        usize_count(
            "memory.embedding_max_response_bytes",
            self.embedding_max_response_bytes,
            64 * 1024,
            32 * 1024 * 1024,
        )?;
        usize_count("memory.embedding_passes", self.embedding_passes, 1, 20)?;
        count(
            "memory.embedding_interval_secs",
            self.embedding_interval_secs,
            10,
            86_400,
        )?;
        signed("memory.search_scan_rows", self.search_scan_rows, 1, 20_000)?;
        usize_count("memory.search_limit", self.search_limit, 1, 20)?;
        if !self.search_min_score.is_finite() || !(0.0..=1.0).contains(&self.search_min_score) {
            return Err("memory.search_min_score must be between 0.0 and 1.0".into());
        }
        usize_count(
            "memory.search_chunk_chars",
            self.search_chunk_chars,
            50,
            2000,
        )?;
        usize_count(
            "memory.search_max_chars",
            self.search_max_chars,
            200,
            20_000,
        )?;
        usize_count(
            "memory.search_recent_messages",
            self.search_recent_messages,
            0,
            50,
        )
    }
}

impl MediaCacheSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        signed("media_cache.ttl_seconds", self.ttl_seconds, 1, 2_592_000)?;
        signed(
            "media_cache.max_publications",
            self.max_publications,
            1,
            10_000,
        )?;
        usize_count(
            "media_cache.max_description_utf16",
            self.max_description_utf16,
            16,
            4096,
        )?;
        usize_count(
            "media_cache.max_file_id_bytes",
            self.max_file_id_bytes,
            16,
            4096,
        )
    }
}
