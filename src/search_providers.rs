use crate::prelude::*;
use crate::search::{SearchClient, SearchFailure, SearchHit};
use crate::search_parse::*;
use serde_json::json;

const MAX_SEARCH_BYTES: usize = 512 * 1024;
const SEARCH_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0";

impl SearchClient {
    fn map_error(error: reqwest::Error) -> SearchFailure {
        if error.is_timeout() {
            SearchFailure::Timeout
        } else {
            SearchFailure::Unavailable
        }
    }

    async fn response_bytes(response: reqwest::Response) -> Result<Vec<u8>, SearchFailure> {
        if response
            .content_length()
            .is_some_and(|length| length > MAX_SEARCH_BYTES as u64)
        {
            return Err(SearchFailure::TooLarge);
        }
        let bytes = response.bytes().await.map_err(Self::map_error)?;
        if bytes.len() > MAX_SEARCH_BYTES {
            return Err(SearchFailure::TooLarge);
        }
        Ok(bytes.to_vec())
    }

    async fn get_html(
        &self,
        url: Url,
        referer: &str,
        blocked_markers: &[&str],
    ) -> Result<String, SearchFailure> {
        let response = self
            .http
            .get(url)
            .header("user-agent", SEARCH_USER_AGENT)
            .header("referer", referer)
            .send()
            .await
            .map_err(Self::map_error)?;
        if !response.status().is_success() {
            return Err(SearchFailure::Unavailable);
        }
        let bytes = Self::response_bytes(response).await?;
        let html = String::from_utf8_lossy(&bytes).into_owned();
        if blocked_markers.iter().any(|marker| html.contains(marker)) {
            return Err(SearchFailure::Unavailable);
        }
        Ok(html)
    }

