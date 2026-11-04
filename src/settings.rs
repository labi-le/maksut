use crate::*;

#[derive(Clone)]
pub(crate) struct Settings {
    pub(crate) owner_id: UserId,
    pub(crate) bot_username: String,
    pub(crate) database: SqlitePool,
    pub(crate) download_dir: PathBuf,
    pub(crate) ytdlp_bin: PathBuf,
    pub(crate) python_bin: PathBuf,
    pub(crate) twitter_helper: PathBuf,
    pub(crate) cookies_file: Option<PathBuf>,
    pub(crate) cookie_refresh_url: Option<Url>,
    pub(crate) chat: Option<ChatConfig>,
    pub(crate) router: Router,
    pub(crate) chat_locks: Arc<tokio::sync::Mutex<HashMap<i64, Arc<tokio::sync::RwLock<()>>>>>,
    pub(crate) self_improve_dir: PathBuf,
    pub(crate) sticker_replies: StickerReplies,
    pub(crate) sticker_catalog: Arc<RwLock<crate::stickers::StickerCatalog>>,
    pub(crate) max_media_bytes: u64,
    pub(crate) downloads: Arc<Semaphore>,
    pub(crate) user_rate_limits: Arc<RwLock<HashMap<UserId, Arc<Semaphore>>>>,
}

impl Settings {
    /// Loads and installs the configuration before anything else reads it.
    pub(crate) async fn load() -> Result<Self, String> {
        let loaded = Config::load().await?;
        install(loaded)?;
        crate::logging::init()?;
        Self::from_config().await
    }

    async fn from_config() -> Result<Self, String> {
        let cfg = config();
        let owner_id = env::var("OWNER_ID")
            .map_err(|_| "OWNER_ID is required".to_string())?
            .parse::<u64>()
            .map_err(|_| "OWNER_ID must be a Telegram user ID".to_string())?;
        let database = open_database(&cfg.paths.database_url)
            .await
            .map_err(|_| "Unable to open the SQLite database".to_string())?;
        let download_dir = PathBuf::from(&cfg.paths.download_dir);
        tokio::fs::create_dir_all(&download_dir)
            .await
            .map_err(|_| "Unable to create DOWNLOAD_DIR".to_string())?;
        let cookies_file = cfg.paths.cookies_file.as_deref().map(PathBuf::from);
        if let Some(path) = &cookies_file {
            check_cookies(path).await?;
        }
        let chat = match env::var("LLM_API_KEY") {
            Ok(key) if !key.trim().is_empty() => {
                let base = parse_http_endpoint(&cfg.llm.base_url, "llm.base_url")?;
                let tavily_key = env::var("TAVILY_API_KEY")
                    .ok()
                    .filter(|value| !value.trim().is_empty());
                let search = cfg
                    .search
                    .url
                    .clone()
                    .unwrap_or_else(|| search_default(cfg.search.provider).to_string());
                let search = parse_http_endpoint(&search, "search.url")?;
                Some(ChatConfig::new(
                    base,
                    key,
                    cfg.llm.model.clone(),
                    search,
                    cfg.search.provider,
                    tavily_key,
                ))
            }
            _ => None,
        };
        let router = Router::from_config(&cfg.router)?;
        let cookie_refresh_url = cfg
            .paths
            .cookie_refresh_url
            .as_deref()
            .map(|value| parse_http_endpoint(value, "cookie_refresh_url"))
            .transpose()?;
        let database_file = cfg
            .paths
            .database_url
            .split('?')
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        let database_file = Path::new(database_file).file_name();
        let settings = Self {
            owner_id: UserId(owner_id),
            bot_username: String::new(),
            database,
            download_dir,
            ytdlp_bin: PathBuf::from(&cfg.paths.ytdlp_bin),
            python_bin: PathBuf::from(&cfg.paths.python_bin),
            twitter_helper: PathBuf::from(&cfg.paths.twitter_helper),
            cookies_file,
            cookie_refresh_url,
            chat,
            router,
            chat_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            self_improve_dir: PathBuf::from(&cfg.paths.self_improve_dir),
            sticker_replies: StickerReplies::default(),
            sticker_catalog: Arc::new(RwLock::new(crate::stickers::StickerCatalog::default())),
            max_media_bytes: cfg.media.max_bytes,
            downloads: Arc::new(Semaphore::new(cfg.media.concurrent_downloads)),
            user_rate_limits: Arc::new(RwLock::new(HashMap::new())),
        };
        tracing::info!(
            database_file = ?database_file,
            download_dir = %settings.download_dir.display(),
            chat_enabled = settings.chat.is_some(),
            cookies = settings.cookies_file.is_some(),
            cookie_refresh = settings.cookie_refresh_url.is_some(),
            "settings loaded"
        );
        Ok(settings)
    }
}

