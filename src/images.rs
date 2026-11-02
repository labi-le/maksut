use crate::*;

pub(crate) fn tiktok_photo_id(url: &Url) -> Option<&str> {
    if !matches!(
        url.host_str(),
        Some("tiktok.com" | "www.tiktok.com" | "m.tiktok.com")
    ) {
        return None;
    }
    let mut parts = url.path().strip_prefix('/')?.split('/');
    let user = parts.next()?.strip_prefix('@')?;
    if user.is_empty() || user.contains('%') || parts.next()? != "photo" {
        return None;
    }
    let id = parts.next()?;
    (parts.next().is_none() && !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(id)
}

pub(crate) fn image_source(url: &Url) -> Option<&'static str> {
    match url.host_str()? {
        "pin.it"
        | "pinterest.com"
        | "www.pinterest.com"
        | "pinterest.ca"
        | "www.pinterest.ca"
        | "pinterest.co.uk"
        | "www.pinterest.co.uk" => Some("pinterest"),
        "instagram.com" | "www.instagram.com" => Some("instagram"),
        "x.com" | "www.x.com" | "twitter.com" | "www.twitter.com" | "mobile.twitter.com" => {
            Some("twitter")
        }
        "tiktok.com" | "www.tiktok.com" | "m.tiktok.com" if tiktok_photo_id(url).is_some() => {
            Some("tiktok")
        }
        _ => None,
    }
}

pub(crate) fn image_cdn_url(raw: &str, source: &str) -> Option<Url> {
    // Url normalizes explicit :443 away; inspect the original authority too.
    let authority = raw
        .strip_prefix("https://")?
        .split(['/', '?', '#'])
        .next()?;
    if authority.contains('@') || authority.contains(':') {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let trusted = match source {
        "pinterest" => host == "pinimg.com" || host.ends_with(".pinimg.com"),
        "instagram" => ["cdninstagram.com", "fbcdn.net"].iter().any(|domain| {
            host == *domain
                || host
                    .strip_suffix(domain)
                    .is_some_and(|prefix| prefix.ends_with('.'))
        }),
        "twitter" => host == "pbs.twimg.com" && url.path().starts_with("/media/"),
        "tiktok" => ["tiktokcdn.com", "tiktokcdn-us.com", "tiktokcdn-eu.com"]
            .iter()
            .any(|domain| {
                host.strip_suffix(domain)
                    .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1)
                    && url
                        .query_pairs()
                        .any(|(key, value)| key == "x-signature" && !value.is_empty())
            }),
        _ => false,
    };
    trusted.then_some(url)
}

// 198.18.0.0/15 is the transparent proxy's fake-IP range, not a local network.
pub(crate) fn permitted_ipv4(ip: IpAddr) -> bool {
    let IpAddr::V4(ip) = ip else { return false };
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && (b == 0 || b == 168))
        || (a == 198 && (b == 51 && c == 100))
        || (a == 203 && b == 0 && c == 113))
}

pub(crate) struct ImageMetadata {
    pub(crate) images: Vec<Url>,
    pub(crate) description: Option<String>,
    pub(crate) has_video: bool,
    pub(crate) skipped: usize,
    pub(crate) alternates: Vec<Vec<Url>>,
}