    pub(crate) async fn tavily_search(
        &self,
        url: &Url,
        key: &str,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        let mut body = json!({
            "api_key": key,
            "query": query,
            "max_results": limit,
            "search_depth": "basic",
        });
        if let Some(range) = recency.and_then(recency_tavily) {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("time_range".into(), json!(range));
            }
        }
        let response = self
            .http
            .post(url.clone())
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(Self::map_error)?;
        if !response.status().is_success() {
            return Err(SearchFailure::Unavailable);
        }
        let bytes = Self::response_bytes(response).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| SearchFailure::Malformed)?;
        if value
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| !error.trim().is_empty())
        {
            return Err(SearchFailure::Unavailable);
        }
        Ok(value
            .get("results")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .take(limit)
            .map(|item| SearchHit {
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                url: item
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                snippet: item
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            })
            .collect())
    }

    pub(crate) async fn firecrawl_search(
        &self,
        url: &Url,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        let is_v2 = url.path().contains("/v2/");
        let mut body = if is_v2 {
            json!({"query": query, "limit": limit, "sources": [{"type": "web"}]})
        } else {
            json!({"query": query, "limit": limit})
        };
        if let Some(tbs) = recency.and_then(recency_tbs) {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("tbs".into(), json!(tbs));
            }
        }
        let response = self
            .http
            .post(url.clone())
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(Self::map_error)?;
        if !response.status().is_success() {
            return Err(SearchFailure::Unavailable);
        }
        let bytes = Self::response_bytes(response).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| SearchFailure::Malformed)?;
        if value.get("success").and_then(Value::as_bool) == Some(false) {
            return Err(SearchFailure::Unavailable);
        }
        let items = value
            .get("data")
            .and_then(|data| {
                data.as_array()
                    .or_else(|| data.get("web").and_then(Value::as_array))
            })
            .or_else(|| value.get("results").and_then(Value::as_array))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Ok(items
            .iter()
            .take(limit)
            .filter_map(|item| {
                let hit_url = item.get("url").and_then(Value::as_str)?;
                if hit_url.trim().is_empty() {
                    return None;
                }
                Some(SearchHit {
                    title: item
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or(hit_url)
                        .to_owned(),
                    url: hit_url.to_owned(),
                    snippet: item
                        .get("description")
                        .or_else(|| item.get("snippet"))
                        .or_else(|| item.get("markdown"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                })
            })
            .collect())
    }

    pub(crate) async fn parallel_mcp_search(
        &self,
        url: &Url,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        let mcp_query = match recency {
            Some("day" | "d") => format!("{query} (past day)"),
            Some("week" | "w") => format!("{query} (past week)"),
            Some("month" | "m") => format!("{query} (past month)"),
            Some("year" | "y") => format!("{query} (past year)"),
            _ => query.to_owned(),
        };
        let body = json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": "tools/call",
            "params": {
                "name": "web_search",
                "arguments": {
                    "objective": query,
                    "search_queries": [mcp_query],
                }
            }
        });
        let response = self
            .http
            .post(url.clone())
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("user-agent", "omp/18.13.2")
            .body(body.to_string())
            .send()
            .await
            .map_err(Self::map_error)?;
        if !response.status().is_success() {
            return Err(SearchFailure::Unavailable);
        }
        let bytes = Self::response_bytes(response).await?;
        let result = parse_mcp_jsonrpc_result(&bytes).ok_or(SearchFailure::Malformed)?;
        Ok(parse_parallel_mcp_hits(&result, limit))
    }

    pub(crate) async fn exa_mcp_search(
        &self,
        url: &Url,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": query,
                    "numResults": limit,
                }
            }
        });
        let response = self
            .http
            .post(url.clone())
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("user-agent", SEARCH_USER_AGENT)
            .body(body.to_string())
            .send()
            .await
            .map_err(Self::map_error)?;
        if !response.status().is_success() {
            return Err(SearchFailure::Unavailable);
        }
        let bytes = Self::response_bytes(response).await?;
        let result = parse_mcp_jsonrpc_result(&bytes).ok_or(SearchFailure::Malformed)?;
        Ok(parse_exa_mcp_hits(&result, limit))
    }

    pub(crate) async fn ddg_search(
        &self,
        base_url: &Url,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        let mut url = base_url.clone();
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("q", query);
            if let Some(df) = recency.and_then(recency_ddg) {
                pairs.append_pair("df", df);
            }
        }
        let html = self
            .get_html(
                url,
                "https://html.duckduckgo.com/",
                &["anomaly-modal", "anomaly.js"],
            )
            .await?;
        Ok(extract_search_results(&html, limit)
            .into_iter()
            .map(|(title, url, snippet)| SearchHit {
                title,
                url,
                snippet,
            })
            .collect())
    }

    pub(crate) async fn startpage_search(
        &self,
        base_url: &Url,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        let mut url = base_url.clone();
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("query", query);
            if let Some(df) = recency.and_then(recency_ddg) {
                pairs.append_pair("with_date", df);
            }
        }
        let html = self
            .get_html(
                url,
                "https://www.startpage.com/",
                &["/sp/captcha", "anubis_challenge"],
            )
            .await?;
        Ok(extract_startpage_results(&html, limit))
    }

    pub(crate) async fn mojeek_search(
        &self,
        base_url: &Url,
        query: &str,
        limit: usize,
        recency: Option<&str>,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        let mut url = base_url.clone();
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("q", query);
            pairs.append_pair("t", &limit.to_string());
            pairs.append_pair("arc", "none");
            if let Some(since) = recency.and_then(recency_tavily) {
                pairs.append_pair("since", since);
            }
        }
        let html = self
            .get_html(
                url,
                "https://www.mojeek.com/",
                &["altcha-widget", "sending automated queries"],
            )
            .await?;
        Ok(extract_mojeek_results(&html, limit))
    }

    pub(crate) async fn ecosia_search(
        &self,
        base_url: &Url,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, SearchFailure> {
        let mut url = base_url.clone();
        url.query_pairs_mut().append_pair("q", query);
        let html = self
            .get_html(
                url,
                "https://www.ecosia.org/",
                &["Ecosia Firewall", "_cf_chl_opt"],
            )
            .await?;
        Ok(extract_ecosia_results(&html, limit))
    }
}

fn recency_ddg(recency: &str) -> Option<&'static str> {
    match recency.trim().to_ascii_lowercase().as_str() {
        "day" | "d" => Some("d"),
        "week" | "w" => Some("w"),
        "month" | "m" => Some("m"),
        "year" | "y" => Some("y"),
        _ => None,
    }
}

fn recency_tbs(recency: &str) -> Option<&'static str> {
    match recency.trim().to_ascii_lowercase().as_str() {
        "day" | "d" => Some("qdr:d"),
        "week" | "w" => Some("qdr:w"),
        "month" | "m" => Some("qdr:m"),
        "year" | "y" => Some("qdr:y"),
        _ => None,
    }
}

fn recency_tavily(recency: &str) -> Option<&'static str> {
    match recency.trim().to_ascii_lowercase().as_str() {
        "day" | "d" => Some("day"),
        "week" | "w" => Some("week"),
        "month" | "m" => Some("month"),
        "year" | "y" => Some("year"),
        _ => None,
    }
}
