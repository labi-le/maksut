use crate::*;

pub(crate) fn twitter_status_id(url: &Url) -> Option<&str> {
    if image_source(url) != Some("twitter") {
        return None;
    }
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    let mut segments = path.trim_start_matches('/').split('/');
    let (Some(user), Some("status"), Some(id)) =
        (segments.next(), segments.next(), segments.next())
    else {
        return None;
    };
    let valid_index = |index: &str| {
        !index.is_empty()
            && index.bytes().all(|byte| byte.is_ascii_digit())
            && index.bytes().any(|byte| byte != b'0')
    };
    let suffix_valid = match (segments.next(), segments.next(), segments.next()) {
        (None, None, None) => true,
        (Some("video"), None, None) => true,
        (Some("photo" | "video"), Some(index), None) => valid_index(index),
        _ => false,
    };
    (suffix_valid
        && !user.is_empty()
        && !id.is_empty()
        && id.bytes().all(|byte| byte.is_ascii_digit()))
    .then_some(id)
}

#[derive(Debug)]
pub(crate) struct TwitterImagesError {
    pub(crate) auth: bool,
}

impl TwitterImagesError {
    pub(crate) fn new(auth: bool) -> Self {
        Self { auth }
    }
}

pub(crate) async fn extract_twitter_images(
    url: &Url,
    cfg: &Settings,
    dir: &Path,
    duration: Duration,
) -> Result<ImageMetadata, TwitterImagesError> {
    let id = twitter_status_id(url).ok_or_else(|| TwitterImagesError::new(false))?;
    let cookie = process_cookies_async(cfg.cookies_file.as_deref(), dir)
        .await
        .map_err(|_| TwitterImagesError::new(false))?;
    let mut cmd = Command::new(&cfg.python_bin);
    cmd.arg(&cfg.twitter_helper)
        .arg(id)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some((_, path)) = &cookie {
        cmd.arg(path);
    }
    #[cfg(unix)]
    cmd.as_std_mut().process_group(0);
    let child = cmd.spawn().map_err(|_| TwitterImagesError::new(false))?;
    let mut process = DownloadProcess::new(child).map_err(|_| TwitterImagesError::new(false))?;
    let mut stdout = process
        .take_stdout(config().media.max_twitter_json_bytes + 1)
        .map_err(|_| TwitterImagesError::new(false))?;
    let (bytes, status) = time::timeout(duration, async {
        let mut bytes = Vec::new();
        stdout
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| TwitterImagesError::new(false))?;
        if bytes.len() as u64 > config().media.max_twitter_json_bytes {
            return Err(TwitterImagesError::new(false));
        }
        let status = process
            .wait()
            .await
            .map_err(|_| TwitterImagesError::new(false))?;
        Ok::<_, TwitterImagesError>((bytes, status))
    })
    .await
    .map_err(|_| TwitterImagesError::new(false))??;
    if !status.success() {
        return Err(TwitterImagesError::new(status.code() == Some(3)));
    }
    let info: Value = serde_json::from_slice(&bytes).map_err(|_| TwitterImagesError::new(false))?;
    let urls = info
        .get("photos")
        .and_then(Value::as_array)
        .ok_or_else(|| TwitterImagesError::new(false))?;
    let has_video = info
        .get("has_video")
        .and_then(Value::as_bool)
        .ok_or_else(|| TwitterImagesError::new(false))?;
    let mut result = ImageMetadata {
        images: Vec::new(),
        has_video,
        skipped: 0,
        alternates: Vec::new(),
        description: info
            .get("description")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
    };
    for raw in urls {
        if let Some(image) = raw.as_str().and_then(|raw| image_cdn_url(raw, "twitter")) {
            if !result.images.contains(&image) {
                result.images.push(image);
            }
        } else {
            result.skipped += 1;
        }
    }
    Ok(result)
}

pub(crate) fn twitter_image_extension(
    url: &Url,
    content_type: Option<&str>,
) -> Option<&'static str> {
    let url_format = url
        .query_pairs()
        .find(|(key, _)| key == "format")
        .and_then(|(_, format)| image_extension(&format));
    let content_format = content_type.map(|value| image_extension(value.split(';').next()?.trim()));
    match (url_format, content_format) {
        (_, Some(None)) => None,
        (Some(expected), Some(Some(actual))) if expected != actual => None,
        (Some(expected), _) => Some(expected),
        (None, Some(Some(actual))) => Some(actual),
        (None, None) => image_extension(url.path().rsplit('.').next()?),
    }
}
