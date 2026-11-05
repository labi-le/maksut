use crate::prelude::*;
pub(crate) use crate::search_parse::*;
use crate::{search_failed, search_finished};

pub(crate) const DEFAULT_SEARCH_URL: &str = "https://html.duckduckgo.com/html/";
pub(crate) const DEFAULT_FIRECRAWL_URL: &str = "https://api.firecrawl.dev/v2/search";
pub(crate) const DEFAULT_FIRECRAWL_V1_URL: &str = "https://api.firecrawl.dev/v1/search";
pub(crate) const DEFAULT_PARALLEL_MCP_URL: &str = "https://search.parallel.ai/mcp";
pub(crate) const DEFAULT_EXA_MCP_URL: &str = "https://mcp.exa.ai/mcp?tools=web_search_exa";
pub(crate) const DEFAULT_STARTPAGE_URL: &str = "https://www.startpage.com/sp/search";
pub(crate) const DEFAULT_MOJEEK_URL: &str = "https://www.mojeek.com/search";
pub(crate) const DEFAULT_ECOSIA_URL: &str = "https://www.ecosia.org/search";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum SearchProvider {
    Auto,
    Public,
    Parallel,
    Exa,
    Firecrawl,
    Tavily,
    DuckDuckGo,
    Startpage,
    Mojeek,
    Ecosia,
}

impl SearchProvider {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "auto" => Self::Auto,
            "public" => Self::Public,
            "parallel" => Self::Parallel,
            "exa" => Self::Exa,
            "firecrawl" => Self::Firecrawl,
            "tavily" => Self::Tavily,
            "ddg" | "duckduckgo" => Self::DuckDuckGo,
            "startpage" => Self::Startpage,
            "mojeek" => Self::Mojeek,
            "ecosia" => Self::Ecosia,
            _ => return None,
        })
    }

    pub(crate) fn names() -> &'static [&'static str] {
        &[
            "auto",
            "public",
            "parallel",
            "exa",
            "firecrawl",
            "tavily",
            "ddg",
            "startpage",
            "mojeek",
            "ecosia",
        ]
    }
}

impl<'de> serde::Deserialize<'de> for SearchProvider {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::from_name(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!("must be one of {}", Self::names().join(", ")))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchHit {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) snippet: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchFailure {
    Unavailable,
    Timeout,
    TooLarge,
    Malformed,
    Canceled,
}

impl SearchFailure {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Unavailable => "недоступен",
            Self::Timeout => "таймаут",
            Self::TooLarge => "слишком большой ответ",
            Self::Malformed => "некорректный ответ",
            Self::Canceled => "отменён",
        }
    }
}

const ENGINE_COOLDOWN_SECS: u64 = 300;

/// `Clone` so `JoinSet` tasks can own it instead of borrowing `ChatConfig`.
#[derive(Clone)]
pub(crate) struct SearchClient {
    pub(crate) url: Url,
    pub(crate) provider: SearchProvider,
    pub(crate) tavily_key: Option<String>,
    pub(crate) http: reqwest::Client,
    pub(crate) engine_cooldowns: Arc<std::sync::Mutex<HashMap<SearchProvider, time::Instant>>>,
}

impl SearchClient {
    pub(crate) fn is_engine_cooled_down(&self, engine: SearchProvider) -> bool {
        let Ok(mut map) = self.engine_cooldowns.lock() else {
            return false;
        };
        let now = time::Instant::now();
        map.retain(|_, until| *until > now);
        map.get(&engine).is_some_and(|until| *until > now)
    }

    pub(crate) fn note_engine_cooldown(&self, engine: SearchProvider, status: u16) {
        if !matches!(status, 401 | 403 | 429) {
            return;
        }
        let Ok(mut map) = self.engine_cooldowns.lock() else {
            return;
        };
        let now = time::Instant::now();
        map.retain(|_, until| *until > now);
        map.insert(engine, now + Duration::from_secs(ENGINE_COOLDOWN_SECS));
    }

