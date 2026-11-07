use crate::*;
use std::sync::OnceLock;

const CONFIG_PATH_ENV: &str = "MAKSUT_CONFIG";
const DEFAULT_CONFIG_PATH: &str = "config.toml";

/// Everything tunable at deploy time: limits, deadlines, retries, model and
/// paths. Loaded once at startup (defaults, then `config.toml`, then the legacy
/// environment variables), validated, and read-only afterwards.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) log: LogSettings,
    pub(crate) paths: PathSettings,
    pub(crate) telegram: TelegramSettings,
    pub(crate) chat: ChatSettings,
    pub(crate) github: GithubSettings,
    pub(crate) voice: VoiceSettings,
    pub(crate) llm: LlmSettings,
    pub(crate) router: RouterSettings,
    pub(crate) search: SearchSettings,
    pub(crate) research: ResearchSettings,
    pub(crate) read_url: ReadUrlSettings,
    pub(crate) media: MediaSettings,
    pub(crate) memory: MemorySettings,
    pub(crate) media_cache: MediaCacheSettings,
    pub(crate) code: CodeSettings,
    pub(crate) improve: ImproveSettings,
    pub(crate) cookies: CookieSettings,
    pub(crate) image_fetch: ImageFetchSettings,
    pub(crate) cleanup: CleanupSettings,
    pub(crate) usage: UsageSettings,
    pub(crate) retry: RetrySettings,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The installed configuration. Startup installs the loaded file; anything
/// reading it before that (only unit tests) gets the documented defaults.
pub(crate) fn config() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

pub(crate) fn install(loaded: Config) -> Result<(), String> {
    CONFIG
        .set(loaded)
        .map_err(|_| "configuration was installed twice".to_string())
}

impl Config {
    /// Defaults, then the config file, then the legacy environment variables.
    pub(crate) async fn load() -> Result<Self, String> {
        // A missing implicit file means "run on defaults"; a missing explicit
        // path is an operator error and must not degrade silently.
        let explicit = env::var(CONFIG_PATH_ENV)
            .ok()
            .filter(|raw| !raw.trim().is_empty());
        let path = explicit
            .as_deref()
            .map_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH), PathBuf::from);
        let mut loaded = match tokio::fs::read_to_string(&path).await {
            Ok(text) => {
                Self::parse(&text).map_err(|reason| format!("{}: {reason}", path.display()))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {
                Self::default()
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        loaded.apply_env()?;
        loaded.validate()?;
        Ok(loaded)
    }

    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        toml::from_str::<Self>(text).map_err(|error| format!("invalid TOML: {error}"))
    }

    fn apply_env(&mut self) -> Result<(), String> {
        if let Some(value) = env_value("DATABASE_URL") {
            self.paths.database_url = value;
        }
        if let Some(value) = env_value("DOWNLOAD_DIR") {
            self.paths.download_dir = value;
        }
        if let Some(value) = env_value("YTDLP_BIN") {
            self.paths.ytdlp_bin = value;
        }
        if let Some(value) = env_value("PYTHON_BIN") {
            self.paths.python_bin = value;
        }
        if let Some(value) = env_value("TWITTER_HELPER") {
            self.paths.twitter_helper = value;
        }
        if let Some(value) = env_value("CODE_DIR") {
            self.paths.code_dir = value;
        }
        if let Some(value) = env_value("SELF_IMPROVE_DIR") {
            self.paths.self_improve_dir = value;
        }
        if let Some(value) = env_value("COOKIES_FILE") {
            self.paths.cookies_file = Some(value);
        }
        if let Some(value) = env_value("COOKIE_REFRESH_URL") {
            self.paths.cookie_refresh_url = Some(value);
        }
        if let Some(value) = env_value("STICKER_SETS_FILE") {
            self.paths.sticker_sets_file = value;
        }
        if let Some(value) = env_value("STICKER_DESCRIPTIONS_FILE") {
            self.paths.sticker_descriptions_file = value;
        }
        if let Some(value) = env_value("LLM_BASE_URL") {
            self.llm.base_url = value;
        }
        if let Some(value) = env_value("LLM_MODEL") {
            self.llm.model = value;
        }
        if let Some(value) = env_value("LLM_REASONING_EFFORT") {
            self.llm.reasoning_effort = Some(value);
        }
        if let Some(value) = env_value("LLM_SEARCH_URL") {
            self.search.url = Some(value);
        }
        if let Some(value) = env_value("QDRANT_URL") {
            self.memory.qdrant_url = value;
        }
        if let Some(value) = env_value("EMBEDDING_URL") {
            self.memory.embedding_url = value;
        }
        if let Some(value) = env_value("EMBEDDING_MODEL") {
            self.memory.embedding_model = value;
        }
        if let Some(value) = env_value("EMBEDDING_DIM") {
            self.memory.embedding_dim = value
                .parse()
                .map_err(|_| "EMBEDDING_DIM must be a non-negative integer".to_string())?;
        }
        if let Some(value) = env_value("SEARCH_PROVIDER") {
            self.search.provider = SearchProvider::from_name(&value).ok_or_else(|| {
                format!(
                    "SEARCH_PROVIDER must be one of {}",
                    SearchProvider::names().join(", ")
                )
            })?;
        }
        if let Some(value) = env_value("MAX_MEDIA_BYTES") {
            self.media.max_bytes = value
                .parse()
                .map_err(|_| "MAX_MEDIA_BYTES must be an integer in bytes".to_string())?;
        }
        Ok(())
    }
}

fn env_value(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

pub(crate) fn parse_http_endpoint(raw: &str, name: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| format!("{name} must be a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "{name} must be an http(s) URL without credentials, query, or fragment"
        ));
    }
    Ok(url)
}
