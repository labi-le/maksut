use crate::browser::USER_AGENT;
use crate::prelude::*;
use crate::read_url::{pinned_public_client, validate_url, ReadUrlError};
use crate::retry::RetryPolicy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FetchCategory {
    Http,
    Timeout,
    Transport,
    TooLarge,
    Redirect,
    InvalidUrl,
}

/// A failed fetch, with the request coordinates kept apart from the text a chat
/// participant reads. `host`/`path` never carry the query string, so a failure
/// can be logged or shown as a label without leaking a tracker search term.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FetchError {
    pub(crate) category: FetchCategory,
    pub(crate) status: Option<u16>,
    pub(crate) host: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) message: String,
}

impl FetchError {
    pub(crate) fn new(category: FetchCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            status: None,
            host: None,
            path: None,
            message: message.into(),
        }
    }

    /// Records the URL a hop was attempted against; `query()` is deliberately
    /// never copied, since it can hold a search term or a token.
    pub(crate) fn at(mut self, url: &Url) -> Self {
        self.host = url.host_str().map(str::to_owned);
        self.path = Some(url.path().to_owned());
        self
    }

    fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub(crate) fn user_reason(&self) -> String {
        self.to_string()
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FetchError {}

/// Callers that surface a failure as chat text keep the previous wording; the
/// structured fields stay available to anyone holding the `FetchError` itself.
impl From<FetchError> for String {
    fn from(error: FetchError) -> Self {
        error.message
    }
}

pub(crate) struct Fetched {
    pub(crate) body: Vec<u8>,
    pub(crate) content_type: String,
    pub(crate) host: Option<String>,
    pub(crate) path: Option<String>,
}

/// One GET with the reader's SSRF guards: public-IPv4 pinning, a validated URL
/// on every redirect hop, a whole-request deadline, and a hard byte cap. A
/// session cookie is sent to the original host and scheme only: a redirect to
/// another host must not receive the operator's tracker session. The body is
/// untrusted and never logged.
pub(crate) async fn fetch(
    url: Url,
    accept: &'static str,
    cookie: Option<&str>,
    max_bytes: usize,
) -> Result<Fetched, FetchError> {
    let deadline = time::Instant::now() + Duration::from_secs(config().read_url.total_timeout_secs);
    fetch_until(url, accept, cookie, max_bytes, deadline).await
}

/// Same GET, with the retry budget drawn from `retry.read_url` and shared with
/// the deadline, so a retry can never outlive the caller's time bound.
pub(crate) async fn fetch_until(
    url: Url,
    accept: &'static str,
    cookie: Option<&str>,
    max_bytes: usize,
    deadline: time::Instant,
) -> Result<Fetched, FetchError> {
    let settings = &config().read_url;
    let policy = &config().retry.read_url;
    let cookie_origin = url
        .host_str()
        .map(|host| (host.to_ascii_lowercase(), url.scheme().to_owned()));
    let mut current = url;
    let mut redirects = 0usize;
    loop {
        // The single retry gate: calling `attempt_once` directly here would
        // silently drop the configured budget.
        let hop = retry_attempts(policy, deadline, || {
            attempt_once(
                &current,
                accept,
                cookie,
                cookie_origin.as_ref(),
                max_bytes,
                deadline,
            )
        })
        .await?;
        match hop {
            Hop::Body { body, content_type } => {
                return Ok(Fetched {
                    body,
                    content_type,
                    host: current.host_str().map(str::to_owned),
                    path: Some(current.path().to_owned()),
                });
            }
            Hop::Redirect(next) => {
                if redirects >= settings.max_redirects {
                    return Err(FetchError::new(
                        FetchCategory::Redirect,
                        "слишком много редиректов",
                    )
                    .at(&current));
                }
                redirects += 1;
                // A redirect is progress, not a failure: the next URL starts
                // with a fresh retry budget instead of inheriting the hop's.
                current = next;
            }
        }
    }
}

/// Runs one attempt function under the retry policy. Kept generic so a test can
/// pin the 429-then-success and 429-then-error sequences without a socket.
pub(crate) async fn retry_attempts<F, Fut, T>(
    policy: &RetryPolicy,
    deadline: time::Instant,
    mut attempt_fn: F,
) -> Result<T, FetchError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, FetchError>>,
{
    let mut attempt = 0u32;
    loop {
        match attempt_fn().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !retryable(policy, &error) || !policy.should_retry(attempt) {
                    return Err(error);
                }
                tracing::debug!(
                    attempt = attempt + 1,
                    host = error.host.as_deref().unwrap_or_default(),
                    status = ?error.status,
                    "fetch retry"
                );
                if !policy.backoff(attempt, deadline).await {
                    return Err(error);
                }
                attempt += 1;
            }
        }
    }
}