    pub(crate) async fn search_hits_with_recency(
        &self,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        let started = time::Instant::now();
        let result = match self.provider {
            SearchProvider::Auto => self.auto_search(query, limit, recency).await,
            SearchProvider::Public => self.public_web_search(query, limit, recency).await,
            SearchProvider::Parallel
            | SearchProvider::Exa
            | SearchProvider::Firecrawl
            | SearchProvider::Tavily
            | SearchProvider::DuckDuckGo
            | SearchProvider::Startpage
            | SearchProvider::Mojeek
            | SearchProvider::Ecosia => {
                // A single configured engine has no chain deadline of its own,
                // so the retry loop is bounded by the same budget as auto mode.
                let budget = Duration::from_secs(config().search.auto_deadline_secs);
                match time::timeout(budget, self.dispatch_single(query, limit, recency)).await {
                    Ok(result) => result,
                    Err(_) => Err(SearchFailure::Timeout),
                }
            }
        };
        let elapsed = crate::elapsed_ms(started);
        match &result {
            Ok(hits) => search_finished(self.provider, recency, hits.len(), elapsed),
            Err(error) => search_failed(self.provider, recency, error, elapsed),
        }
        result
    }

    async fn dispatch_single(
        &self,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        match self.provider {
            SearchProvider::Auto | SearchProvider::Public => Err(SearchFailure::Unavailable),
            SearchProvider::Parallel => {
                self.parallel_mcp_search(&self.url, query, limit, recency)
                    .await
            }
            SearchProvider::Exa => self.exa_mcp_search(&self.url, query, limit).await,
            SearchProvider::Firecrawl => {
                self.firecrawl_search(&self.url, query, limit, recency)
                    .await
            }
            SearchProvider::Tavily => match self.tavily_key.as_deref() {
                Some(key) => {
                    self.tavily_search(&self.url, key, query, limit, recency)
                        .await
                }
                None => self.auto_search(query, limit, recency).await,
            },
            SearchProvider::DuckDuckGo => self.ddg_search(&self.url, query, limit, recency).await,
            SearchProvider::Startpage => {
                self.startpage_search(&self.url, query, limit, recency)
                    .await
            }
            SearchProvider::Mojeek => self.mojeek_search(&self.url, query, limit, recency).await,
            SearchProvider::Ecosia => self.ecosia_search(&self.url, query, limit).await,
        }
    }