pub(crate) fn collect_images(info: &Value, source: &str) -> ImageMetadata {
    let mut result = ImageMetadata {
        images: Vec::new(),
        has_video: false,
        skipped: 0,
        alternates: Vec::new(),
        description: None,
    };
    if source == "tiktok" {
        result.description = info
            .get("__DEFAULT_SCOPE__")
            .and_then(|scope| scope.get("webapp.video-detail"))
            .and_then(|detail| detail.get("itemInfo"))
            .and_then(|item| item.get("itemStruct"))
            .and_then(|item| item.get("desc"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        let images = info
            .get("__DEFAULT_SCOPE__")
            .and_then(|scope| scope.get("webapp.video-detail"))
            .and_then(|detail| detail.get("itemInfo"))
            .and_then(|item| item.get("itemStruct"))
            .and_then(|item| item.get("imagePost"))
            .and_then(|post| post.get("images"))
            .and_then(Value::as_array);
        if let Some(images) = images {
            for image in images.iter().take(config().media.max_tiktok_photos) {
                let mut candidates = image
                    .get("imageURL")
                    .and_then(|image| image.get("urlList"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter_map(|raw| image_cdn_url(raw, source));
                if let Some(url) = candidates.next() {
                    result.images.push(url);
                    result.alternates.push(candidates.collect());
                } else {
                    result.skipped += 1;
                }
            }
            result.skipped += images
                .len()
                .saturating_sub(config().media.max_tiktok_photos);
        }
        return result;
    }
    let items: &[Value] = info
        .get("entries")
        .and_then(Value::as_array)
        .map_or(std::slice::from_ref(info), Vec::as_slice);
    result.description = caption_text(
        info.get("title").and_then(Value::as_str),
        info.get("description")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .or_else(|| {
                items.iter().find_map(|item| {
                    item.get("description")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                })
            }),
    );
    for item in items.iter().take(config().media.max_files) {
        if item.is_null() {
            result.skipped += 1;
            continue;
        }
        let formats = item
            .get("formats")
            .and_then(Value::as_array)
            .is_some_and(|formats| !formats.is_empty());
        let video_url = item
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|url| !url.is_empty())
            && !matches!(
                item.get("ext").and_then(Value::as_str),
                Some("jpg" | "jpeg" | "png" | "webp")
            );
        if formats || video_url {
            result.has_video = true;
            continue;
        }
        let best = item
            .get("thumbnails")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|thumbnail| {
                let url = image_cdn_url(thumbnail.get("url")?.as_str()?, source)?;
                let width = thumbnail.get("width").and_then(Value::as_u64).unwrap_or(0);
                let height = thumbnail.get("height").and_then(Value::as_u64).unwrap_or(0);
                Some((width.saturating_mul(height), url))
            })
            .max_by_key(|(size, _)| *size);
        if let Some((_, url)) = best {
            result.images.push(url);
        } else {
            result.skipped += 1;
        }
    }
    result
}

pub(crate) fn tiktok_rehydration(html: &str) -> Option<Value> {
    let mut remaining = html;
    while let Some(start) = remaining.find("<script") {
        remaining = remaining.get(start + "<script".len()..)?;
        let tag_end = remaining.find('>')?;
        let tag = remaining.get(..tag_end)?;
        let body = remaining.get(tag_end + 1..)?;
        let end = body.find("</script>")?;
        if tag.split_ascii_whitespace().any(|attribute| {
            matches!(
                attribute,
                "id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\""
                    | "id='__UNIVERSAL_DATA_FOR_REHYDRATION__'"
            )
        }) {
            return serde_json::from_str(body.get(..end)?).ok();
        }
        remaining = body.get(end + "</script>".len()..)?;
    }
    None
}

pub(crate) async fn extract_tiktok_images(
    url: &Url,
    cfg: &Settings,
    deadline: time::Instant,
) -> Result<ImageMetadata, DownloadFailure> {
    let invalid = || {
        DownloadFailure::new(
            "Некорректная ссылка на фото TikTok".into(),
            "photo-page",
            "Photo metadata failed",
        )
    };
    let id = tiktok_photo_id(url).ok_or_else(invalid)?;
    let mut page = url.clone();
    page.set_scheme("https").map_err(|_| invalid())?;
    page.set_host(Some("www.tiktok.com"))
        .map_err(|_| invalid())?;
    page.set_path(&url.path().replacen("/photo/", "/video/", 1));
    page.set_query(None);
    let cookies = tiktok_page_cookies_async(cfg.cookies_file.as_deref(), &page)
        .await
        .map_err(|reason| DownloadFailure::new(reason, "photo-page", "Cookies unavailable"))?;
    let failure = |reason: &str, detail: &'static str| {
        DownloadFailure::new(reason.to_owned(), "photo-page", detail)
    };
    let info = time::timeout_at(deadline, async {
        let client = pinned_client(&page).await.map_err(|reason| {
            DownloadFailure::new(reason, "photo-page", "Photo metadata failed")
        })?;
        let mut request = client
            .get(page)
            .header(reqwest::header::USER_AGENT, TIKTOK_USER_AGENT);
        if let Some(cookies) = cookies {
            request = request.header(reqwest::header::COOKIE, cookies);
        }
        let mut response = request.send().await.map_err(|error| {
            failure(
                "Не удалось загрузить страницу TikTok",
                if error.is_timeout() {
                    "Timeout"
                } else {
                    "Photo metadata failed"
                },
            )
        })?;
        if !response.status().is_success() {
            return Err(failure(
                "Страница TikTok вернула ошибку или перенаправление",
                user_errors::http_detail(response.status().as_u16()),
            ));
        }
        if response
            .content_length()
            .is_some_and(|size| size > config().media.max_metadata_bytes)
        {
            return Err(failure(
                "Превышен лимит страницы TikTok",
                "Photo metadata failed",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            failure(
                "Ошибка чтения страницы TikTok",
                if error.is_timeout() {
                    "Timeout"
                } else {
                    "Photo metadata failed"
                },
            )
        })? {
            if chunk.len() as u64
                > config()
                    .media
                    .max_metadata_bytes
                    .saturating_sub(bytes.len() as u64)
            {
                return Err(failure(
                    "Превышен лимит страницы TikTok",
                    "Photo metadata failed",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let html = std::str::from_utf8(&bytes)
            .map_err(|_| failure("Некорректная страница TikTok", "Photo metadata failed"))?;
        tiktok_rehydration(html)
            .ok_or_else(|| failure("Нет метаданных фото TikTok", "Photo metadata failed"))
    })
    .await
    .map_err(|_| failure("Время извлечения фото TikTok истекло", "Timeout"))??;
    let detail = info
        .get("__DEFAULT_SCOPE__")
        .and_then(|scope| scope.get("webapp.video-detail"))
        .ok_or_else(|| failure("Нет информации о фото TikTok", "Photo metadata failed"))?;
    if detail.get("statusCode").and_then(Value::as_i64) != Some(0)
        || detail
            .get("itemInfo")
            .and_then(|item| item.get("itemStruct"))
            .and_then(|item| item.get("id"))
            .and_then(Value::as_str)
            != Some(id)
    {
        return Err(failure(
            "Некорректные метаданные фото TikTok",
            "Photo metadata failed",
        ));
    }
    let images = collect_images(&info, "tiktok");
    if images.images.is_empty() {
        return Err(failure("Не найдены фото TikTok", "Media not found"));
    }
    Ok(images)
}
pub(crate) async fn extract_images(
    url: &Url,
    cfg: &Settings,
    source: &str,
    dir: &Path,
    duration: Duration,
) -> Result<ImageMetadata, DownloadFailure> {
    let failure = |reason: &str, detail: &'static str| {
        DownloadFailure::new(reason.to_owned(), "metadata", detail)
    };
    let cookie = process_cookies_async(cfg.cookies_file.as_deref(), dir)
        .await
        .map_err(|reason| DownloadFailure::new(reason, "metadata", "Cookies unavailable"))?;
    let deadline = time::Instant::now() + duration;
    let mut cmd = Command::new(&cfg.ytdlp_bin);
    cmd.arg("--quiet")
        .arg("--no-warnings")
        .arg("--skip-download")
        .arg("--ignore-no-formats-error")
        .arg("--dump-single-json")
        .arg("--playlist-end")
        .arg(config().media.max_files.to_string())
        .arg("--max-filesize")
        .arg(cfg.max_media_bytes.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some((_, path)) = &cookie {
        cmd.arg("--cookies").arg(path);
    }
    cmd.arg("--").arg(url.as_str());
    #[cfg(unix)]
    cmd.as_std_mut().process_group(0);
    let child = cmd.spawn().map_err(|_| {
        failure(
            "Не удалось запустить yt-dlp для метаданных",
            "Launch failed",
        )
    })?;
    let mut process = DownloadProcess::new(child).map_err(|_| {
        failure(
            "Не удалось запустить yt-dlp для метаданных",
            "Launch failed",
        )
    })?;
    let mut stdout = process
        .take_stdout(config().media.max_metadata_bytes + 1)
        .map_err(|_| {
            failure(
                "Не удалось запустить yt-dlp для метаданных",
                "Launch failed",
            )
        })?;
    let (bytes, status) = time::timeout_at(deadline, async {
        let mut bytes = Vec::new();
        stdout
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| failure("Ошибка чтения метаданных", "Metadata failed"))?;
        if bytes.len() as u64 > config().media.max_metadata_bytes {
            return Err(failure("Превышен лимит JSON метаданных", "Metadata failed"));
        }
        let status = process
            .wait()
            .await
            .map_err(|_| failure("Ошибка процесса yt-dlp", "Attempt failed"))?;
        Ok::<_, DownloadFailure>((bytes, status))
    })
    .await
    .map_err(|_| failure("Время извлечения метаданных истекло", "Timeout"))??;
    let label = process.stderr_label(deadline).await;
    if !status.success() {
        let mut failure = failure("yt-dlp не смог извлечь метаданные публикации", label);
        failure.diagnostic.exit_code = status.code();
        return Err(failure);
    }
    let info: Value = serde_json::from_slice(&bytes).map_err(|_| {
        failure(
            "yt-dlp вернул некорректные JSON метаданные",
            "Metadata failed",
        )
    })?;
    Ok(collect_images(&info, source))
}

pub(crate) fn image_extension(raw: &str) -> Option<&'static str> {
    match raw.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "image/jpeg" => Some("jpg"),
        "png" | "image/png" => Some("png"),
        "webp" | "image/webp" => Some("webp"),
        _ => None,
    }
}
