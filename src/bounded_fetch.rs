use crate::browser::USER_AGENT;
use crate::prelude::*;
use crate::read_url::{pinned_public_client, validate_url};

pub(crate) struct Fetched {
    pub(crate) body: Vec<u8>,
    pub(crate) content_type: String,
}

/// One GET with the reader's SSRF guards: public-IPv4 pinning, a validated URL
/// on every redirect hop, a whole-request deadline, and a hard byte cap. A
/// session cookie is sent to the original host and scheme only: a redirect to
/// another host must not receive the operator's tracker session. The body is
/// untrusted and never logged.
pub(crate) async fn fetch(
    mut url: Url,
    accept: &'static str,
    cookie: Option<&str>,
    max_bytes: usize,
) -> Result<Fetched, String> {
    let settings = &config().read_url;
    let deadline = time::Instant::now() + Duration::from_secs(settings.total_timeout_secs);
    let cookie_origin = url
        .host_str()
        .map(|host| (host.to_ascii_lowercase(), url.scheme().to_owned()));
    let mut redirects = 0usize;
    loop {
        validate_url(&url).map_err(|error| error.message())?;
        let remaining = deadline.saturating_duration_since(time::Instant::now());
        if remaining.is_zero() {
            return Err("таймаут".into());
        }
        let client = time::timeout(remaining, pinned_public_client(&url))
            .await
            .map_err(|_| "таймаут".to_string())?
            .map_err(|error| error.message())?;
        let mut request = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, accept)
            .header(reqwest::header::USER_AGENT, USER_AGENT);
        if let Some(cookie) = cookie {
            let same_origin = cookie_origin.as_ref().is_some_and(|(host, scheme)| {
                url.host_str()
                    .is_some_and(|current| current.eq_ignore_ascii_case(host))
                    && url.scheme() == scheme
            });
            if same_origin {
                request = request.header(reqwest::header::COOKIE, cookie);
            }
        }
        let remaining = deadline.saturating_duration_since(time::Instant::now());
        if remaining.is_zero() {
            return Err("таймаут".into());
        }
        let response = time::timeout(remaining, request.send())
            .await
            .map_err(|_| "таймаут".to_string())?
            .map_err(|_| "хост недоступен".to_string())?;
        if response.status().is_redirection() {
            if redirects >= settings.max_redirects {
                return Err("слишком много редиректов".into());
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "некорректный редирект".to_string())?;
            url = url
                .join(location)
                .map_err(|_| "некорректный редирект".to_string())?;
            redirects += 1;
            continue;
        }
        let status = response.status();
        if !status.is_success() {
            return Err(format!("хост ответил HTTP {}", status.as_u16()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let remaining = deadline.saturating_duration_since(time::Instant::now());
        if remaining.is_zero() {
            return Err("таймаут".into());
        }
        let body = time::timeout(remaining, read_bounded(response, max_bytes))
            .await
            .map_err(|_| "таймаут".to_string())??;
        return Ok(Fetched { body, content_type });
    }
}

async fn read_bounded(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err("ответ слишком большой".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "соединение оборвалось".to_string())?
    {
        if body.len() + chunk.len() > max_bytes {
            return Err("ответ слишком большой".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
