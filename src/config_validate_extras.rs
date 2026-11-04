use crate::config_validate::{count, non_empty, signed, usize_count};
use crate::*;

impl CodeSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        count(
            "code.max_file_bytes",
            self.max_file_bytes,
            1024,
            16 * 1024 * 1024,
        )?;
        usize_count("code.max_output_chars", self.max_output_chars, 100, 200_000)?;
        usize_count("code.max_matches", self.max_matches, 1, 1000)?;
        usize_count("code.max_line_chars", self.max_line_chars, 20, 4096)?;
        signed("code.memory_max_rows", self.memory_max_rows, 1, 1000)?;
        usize_count("code.memory_max_chars", self.memory_max_chars, 100, 20_000)?;
        usize_count("code.topic_max_chars", self.topic_max_chars, 10, 500)?;
        usize_count("code.fact_max_chars", self.fact_max_chars, 10, 2000)?;
        usize_count("code.url_max_chars", self.url_max_chars, 32, 4096)
    }
}

impl ImproveSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        usize_count("improve.goal_max_chars", self.goal_max_chars, 100, 20_000)?;
        usize_count(
            "improve.feedback_max_chars",
            self.feedback_max_chars,
            100,
            100_000,
        )?;
        usize_count(
            "improve.previous_spec_max_chars",
            self.previous_spec_max_chars,
            100,
            200_000,
        )?;
        usize_count(
            "improve.previous_review_max_chars",
            self.previous_review_max_chars,
            100,
            200_000,
        )?;
        usize_count("improve.status_max_chars", self.status_max_chars, 100, 4096)?;
        usize_count(
            "improve.result_goal_max_chars",
            self.result_goal_max_chars,
            100,
            20_000,
        )?;
        usize_count(
            "improve.result_summary_max_chars",
            self.result_summary_max_chars,
            100,
            10_000,
        )?;
        usize_count("improve.result_files", self.result_files, 1, 100)?;
        usize_count("improve.result_tasks", self.result_tasks, 1, 100)?;
        usize_count(
            "improve.result_max_chars",
            self.result_max_chars,
            100,
            100_000,
        )
    }
}

impl CookieSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        count(
            "cookies.max_jar_bytes",
            self.max_jar_bytes,
            1024,
            64 * 1024 * 1024,
        )?;
        usize_count(
            "cookies.max_header_bytes",
            self.max_header_bytes,
            256,
            65_536,
        )
    }
}

impl ImageFetchSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        usize_count(
            "image_fetch.max_pinned_clients",
            self.max_pinned_clients,
            1,
            10_000,
        )?;
        count(
            "image_fetch.pinned_client_ttl_secs",
            self.pinned_client_ttl_secs,
            1,
            86_400,
        )
    }
}

impl CleanupSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        count("cleanup.max_age_secs", self.max_age_secs, 60, 2_592_000)
    }
}

impl UsageSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        count("usage.retention_days", self.retention_days, 1, 3650)?;
        for (key, value) in [
            ("usage.input_price_per_m", self.input_price_per_m),
            (
                "usage.cached_input_price_per_m",
                self.cached_input_price_per_m,
            ),
            ("usage.output_price_per_m", self.output_price_per_m),
        ] {
            if !value.is_finite() || !(0.0..=1000.0).contains(&value) {
                return Err(format!("{key} must be between 0 and 1000"));
            }
        }
        Ok(())
    }
}

impl RetrySettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.llm.validate("retry.llm")?;
        self.read_url.validate("retry.read_url")?;
        self.search.validate("retry.search")
    }
}

impl RetryPolicy {
    pub(crate) fn validate(&self, prefix: &str) -> Result<(), String> {
        count(
            &format!("{prefix}.attempts"),
            u64::from(self.attempts),
            1,
            10,
        )?;
        count(
            &format!("{prefix}.base_backoff_ms"),
            self.base_backoff_ms,
            1,
            60_000,
        )?;
        count(
            &format!("{prefix}.max_backoff_ms"),
            self.max_backoff_ms,
            1,
            120_000,
        )?;
        if self.max_backoff_ms < self.base_backoff_ms {
            return Err(format!(
                "{prefix}.max_backoff_ms must be at least {prefix}.base_backoff_ms"
            ));
        }
        for status in &self.retry_statuses {
            count(
                &format!("{prefix}.retry_statuses"),
                u64::from(*status),
                100,
                599,
            )?;
        }
        Ok(())
    }
}

impl RouterSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        non_empty("router.jev_model", &self.jev_model)?;
        count("router.jev_timeout_ms", self.jev_timeout_ms, 50, 10_000)?;
        usize_count(
            "router.jev_max_response_bytes",
            self.jev_max_response_bytes,
            1024,
            1024 * 1024,
        )?;
        usize_count("router.jev_concurrency", self.jev_concurrency, 1, 64)?;
        if !self.jev_min_confidence.is_finite() || !(0.0..=1.0).contains(&self.jev_min_confidence) {
            return Err("router.jev_min_confidence must be between 0 and 1".into());
        }
        if self.mode == RouterMode::Jev {
            parse_http_endpoint(&self.jev_url, "router.jev_url")?;
        }
        Ok(())
    }
}
