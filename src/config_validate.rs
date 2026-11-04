use crate::*;

const LOG_LEVELS: [&str; 6] = ["trace", "debug", "info", "warn", "error", "off"];

impl Config {
    /// Rejects values that would silently disable a bound: zero timeouts, zero
    /// retries, unbounded caches, or caps that exceed a hard protocol limit.
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.log.validate()?;
        self.paths.validate()?;
        self.telegram.validate()?;
        self.chat.validate()?;
        self.llm.validate()?;
        self.router.validate()?;
        self.search.validate()?;
        self.research.validate()?;
        self.read_url.validate()?;
        self.media.validate(&self.telegram)?;
        self.memory.validate()?;
        self.media_cache.validate()?;
        self.code.validate()?;
        self.improve.validate()?;
        self.cookies.validate()?;
        self.image_fetch.validate()?;
        self.cleanup.validate()?;
        self.retry.validate()
    }
}

pub(crate) fn count(key: &str, value: u64, min: u64, max: u64) -> Result<(), String> {
    if value < min || value > max {
        return Err(format!("{key} must be between {min} and {max}"));
    }
    Ok(())
}

pub(crate) fn usize_count(key: &str, value: usize, min: usize, max: usize) -> Result<(), String> {
    count(key, value as u64, min as u64, max as u64)
}

pub(crate) fn signed(key: &str, value: i64, min: i64, max: i64) -> Result<(), String> {
    if value < min || value > max {
        return Err(format!("{key} must be between {min} and {max}"));
    }
    Ok(())
}

pub(crate) fn non_empty(key: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{key} must not be empty"));
    }
    Ok(())
}

impl LogSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !LOG_LEVELS.contains(&self.level.as_str()) {
            return Err(format!(
                "log.level must be one of {}",
                LOG_LEVELS.join(", ")
            ));
        }
        Ok(())
    }
}

impl PathSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for (key, value) in [
            ("paths.database_url", &self.database_url),
            ("paths.download_dir", &self.download_dir),
            ("paths.ytdlp_bin", &self.ytdlp_bin),
            ("paths.python_bin", &self.python_bin),
            ("paths.twitter_helper", &self.twitter_helper),
            ("paths.code_dir", &self.code_dir),
            ("paths.self_improve_dir", &self.self_improve_dir),
            ("paths.sticker_sets_file", &self.sticker_sets_file),
            (
                "paths.sticker_descriptions_file",
                &self.sticker_descriptions_file,
            ),
        ] {
            non_empty(key, value)?;
        }
        Ok(())
    }
}

impl TelegramSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        count(
            "telegram.connect_timeout_secs",
            self.connect_timeout_secs,
            1,
            60,
        )?;
        count(
            "telegram.request_timeout_secs",
            self.request_timeout_secs,
            1,
            3600,
        )?;
        count(
            "telegram.pool_idle_timeout_secs",
            self.pool_idle_timeout_secs,
            1,
            3600,
        )?;
        count(
            "telegram.tcp_keepalive_secs",
            self.tcp_keepalive_secs,
            1,
            3600,
        )?;
        // Telegram's own upload ceiling; a larger value would fail every send.
        count(
            "telegram.file_limit_bytes",
            self.file_limit_bytes,
            1,
            50_000_000,
        )
    }
}