fn search_default(provider: SearchProvider) -> &'static str {
    match provider {
        SearchProvider::Auto | SearchProvider::Public | SearchProvider::Firecrawl => {
            crate::search::DEFAULT_FIRECRAWL_URL
        }
        SearchProvider::Parallel => crate::search::DEFAULT_PARALLEL_MCP_URL,
        SearchProvider::Exa => crate::search::DEFAULT_EXA_MCP_URL,
        SearchProvider::Tavily => "https://api.tavily.com/search",
        SearchProvider::DuckDuckGo => crate::search::DEFAULT_SEARCH_URL,
        SearchProvider::Startpage => crate::search::DEFAULT_STARTPAGE_URL,
        SearchProvider::Mojeek => crate::search::DEFAULT_MOJEEK_URL,
        SearchProvider::Ecosia => crate::search::DEFAULT_ECOSIA_URL,
    }
}

impl Settings {
    pub(crate) async fn chat_lock(&self, key: i64) -> Arc<tokio::sync::RwLock<()>> {
        let limit = config().chat.max_chat_locks;
        let mut locks = self.chat_locks.lock().await;
        if locks.len() >= limit && !locks.contains_key(&key) {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            if locks.len() >= limit {
                let idle = locks
                    .iter()
                    .find(|(_, lock)| Arc::strong_count(lock) == 1)
                    .map(|(key, _)| *key);
                if let Some(idle) = idle {
                    locks.remove(&idle);
                }
            }
        }
        Arc::clone(locks.entry(key).or_default())
    }

    pub(crate) async fn user_turn_lock(
        &self,
        chat_id: ChatId,
        user_id: UserId,
    ) -> Arc<tokio::sync::RwLock<()>> {
        let key = chat_id.0 ^ ((user_id.0 as i64).rotate_left(32) | 1);
        self.chat_lock(key).await
    }

    pub(crate) async fn user_rate_limit(&self, user_id: UserId) -> Arc<Semaphore> {
        let limit = config().chat.max_user_rate_limits;
        let permits = config().media.user_download_limit;
        let mut limits = self.user_rate_limits.write().await;
        if limits.len() >= limit && !limits.contains_key(&user_id) {
            limits.retain(|_, semaphore| {
                semaphore.available_permits() < permits || Arc::strong_count(semaphore) > 1
            });
            if limits.len() >= limit {
                let idle = limits
                    .iter()
                    .find(|(_, semaphore)| Arc::strong_count(semaphore) == 1)
                    .map(|(user_id, _)| *user_id);
                if let Some(idle) = idle {
                    limits.remove(&idle);
                }
            }
        }
        Arc::clone(
            limits
                .entry(user_id)
                .or_insert_with(|| Arc::new(Semaphore::new(permits))),
        )
    }
}

async fn check_cookies(path: &Path) -> Result<(), String> {
    let readable = match tokio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_file() => tokio::fs::File::open(path).await.is_ok(),
        _ => false,
    };
    if readable {
        Ok(())
    } else {
        Err("cookies_file must point to a readable regular file".into())
    }
}