enum Hop {
    Body { body: Vec<u8>, content_type: String },
    Redirect(Url),
}

async fn attempt_once(
    url: &Url,
    accept: &'static str,
    cookie: Option<&str>,
    cookie_origin: Option<&(String, String)>,
    max_bytes: usize,
    deadline: time::Instant,
) -> Result<Hop, FetchError> {
    validate_url(url)
        .map_err(|error| FetchError::new(FetchCategory::InvalidUrl, error.message()).at(url))?;
    let client = time::timeout(remaining(deadline, url)?, pinned_public_client(url))
        .await
        .map_err(|_| timeout_error(url))?
        .map_err(|error| FetchError::new(client_category(&error), error.message()).at(url))?;
    let mut request = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, accept)
        .header(reqwest::header::USER_AGENT, USER_AGENT);
    if let Some(cookie) = cookie {
        let same_origin = cookie_origin.is_some_and(|(host, scheme)| {
            url.host_str()
                .is_some_and(|current| current.eq_ignore_ascii_case(host))
                && url.scheme() == scheme
        });
        if same_origin {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
    }
    let response = time::timeout(remaining(deadline, url)?, request.send())
        .await
        .map_err(|_| timeout_error(url))?
        .map_err(|_| FetchError::new(FetchCategory::Transport, "хост недоступен").at(url))?;
    let status = response.status();
    if status.is_redirection() {
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                FetchError::new(FetchCategory::Redirect, "некорректный редирект").at(url)
            })?;
        let next = url.join(location).map_err(|_| {
            FetchError::new(FetchCategory::Redirect, "некорректный редирект").at(url)
        })?;
        return Ok(Hop::Redirect(next));
    }
    if !status.is_success() {
        return Err(FetchError::new(
            FetchCategory::Http,
            format!("хост ответил HTTP {}", status.as_u16()),
        )
        .with_status(status.as_u16())
        .at(url));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body = time::timeout(
        remaining(deadline, url)?,
        read_bounded(response, max_bytes, url),
    )
    .await
    .map_err(|_| timeout_error(url))??;
    Ok(Hop::Body { body, content_type })
}

/// Timeouts never retry: the request that timed out was given the whole
/// remaining budget, so there is nothing left for a backoff sleep.
pub(crate) fn retryable(policy: &RetryPolicy, error: &FetchError) -> bool {
    match error.category {
        FetchCategory::Http => error
            .status
            .is_some_and(|status| policy.retry_status(status)),
        FetchCategory::Transport => policy.retry_transport,
        FetchCategory::Timeout
        | FetchCategory::TooLarge
        | FetchCategory::Redirect
        | FetchCategory::InvalidUrl => false,
    }
}

fn client_category(error: &ReadUrlError) -> FetchCategory {
    match error {
        ReadUrlError::Timeout => FetchCategory::Timeout,
        ReadUrlError::Network => FetchCategory::Transport,
        ReadUrlError::NoPublicAddress
        | ReadUrlError::InvalidUrl
        | ReadUrlError::UnsupportedScheme
        | ReadUrlError::Credentials
        | ReadUrlError::PortNotAllowed
        | ReadUrlError::LocalHost
        | ReadUrlError::TooComplex
        | ReadUrlError::Empty
        | ReadUrlError::UnsupportedContent(_) => FetchCategory::InvalidUrl,
        ReadUrlError::TooManyRedirects | ReadUrlError::Redirect => FetchCategory::Redirect,
        ReadUrlError::Http(_) => FetchCategory::Http,
        ReadUrlError::TooLarge => FetchCategory::TooLarge,
    }
}

fn timeout_error(url: &Url) -> FetchError {
    FetchError::new(FetchCategory::Timeout, "таймаут").at(url)
}

fn remaining(deadline: time::Instant, url: &Url) -> Result<Duration, FetchError> {
    let left = deadline.saturating_duration_since(time::Instant::now());
    if left.is_zero() {
        return Err(timeout_error(url));
    }
    Ok(left)
}

async fn read_bounded(
    mut response: reqwest::Response,
    max_bytes: usize,
    url: &Url,
) -> Result<Vec<u8>, FetchError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(FetchError::new(FetchCategory::TooLarge, "ответ слишком большой").at(url));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| FetchError::new(FetchCategory::Transport, "соединение оборвалось").at(url))?
    {
        if body.len() + chunk.len() > max_bytes {
            return Err(FetchError::new(FetchCategory::TooLarge, "ответ слишком большой").at(url));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