    /// omp-style automatic keyless search chain: tries configured Tavily (when
    /// key is present), Firecrawl v2/v1, DuckDuckGo HTML, Parallel MCP, Exa MCP,
    /// Startpage, Mojeek, and Ecosia in order, returning the first non-empty hit list.
    async fn auto_search(
        &self,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        let deadline =
            time::Instant::now() + Duration::from_secs(config().search.auto_deadline_secs);
        let mut last_error = SearchFailure::Unavailable;
        let mut saw_empty = false;
        let tavily_key = self
            .tavily_key
            .as_deref()
            .filter(|_| !self.is_engine_cooled_down(SearchProvider::Tavily));
        if let Some(key) = tavily_key {
            if let Ok(url) = Url::parse("https://api.tavily.com/search") {
                let remaining = deadline.saturating_duration_since(time::Instant::now());
                match time::timeout(
                    remaining,
                    self.tavily_search(&url, key, query, limit, recency),
                )
                .await
                {
                    Ok(Ok(hits)) if !hits.is_empty() => return Ok(hits),
                    Ok(Ok(_)) => saw_empty = true,
                    Ok(Err(err)) => last_error = err,
                    Err(_) => return Err(SearchFailure::Timeout),
                }
            }
        }
        for engine in [
            SearchProvider::Firecrawl,
            SearchProvider::DuckDuckGo,
            SearchProvider::Parallel,
            SearchProvider::Exa,
            SearchProvider::Startpage,
            SearchProvider::Mojeek,
            SearchProvider::Ecosia,
        ] {
            if self.is_engine_cooled_down(engine) {
                continue;
            }
            let remaining = deadline.saturating_duration_since(time::Instant::now());
            if remaining.is_zero() {
                return if saw_empty {
                    Ok(Vec::new())
                } else {
                    Err(SearchFailure::Timeout)
                };
            }
            match time::timeout(
                remaining,
                self.run_single_engine(engine, query, limit, recency),
            )
            .await
            {
                Ok(Ok(hits)) if !hits.is_empty() => return Ok(hits),
                Ok(Ok(_)) => {
                    saw_empty = true;
                }
                Ok(Err(err)) => {
                    last_error = err;
                }
                Err(_) => {
                    last_error = SearchFailure::Timeout;
                    break;
                }
            }
        }
        if saw_empty {
            Ok(Vec::new())
        } else {
            Err(last_error)
        }
    }
    /// omp `web/public`-style fanout across multiple keyless engines for one
    /// query, deduplicating and ranking by cross-engine consensus, then best
    /// per-engine rank, keeping the longest snippet.
    pub(crate) async fn public_web_search(
        &self,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        match self.provider {
            SearchProvider::Auto | SearchProvider::Public => {}
            SearchProvider::Parallel => {
                return self
                    .parallel_mcp_search(&self.url, query, limit, recency)
                    .await
            }
            SearchProvider::Exa => return self.exa_mcp_search(&self.url, query, limit).await,
            SearchProvider::Firecrawl => {
                return self
                    .firecrawl_search(&self.url, query, limit, recency)
                    .await
            }
            SearchProvider::Tavily => {
                if let Some(key) = self.tavily_key.as_deref() {
                    return self
                        .tavily_search(&self.url, key, query, limit, recency)
                        .await;
                }
            }
            SearchProvider::DuckDuckGo => {
                return self.ddg_search(&self.url, query, limit, recency).await
            }
            SearchProvider::Startpage => {
                return self
                    .startpage_search(&self.url, query, limit, recency)
                    .await
            }
            SearchProvider::Mojeek => {
                return self.mojeek_search(&self.url, query, limit, recency).await
            }
            SearchProvider::Ecosia => return self.ecosia_search(&self.url, query, limit).await,
        }
        let engines = [
            SearchProvider::Firecrawl,
            SearchProvider::DuckDuckGo,
            SearchProvider::Parallel,
            SearchProvider::Exa,
            SearchProvider::Startpage,
            SearchProvider::Mojeek,
            SearchProvider::Ecosia,
        ];
        let mut tasks = tokio::task::JoinSet::new();
        for (engine_idx, engine) in engines.into_iter().enumerate() {
            if self.is_engine_cooled_down(engine) {
                continue;
            }
            let client = self.clone();
            let query = query.to_owned();
            let recency = recency.map(str::to_owned);
            tasks.spawn(async move {
                let res = client
                    .run_single_engine(engine, &query, limit.max(8), recency.as_deref())
                    .await;
                (engine_idx, res)
            });
        }
        let mut engine_results: Vec<(usize, Vec<SearchHit>)> = Vec::new();
        let mut last_err = SearchFailure::Unavailable;
        let mut saw_ok = false;
        let soft =
            time::Instant::now() + Duration::from_secs(config().search.public_soft_deadline_secs);
        let hard =
            time::Instant::now() + Duration::from_secs(config().search.public_hard_deadline_secs);
        loop {
            let deadline = if engine_results.iter().any(|(_, hits)| !hits.is_empty()) {
                soft
            } else {
                hard
            };
            let remaining = deadline.saturating_duration_since(time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match time::timeout(remaining, tasks.join_next()).await {
                Ok(Some(Ok((idx, Ok(hits))))) => {
                    saw_ok = true;
                    if !hits.is_empty() {
                        engine_results.push((idx, hits));
                    }
                }
                Ok(Some(Ok((_, Err(err))))) => last_err = err,
                Ok(Some(Err(_))) => {}
                Ok(None) | Err(_) => break,
            }
        }
        tasks.abort_all();
        if engine_results.is_empty() {
            return if saw_ok {
                Ok(Vec::new())
            } else {
                Err(last_err)
            };
        }
        engine_results.sort_by_key(|(idx, _)| *idx);
        Ok(merge_public_hits(&engine_results, limit))
    }

    async fn run_single_engine(
        &self,
        engine: SearchProvider,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        match engine {
            SearchProvider::Firecrawl => {
                let url =
                    Url::parse(DEFAULT_FIRECRAWL_URL).map_err(|_| SearchFailure::Unavailable)?;
                match self.firecrawl_search(&url, query, limit, recency).await {
                    Ok(hits) if !hits.is_empty() => Ok(hits),
                    _ if self.is_engine_cooled_down(SearchProvider::Firecrawl) => {
                        Err(SearchFailure::Unavailable)
                    }
                    _ => {
                        let v1 = Url::parse(DEFAULT_FIRECRAWL_V1_URL)
                            .map_err(|_| SearchFailure::Unavailable)?;
                        self.firecrawl_search(&v1, query, limit, recency).await
                    }
                }
            }
            SearchProvider::DuckDuckGo => {
                let url = Url::parse(crate::DEFAULT_SEARCH_URL)
                    .map_err(|_| SearchFailure::Unavailable)?;
                self.ddg_search(&url, query, limit, recency).await
            }
            SearchProvider::Parallel => {
                let url =
                    Url::parse(DEFAULT_PARALLEL_MCP_URL).map_err(|_| SearchFailure::Unavailable)?;
                self.parallel_mcp_search(&url, query, limit, recency).await
            }
            SearchProvider::Exa => {
                let url =
                    Url::parse(DEFAULT_EXA_MCP_URL).map_err(|_| SearchFailure::Unavailable)?;
                self.exa_mcp_search(&url, query, limit).await
            }
            SearchProvider::Startpage => {
                let url =
                    Url::parse(DEFAULT_STARTPAGE_URL).map_err(|_| SearchFailure::Unavailable)?;
                self.startpage_search(&url, query, limit, recency).await
            }
            SearchProvider::Mojeek => {
                let url = Url::parse(DEFAULT_MOJEEK_URL).map_err(|_| SearchFailure::Unavailable)?;
                self.mojeek_search(&url, query, limit, recency).await
            }
            SearchProvider::Ecosia => {
                let url = Url::parse(DEFAULT_ECOSIA_URL).map_err(|_| SearchFailure::Unavailable)?;
                self.ecosia_search(&url, query, limit).await
            }
            SearchProvider::Auto | SearchProvider::Public | SearchProvider::Tavily => {
                Err(SearchFailure::Unavailable)
            }
        }
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn web_search_with(
        &self,
        query: &str,
        recency: Option<&str>,
        limit: usize,
    ) -> String {
        let limit = limit.clamp(1, 10);
        let hits = match self.search_hits_with_recency(query, limit, recency).await {
            Ok(hits) => hits,
            Err(SearchFailure::TooLarge) => return "Поиск вернул слишком большой ответ.".into(),
            Err(_) => return "Поиск недоступен.".into(),
        };
        if hits.is_empty() {
            return "Ничего не найдено.".into();
        }
        let mut report = String::new();
        let result_chars = config().search.result_chars;
        for (index, hit) in hits.iter().enumerate() {
            tracing::trace!(snippet = %crate::sanitize_text(&hit.snippet), "hit");
            report.push_str(&format!(
                "{}. {}\n{}\n{}\n\n",
                index + 1,
                hit.title,
                hit.url,
                hit.snippet
            ));
            if report.chars().count() > result_chars {
                break;
            }
        }
        report.chars().take(result_chars).collect()
    }
}

struct MergedPublicEntry {
    hit: SearchHit,
    engines: usize,
    best_rank: usize,
    order: usize,
}

pub(crate) fn merge_public_hits(
    engine_results: &[(usize, Vec<SearchHit>)],
    limit: usize,
) -> Vec<SearchHit> {
    let mut merged: HashMap<String, MergedPublicEntry> = HashMap::new();
    let mut order_seq = 0usize;
    for (_, hits) in engine_results {
        for (rank, hit) in hits.iter().enumerate() {
            let key = public_dedup_key(&hit.url);
            match merged.entry(key) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    existing.engines += 1;
                    if rank < existing.best_rank {
                        existing.best_rank = rank;
                        if !hit.title.is_empty() {
                            existing.hit.title = hit.title.clone();
                        }
                        existing.hit.url = hit.url.clone();
                    }
                    if hit.snippet.chars().count() > existing.hit.snippet.chars().count() {
                        existing.hit.snippet = hit.snippet.clone();
                    }
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(MergedPublicEntry {
                        hit: hit.clone(),
                        engines: 1,
                        best_rank: rank,
                        order: order_seq,
                    });
                    order_seq += 1;
                }
            }
        }
    }
    let mut values: Vec<MergedPublicEntry> = merged.into_values().collect();
    values.sort_by(|left, right| {
        right
            .engines
            .cmp(&left.engines)
            .then(left.best_rank.cmp(&right.best_rank))
            .then(left.order.cmp(&right.order))
    });
    values.into_iter().take(limit).map(|e| e.hit).collect()
}

fn public_dedup_key(raw: &str) -> String {
    let Ok(url) = Url::parse(raw.trim()) else {
        return raw.trim().to_owned();
    };
    let host = url
        .host_str()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .strip_prefix("www.")
        .map(str::to_owned)
        .unwrap_or_else(|| url.host_str().unwrap_or_default().to_ascii_lowercase());
    let mut path = url.path().to_owned();
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    format!("{host}{path}{query}")
}
