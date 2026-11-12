use crate::*;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LogSettings {
    /// trace, debug, info, warn, error or off.
    pub(crate) level: String,
    /// Newest formatted lines kept in the in-process ring behind `read_logs`.
    pub(crate) ring_lines: usize,
    /// Hard byte cap for that ring; a few multi-line events can exhaust it.
    pub(crate) ring_max_bytes: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PathSettings {
    pub(crate) database_url: String,
    pub(crate) download_dir: String,
    pub(crate) ytdlp_bin: String,
    pub(crate) python_bin: String,
    pub(crate) twitter_helper: String,
    pub(crate) code_dir: String,
    pub(crate) self_improve_dir: String,
    pub(crate) cookies_file: Option<String>,
    pub(crate) cookie_refresh_url: Option<String>,
    pub(crate) sticker_sets_file: String,
    pub(crate) sticker_descriptions_file: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct TelegramSettings {
    pub(crate) connect_timeout_secs: u64,
    pub(crate) request_timeout_secs: u64,
    pub(crate) pool_idle_timeout_secs: u64,
    pub(crate) tcp_keepalive_secs: u64,
    /// Telegram rejects uploads above 50 MB; `media.max_bytes` must fit under it.
    pub(crate) file_limit_bytes: u64,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ChatSettings {
    pub(crate) tool_rounds: usize,
    pub(crate) max_tokens: u32,
    pub(crate) status_max_tokens: u32,
    pub(crate) research_max_tokens: u32,
    pub(crate) feature_title_max_tokens: u32,
    pub(crate) typing_keepalive_secs: u64,
    pub(crate) cooldown_ms: u64,
    pub(crate) followup_window_secs: u64,
    pub(crate) reply_max_chars: usize,
    pub(crate) rich_messages: bool,
    pub(crate) rich_max_chars: usize,
    pub(crate) research_body_chars: usize,
    pub(crate) research_sources_chars: usize,
    pub(crate) research_max_chars: usize,
    pub(crate) concurrency: usize,
    pub(crate) max_active_chats: usize,
    pub(crate) max_pace_entries: usize,
    pub(crate) max_chat_locks: usize,
    pub(crate) max_user_rate_limits: usize,
    pub(crate) discovered_url_ttl_secs: u64,
    pub(crate) discovered_urls_per_chat: usize,
    pub(crate) profile_cache_ttl_secs: u64,
    pub(crate) profile_cache_max_entries: usize,
    pub(crate) image_max_bytes: u64,
    pub(crate) image_concurrency: usize,
    pub(crate) image_timeout_secs: u64,
    pub(crate) read_url_calls_per_reply: usize,
    pub(crate) inspect_code_calls_per_reply: usize,
    pub(crate) chat_profile_calls_per_reply: usize,
    pub(crate) read_logs_calls_per_reply: usize,
    pub(crate) remember_self_calls_per_reply: usize,
    pub(crate) search_calls_per_reply: usize,
    pub(crate) research_calls_per_reply: usize,
    pub(crate) chat_search_calls_per_reply: usize,
    pub(crate) usage_stats_calls_per_reply: usize,
    pub(crate) research_status_calls_per_reply: usize,
    pub(crate) research_cancel_calls_per_reply: usize,
    pub(crate) github_calls_per_reply: usize,
    pub(crate) github_comment_calls_per_reply: usize,
}

/// The GitHub tools talk to exactly one repository. `repo` and `api_base` are
/// operator settings, never model input: a model-chosen `owner/repo` would turn
/// the tool into an arbitrary outbound request and chat-memory exfiltration
/// channel.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GithubSettings {
    pub(crate) repo: String,
    pub(crate) api_base: String,
    pub(crate) timeout_secs: u64,
    pub(crate) max_response_bytes: usize,
    pub(crate) cache_ttl_secs: u64,
}

/// Speech-to-text for voice and audio attachments. `enabled` and a non-empty
/// `model` are both required before any download or model call happens.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct VoiceSettings {
    pub(crate) enabled: bool,
    pub(crate) model: String,
    pub(crate) timeout_secs: u64,
    pub(crate) max_bytes: u64,
    pub(crate) max_duration_secs: u64,
    pub(crate) max_transcript_chars: usize,
    pub(crate) concurrency: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LlmSettings {
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) request_timeout_secs: u64,
    pub(crate) max_response_bytes: usize,
}

/// Where the pre-model reply decision comes from. `off` keeps the historical
/// behavior, and `jev` asks the TypeSafe System One API with a deterministic
/// fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RouterMode {
    #[default]
    Off,
    Jev,
}