impl ChatSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        usize_count("chat.tool_rounds", self.tool_rounds, 1, 64)?;
        count("chat.max_tokens", u64::from(self.max_tokens), 1, 32_768)?;
        count(
            "chat.status_max_tokens",
            u64::from(self.status_max_tokens),
            1,
            32_768,
        )?;
        count(
            "chat.research_max_tokens",
            u64::from(self.research_max_tokens),
            1,
            32_768,
        )?;
        count(
            "chat.typing_keepalive_secs",
            self.typing_keepalive_secs,
            1,
            60,
        )?;
        count("chat.cooldown_ms", self.cooldown_ms, 0, 60_000)?;
        count(
            "chat.followup_window_secs",
            self.followup_window_secs,
            1,
            86_400,
        )?;
        usize_count("chat.reply_max_chars", self.reply_max_chars, 100, 4096)?;
        usize_count("chat.rich_max_chars", self.rich_max_chars, 100, 32_768)?;
        usize_count(
            "chat.research_body_chars",
            self.research_body_chars,
            100,
            100_000,
        )?;
        usize_count(
            "chat.research_sources_chars",
            self.research_sources_chars,
            100,
            100_000,
        )?;
        usize_count(
            "chat.research_max_chars",
            self.research_max_chars,
            100,
            100_000,
        )?;
        if self.research_max_chars < self.research_body_chars {
            return Err("chat.research_max_chars must be at least chat.research_body_chars".into());
        }
        usize_count("chat.concurrency", self.concurrency, 1, 64)?;
        usize_count("chat.max_active_chats", self.max_active_chats, 1, 1_000_000)?;
        usize_count("chat.max_pace_entries", self.max_pace_entries, 1, 1_000_000)?;
        usize_count("chat.max_chat_locks", self.max_chat_locks, 1, 1_000_000)?;
        usize_count(
            "chat.max_user_rate_limits",
            self.max_user_rate_limits,
            1,
            1_000_000,
        )?;
        count(
            "chat.discovered_url_ttl_secs",
            self.discovered_url_ttl_secs,
            1,
            86_400,
        )?;
        usize_count(
            "chat.discovered_urls_per_chat",
            self.discovered_urls_per_chat,
            1,
            1024,
        )?;
        count(
            "chat.profile_cache_ttl_secs",
            self.profile_cache_ttl_secs,
            1,
            86_400,
        )?;
        usize_count(
            "chat.profile_cache_max_entries",
            self.profile_cache_max_entries,
            1,
            65_536,
        )?;
        count("chat.image_max_bytes", self.image_max_bytes, 1, 50_000_000)?;
        usize_count("chat.image_concurrency", self.image_concurrency, 1, 64)?;
        count("chat.image_timeout_secs", self.image_timeout_secs, 1, 600)?;
        for (key, value) in [
            (
                "chat.read_url_calls_per_reply",
                self.read_url_calls_per_reply,
            ),
            (
                "chat.inspect_code_calls_per_reply",
                self.inspect_code_calls_per_reply,
            ),
            (
                "chat.chat_profile_calls_per_reply",
                self.chat_profile_calls_per_reply,
            ),
            (
                "chat.remember_self_calls_per_reply",
                self.remember_self_calls_per_reply,
            ),
            ("chat.search_calls_per_reply", self.search_calls_per_reply),
            (
                "chat.research_calls_per_reply",
                self.research_calls_per_reply,
            ),
            (
                "chat.chat_search_calls_per_reply",
                self.chat_search_calls_per_reply,
            ),
        ] {
            usize_count(key, value, 1, 64)?;
        }
        Ok(())
    }
}

impl LlmSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        non_empty("llm.base_url", &self.base_url)?;
        non_empty("llm.model", &self.model)?;
        count(
            "llm.request_timeout_secs",
            self.request_timeout_secs,
            1,
            600,
        )?;
        usize_count(
            "llm.max_response_bytes",
            self.max_response_bytes,
            1024,
            4 * 1024 * 1024,
        )
    }
}

impl SearchSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(url) = &self.url {
            non_empty("search.url", url)?;
        }
        count("search.http_timeout_secs", self.http_timeout_secs, 1, 300)?;
        usize_count("search.max_bytes", self.max_bytes, 1024, 16 * 1024 * 1024)?;
        usize_count("search.result_chars", self.result_chars, 100, 100_000)?;
        count("search.auto_deadline_secs", self.auto_deadline_secs, 1, 300)?;
        count(
            "search.public_soft_deadline_secs",
            self.public_soft_deadline_secs,
            1,
            300,
        )?;
        count(
            "search.public_hard_deadline_secs",
            self.public_hard_deadline_secs,
            1,
            300,
        )?;
        if self.public_soft_deadline_secs > self.public_hard_deadline_secs {
            return Err(
                "search.public_soft_deadline_secs must not exceed search.public_hard_deadline_secs"
                    .into(),
            );
        }
        if self.public_hard_deadline_secs > self.auto_deadline_secs {
            return Err(
                "search.public_hard_deadline_secs must not exceed search.auto_deadline_secs".into(),
            );
        }
        Ok(())
    }
}