impl RouterMode {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "off" => Self::Off,
            "jev" => Self::Jev,
            _ => return None,
        })
    }

    pub(crate) fn names() -> &'static [&'static str] {
        &["off", "jev"]
    }
}

impl<'de> serde::Deserialize<'de> for RouterMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::from_name(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!("must be one of {}", Self::names().join(", ")))
        })
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RouterSettings {
    pub(crate) mode: RouterMode,
    pub(crate) jev_url: String,
    pub(crate) jev_model: String,
    pub(crate) jev_timeout_ms: u64,
    pub(crate) jev_max_response_bytes: usize,
    /// Concurrent jev requests; excess messages fall back to the rules table
    /// instead of queueing in front of a 400 ms call.
    pub(crate) jev_concurrency: usize,
    /// Minimum probability of the selected choice; below it the rules
    /// decision wins instead of a shaky model answer.
    pub(crate) jev_min_confidence: f64,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SearchSettings {
    pub(crate) provider: SearchProvider,
    /// Overrides the provider default endpoint when set.
    pub(crate) url: Option<String>,
    pub(crate) http_timeout_secs: u64,
    pub(crate) max_bytes: usize,
    pub(crate) result_chars: usize,
    pub(crate) auto_deadline_secs: u64,
    pub(crate) public_soft_deadline_secs: u64,
    pub(crate) public_hard_deadline_secs: u64,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ResearchSettings {
    pub(crate) cooldown_secs: u64,
    pub(crate) min_queries: usize,
    pub(crate) max_queries: usize,
    pub(crate) results_per_query: usize,
    pub(crate) max_sources: usize,
    pub(crate) title_chars: usize,
    pub(crate) snippet_chars: usize,
    pub(crate) dossier_chars: usize,
    pub(crate) timeout_secs: u64,
    pub(crate) query_min_chars: usize,
    pub(crate) query_max_chars: usize,
    pub(crate) question_max_chars: usize,
    pub(crate) url_max_chars: usize,
    pub(crate) max_tracked: usize,
    pub(crate) tracked_ttl_secs: u64,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ReadUrlSettings {
    pub(crate) max_bytes: usize,
    pub(crate) max_chars: usize,
    pub(crate) title_chars: usize,
    pub(crate) timeout_secs: u64,
    pub(crate) total_timeout_secs: u64,
    pub(crate) max_redirects: usize,
    pub(crate) cache_ttl_secs: u64,
    pub(crate) cache_entries: usize,
    pub(crate) cache_chars: usize,
    pub(crate) max_discovered_links: usize,
    pub(crate) max_link_chars: usize,
    pub(crate) footer_budget_chars: usize,
    pub(crate) max_html_elements: usize,
    pub(crate) max_html_depth: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct MediaSettings {
    pub(crate) max_bytes: u64,
    pub(crate) max_files: usize,
    pub(crate) max_tiktok_photos: usize,
    pub(crate) user_download_limit: usize,
    pub(crate) concurrent_downloads: usize,
    pub(crate) download_timeout_secs: u64,
    pub(crate) max_video_duration_secs: u64,
    pub(crate) youtube_probe_timeout_secs: u64,
    pub(crate) max_metadata_bytes: u64,
    pub(crate) mp4_mux_headroom: u64,
    pub(crate) max_twitter_json_bytes: u64,
    pub(crate) max_video_dimension: u32,
    pub(crate) max_video_pixels: u32,
    pub(crate) cookie_refresh_timeout_secs: u64,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct MemorySettings {
    pub(crate) ignore_secs: i64,
    pub(crate) summary_max_chars: usize,
    pub(crate) recent_messages: usize,
    pub(crate) recent_max_chars: usize,
    pub(crate) keep_newest: usize,
    pub(crate) keep_fresh: usize,
    pub(crate) compact_after_secs: i64,
    pub(crate) compact_batch_messages: usize,
    pub(crate) compact_batch_chars: usize,
    pub(crate) compact_message_chars: usize,
    pub(crate) compact_passes: usize,
    pub(crate) compact_interval_secs: u64,
    pub(crate) load_limit: i64,
    pub(crate) embedding_url: String,
    pub(crate) embedding_model: String,
    pub(crate) embedding_dim: usize,
    pub(crate) embedding_batch: usize,
    pub(crate) embedding_max_chars: usize,
    pub(crate) embedding_max_response_bytes: usize,
    pub(crate) embedding_passes: usize,
    pub(crate) embedding_interval_secs: u64,
    pub(crate) search_scan_rows: i64,
    pub(crate) search_limit: usize,
    pub(crate) search_min_score: f32,
    pub(crate) search_chunk_chars: usize,
    pub(crate) search_max_chars: usize,
    pub(crate) search_recent_messages: usize,
    pub(crate) qdrant_url: String,
    pub(crate) qdrant_collection: String,
    pub(crate) qdrant_timeout_secs: u64,
    pub(crate) qdrant_max_response_bytes: usize,
    pub(crate) self_fact_limit: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct MediaCacheSettings {
    pub(crate) ttl_seconds: i64,
    pub(crate) max_publications: i64,
    pub(crate) max_description_utf16: usize,
    pub(crate) max_file_id_bytes: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CodeSettings {
    pub(crate) max_file_bytes: u64,
    pub(crate) max_output_chars: usize,
    pub(crate) max_matches: usize,
    pub(crate) max_line_chars: usize,
    pub(crate) memory_max_rows: i64,
    pub(crate) memory_max_chars: usize,
    pub(crate) topic_max_chars: usize,
    pub(crate) fact_max_chars: usize,
    pub(crate) url_max_chars: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ImproveSettings {
    pub(crate) goal_max_chars: usize,
    pub(crate) feedback_max_chars: usize,
    pub(crate) previous_spec_max_chars: usize,
    pub(crate) previous_review_max_chars: usize,
    pub(crate) status_max_chars: usize,
    pub(crate) result_goal_max_chars: usize,
    pub(crate) result_summary_max_chars: usize,
    pub(crate) result_files: usize,
    pub(crate) result_tasks: usize,
    pub(crate) result_max_chars: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CookieSettings {
    pub(crate) max_jar_bytes: u64,
    pub(crate) max_header_bytes: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ImageFetchSettings {
    pub(crate) max_pinned_clients: usize,
    pub(crate) pinned_client_ttl_secs: u64,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CleanupSettings {
    pub(crate) max_age_secs: u64,
}

/// LLM spend accounting. Prices are USD per 1M tokens and turn reported token
/// counts into `llm_usage.cost_microdollars`; zero disables the money column.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UsageSettings {
    pub(crate) retention_days: u64,
    pub(crate) input_price_per_m: f64,
    pub(crate) cached_input_price_per_m: f64,
    pub(crate) output_price_per_m: f64,
}

/// HTTP surface for the Telegram Mini App. The process serves plain HTTP on
/// `listen`; TLS terminates at the host reverse proxy, and `public_url` is the
/// https address Telegram opens through the chat button. `enabled = false`
/// keeps the listener closed, which is the default.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct WebSettings {
    pub(crate) enabled: bool,
    pub(crate) listen: String,
    pub(crate) public_url: Option<String>,
    /// Rejects an `initData` older than this, so a captured payload expires.
    pub(crate) auth_max_age_secs: u64,
    /// Cap for a JSON request body; every endpoint takes one small object.
    pub(crate) max_body_bytes: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetrySettings {
    #[serde(default = "RetryPolicy::llm")]
    pub(crate) llm: RetryPolicy,
    #[serde(default = "RetryPolicy::read_url")]
    pub(crate) read_url: RetryPolicy,
    #[serde(default = "RetryPolicy::search")]
    pub(crate) search: RetryPolicy,
}

macro_rules! defaults {
    ($ty:ty { $($field:ident: $value:expr),* $(,)? }) => {
        impl Default for $ty {
            fn default() -> Self {
                Self { $($field: $value),* }
            }
        }
    };
}

defaults!(LogSettings {
    level: "info".into(),
    ring_lines: 500,
    ring_max_bytes: 262_144
});

defaults!(PathSettings {
    database_url: "sqlite:///data/maksut.db".into(),
    download_dir: "/tmp/maksut".into(),
    ytdlp_bin: "yt-dlp".into(),
    python_bin: "/opt/yt-dlp/bin/python".into(),
    twitter_helper: "/opt/maksut/twitter_images.py".into(),
    code_dir: "/var/lib/maksut/code".into(),
    self_improve_dir: "/var/lib/maksut/self-improve".into(),
    cookies_file: None,
    cookie_refresh_url: None,
    sticker_sets_file: "sticker_sets.txt".into(),
    sticker_descriptions_file: "sticker_descriptions.json".into(),
});

defaults!(TelegramSettings {
    connect_timeout_secs: 5,
    request_timeout_secs: 180,
    pool_idle_timeout_secs: 15,
    tcp_keepalive_secs: 15,
    file_limit_bytes: 50_000_000,
});

defaults!(ChatSettings {
    tool_rounds: 8,
    max_tokens: 4000,
    status_max_tokens: 1500,
    research_max_tokens: 4000,
    feature_title_max_tokens: 500,
    typing_keepalive_secs: 4,
    cooldown_ms: 3000,
    followup_window_secs: 180,
    reply_max_chars: 3900,
    rich_messages: true,
    rich_max_chars: 16000,
    research_body_chars: 4800,
    research_sources_chars: 1200,
    research_max_chars: 6000,
    concurrency: 4,
    max_active_chats: 1024,
    max_pace_entries: 1024,
    max_chat_locks: 1024,
    max_user_rate_limits: 2048,
    discovered_url_ttl_secs: 1800,
    discovered_urls_per_chat: 32,
    profile_cache_ttl_secs: 300,
    profile_cache_max_entries: 1024,
    image_max_bytes: 8 * 1024 * 1024,
    image_concurrency: 4,
    image_timeout_secs: 20,
    read_url_calls_per_reply: 5,
    inspect_code_calls_per_reply: 5,
    chat_profile_calls_per_reply: 3,
    read_logs_calls_per_reply: 3,
    remember_self_calls_per_reply: 3,
    search_calls_per_reply: 6,
    research_calls_per_reply: 1,
    chat_search_calls_per_reply: 3,
    usage_stats_calls_per_reply: 3,
    research_status_calls_per_reply: 3,
    research_cancel_calls_per_reply: 2,
    github_calls_per_reply: 4,
    github_comment_calls_per_reply: 1,
});

defaults!(GithubSettings {
    repo: "labi-le/maksut".into(),
    api_base: "https://api.github.com".into(),
    timeout_secs: 15,
    max_response_bytes: 512 * 1024,
    cache_ttl_secs: 60,
});

defaults!(VoiceSettings {
    enabled: true,
    model: "gemini-2.5-flash".into(),
    timeout_secs: 25,
    max_bytes: 4 * 1024 * 1024,
    max_duration_secs: 300,
    max_transcript_chars: 2000,
    concurrency: 2,
});

defaults!(LlmSettings {
    base_url: "https://llm.labile.cc/v1".into(),
    model: "deepseek-v4.1-flash".into(),
    reasoning_effort: Some("low".into()),
    request_timeout_secs: 40,
    max_response_bytes: 64 * 1024,
});

defaults!(RouterSettings {
    mode: RouterMode::Off,
    jev_url: "https://api.typesafe.ai/v1/systemone".into(),
    jev_model: "jev-latest".into(),
    jev_timeout_ms: 800,
    jev_max_response_bytes: 16 * 1024,
    jev_concurrency: 8,
    jev_min_confidence: 0.6,
});

defaults!(SearchSettings {
    provider: SearchProvider::Auto,
    url: None,
    http_timeout_secs: 10,
    max_bytes: 512 * 1024,
    result_chars: 2500,
    auto_deadline_secs: 15,
    public_soft_deadline_secs: 5,
    public_hard_deadline_secs: 12,
});

defaults!(ResearchSettings {
    cooldown_secs: 20,
    min_queries: 2,
    max_queries: 4,
    results_per_query: 4,
    max_sources: 8,
    title_chars: 160,
    snippet_chars: 300,
    dossier_chars: 5000,
    timeout_secs: 15,
    query_min_chars: 3,
    query_max_chars: 200,
    question_max_chars: 500,
    url_max_chars: 2048,
    max_tracked: 256,
    tracked_ttl_secs: 3600,
});

defaults!(ReadUrlSettings {
    max_bytes: 2 * 1024 * 1024,
    max_chars: 24_000,
    title_chars: 200,
    timeout_secs: 15,
    total_timeout_secs: 20,
    max_redirects: 5,
    cache_ttl_secs: 600,
    cache_entries: 16,
    cache_chars: 64 * 1024,
    max_discovered_links: 32,
    max_link_chars: 500,
    footer_budget_chars: 4000,
    max_html_elements: 20_000,
    max_html_depth: 128,
});

defaults!(MediaSettings {
    max_bytes: 45 * 1024 * 1024,
    max_files: 10,
    max_tiktok_photos: 35,
    user_download_limit: 3,
    concurrent_downloads: 2,
    download_timeout_secs: 180,
    max_video_duration_secs: 30 * 60,
    youtube_probe_timeout_secs: 25,
    max_metadata_bytes: 4 * 1024 * 1024,
    mp4_mux_headroom: 64 * 1024,
    max_twitter_json_bytes: 16 * 1024,
    max_video_dimension: 3840,
    max_video_pixels: 3840 * 2160,
    cookie_refresh_timeout_secs: 30,
});

defaults!(MemorySettings {
    ignore_secs: 24 * 60 * 60,
    summary_max_chars: 1500,
    recent_messages: 60,
    recent_max_chars: 6_000,
    keep_newest: 5,
    keep_fresh: 40,
    compact_after_secs: 12 * 60 * 60,
    compact_batch_messages: 200,
    compact_batch_chars: 12_000,
    compact_message_chars: 500,
    compact_passes: 3,
    compact_interval_secs: 10 * 60,
    load_limit: 2000,
    embedding_url: String::new(),
    embedding_model: "intfloat/multilingual-e5-small".into(),
    embedding_dim: 384,
    embedding_batch: 32,
    embedding_max_chars: 500,
    embedding_max_response_bytes: 4 * 1024 * 1024,
    embedding_passes: 3,
    embedding_interval_secs: 120,
    search_scan_rows: 1000,
    search_limit: 5,
    search_min_score: 0.25,
    search_chunk_chars: 400,
    search_max_chars: 4000,
    search_recent_messages: 5,
    qdrant_url: String::new(),
    qdrant_collection: "maksut_memory".into(),
    qdrant_timeout_secs: 5,
    qdrant_max_response_bytes: 1024 * 1024,
    self_fact_limit: 10,
});

defaults!(MediaCacheSettings {
    ttl_seconds: 24 * 60 * 60,
    max_publications: 200,
    max_description_utf16: 1024,
    max_file_id_bytes: 1024,
});

defaults!(CodeSettings {
    max_file_bytes: 256 * 1024,
    max_output_chars: 8000,
    max_matches: 40,
    max_line_chars: 300,
    memory_max_rows: 50,
    memory_max_chars: 1200,
    topic_max_chars: 80,
    fact_max_chars: 300,
    url_max_chars: 300,
});

defaults!(ImproveSettings {
    goal_max_chars: 2000,
    feedback_max_chars: 4000,
    previous_spec_max_chars: 16_000,
    previous_review_max_chars: 8000,
    status_max_chars: 3200,
    result_goal_max_chars: 2000,
    result_summary_max_chars: 500,
    result_files: 20,
    result_tasks: 20,
    result_max_chars: 6000,
});

defaults!(CookieSettings {
    max_jar_bytes: 4 * 1024 * 1024,
    max_header_bytes: 8192,
});

defaults!(ImageFetchSettings {
    max_pinned_clients: 256,
    pinned_client_ttl_secs: 3600,
});

defaults!(CleanupSettings {
    max_age_secs: 24 * 60 * 60
});

defaults!(UsageSettings {
    retention_days: 90,
    input_price_per_m: 0.27,
    cached_input_price_per_m: 0.07,
    output_price_per_m: 1.10,
});

defaults!(WebSettings {
    enabled: false,
    listen: "0.0.0.0:8090".into(),
    public_url: None,
    auth_max_age_secs: 24 * 60 * 60,
    max_body_bytes: 16 * 1024,
});

defaults!(RetrySettings {
    llm: RetryPolicy::llm(),
    read_url: RetryPolicy::read_url(),
    search: RetryPolicy::search(),
});
