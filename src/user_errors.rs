use crate::FailureDiagnostic;
use teloxide::{ApiError, RequestError};

pub(crate) fn http_detail(status: u16) -> &'static str {
    match status {
        401 => "Authentication required",
        403 => "HTTP 403",
        404 => "HTTP 404",
        408 => "Timeout",
        429 => "HTTP 429",
        500 => "HTTP 500",
        502 => "HTTP 502",
        503 => "HTTP 503",
        504 => "HTTP 504",
        501 | 505..=599 => "HTTP 5xx",
        _ => "HTTP error",
    }
}

pub(crate) fn format_mib(bytes: u64) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    if (mib - mib.round()).abs() < 0.05 {
        format!("{} МБ", mib.round() as u64)
    } else {
        format!("{mib:.1} МБ")
    }
}

pub(crate) fn diagnostic_message(diagnostic: FailureDiagnostic, max_bytes: u64) -> String {
    if diagnostic.stage == "delivery" {
        return match diagnostic.detail {
            "Rate limit" | "HTTP 429" => {
                "Telegram временно ограничил отправку медиа (429). Попробуйте позже."
            }
            "File too large" | "Size limit" => {
                return format!(
                    "Telegram отклонил медиа: размер файла превышает лимит Bot API ({}, {max_bytes} байт).",
                    format_mib(max_bytes)
                );
            }
            "Insufficient rights" => "У бота недостаточно прав для отправки медиа в этот чат.",
            "Bot blocked" => "Бот заблокирован пользователем и не может отправить медиа.",
            "Bot kicked" => "Бот удалён из чата и не может отправить медиа.",
            "Invalid file ID" => "Telegram отклонил идентификатор сохранённого файла (file_id).",
            "Media format rejected" => {
                "Telegram не смог обработать формат или параметры медиафайла (IMAGE_PROCESS_FAILED / повреждённый контейнер)."
            }
            "Caption formatting error" => {
                "Telegram отклонил разметку подписи к медиа (ошибка HTML-тегов или длины подписи)."
            }
            "Reply message not found" => {
                "Telegram отклонил отправку медиа: исходное сообщение или ветка были удалены."
            }
            "Voice messages forbidden" => {
                "Telegram отклонил отправку аудио/голоса: у получателя запрещены голосовые сообщения."
            }
            "Timeout" => "Истекло время ожидания ответа Telegram; доставка медиа не подтверждена.",
            "Network error" => {
                "Сетевая ошибка при связи с Telegram; доставка медиа не подтверждена."
            }
            "Invalid response" => {
                "Telegram вернул некорректный ответ; доставка медиа не подтверждена."
            }
            "File I/O error" => "Не удалось прочитать файл для отправки в Telegram.",
            "Chat migrated" => "Чат перенесён в супергруппу; отправка в прежний чат невозможна.",
            "Chat unavailable" => "Чат недоступен для отправки медиа.",
            "Bot authorization failed" => "Telegram отклонил авторизацию бота.",
            _ => "Telegram отклонил отправку медиа (неизвестный код ответа Bot API; подробности в логе).",
        }
        .to_owned();
    }

    let stage = diagnostic.stage;
    let message = match diagnostic.detail {
        "HTTP 403" => "Сайт отказал в доступе к публикации или медиа (HTTP 403: антибот-защита или закрытый доступ).",
        "HTTP 404" => "Публикация или медиа не найдены на сайте (HTTP 404: пост удалён или ссылка неверна).",
        "HTTP 429" => "Сайт временно ограничил частоту запросов (HTTP 429: rate limit). Попробуйте позже.",
        "HTTP 500" => "На стороне сайта произошла ошибка сервера (HTTP 500). Попробуйте позже.",
        "HTTP 502" => "Сайт временно недоступен из-за ошибки шлюза (HTTP 502). Попробуйте позже.",
        "HTTP 503" => "Сайт временно недоступен (HTTP 503). Попробуйте позже.",
        "HTTP 504" => "Сервер сайта не дождался ответа (HTTP 504). Попробуйте позже.",
        "HTTP 5xx" => {
            "На стороне сайта произошла временная ошибка сервера (HTTP 5xx). Попробуйте позже."
        }
        "HTTP error" => "Сайт вернул ошибку HTTP; точная причина неизвестна.",
        "Private video" => "Публикация закрытая или приватная; сайт не предоставил доступ к медиа.",
        "Video unavailable" => "Публикация или её медиа недоступны на сайте (удалено автором или скрыто).",
        "Region blocked" => "Публикация недоступна в регионе сервера бота (геоблок).",
        "Members only" => "Публикация доступна только платным подписчикам канала (Members only).",
        "Video filtered" => {
            let mins = crate::config::config().media.max_video_duration_secs / 60;
            return format!(
                "Видео слишком длинное (лимит {mins} мин) или это активный/предстоящий стрим; такие ролики не скачиваются."
            );
        }
        "Format unavailable" => "Сайт не выдал подходящий видеопоток со звуком (Requested format is not available); попробуйте ещё раз.",
        "Download interrupted" => {
            "Соединение с сайтом прервалось во время скачивания потока; попробуйте ещё раз."
        }
        "Processing failed" => "Не удалось перекодировать или собрать медиафайл через ffmpeg.",
        "Site access restricted" => {
            "Сайт ограничил доступ с IP сервера к этой публикации и не отдал медиа."
        }
        "Audience restricted" => "Сайт ограничил аудиторию этой публикации (18+ / ограничение аккаунта) и не отдал медиа.",
        "Authentication required" => "Сайт требует авторизацию (вход в аккаунт / капчу) для доступа к публикации.",
        "Age verification required" => "Сайт требует подтверждения возраста (18+) в аккаунте для доступа к медиа.",
        "Cookies expired" => "Сессия авторизации на сайте истекла: требуется обновить cookies.",
        "Cookies unavailable" => "Не удалось подготовить cookies для авторизации на сайте.",
        "Timeout" => {
            return format!("Превышено время ожидания загрузки медиа с сайта (этап: {stage}).");
        }
        "Network error" => {
            return format!("Сетевая ошибка при загрузке медиа с сайта (этап: {stage}).");
        }
        "File I/O error" => {
            "Не удалось прочитать или сохранить файлы загрузки во временном хранилище."
        }
        "Empty photo" => "CDN сайта вернул пустой файл фото (0 байт).",
        "Temporary storage limit" => {
            "Превышен лимит временного хранилища для одной загрузки медиа."
        }
        "Size limit" | "File too large" => {
            return format!(
                "Размер медиа превышает допустимый лимит Telegram Bot API: {} ({max_bytes} байт).",
                format_mib(max_bytes)
            );
        }
        "Unsupported URL" => "Этот формат ссылки или раздел сайта не поддерживается загрузчиком.",
        "Unsupported format" => "Формат медиафайла в публикации не поддерживается для отправки.",
        "Extractor failed" => {
            "Загрузчик (yt-dlp) не смог разобрать страницу публикации (изменилась вёрстка или API сайта); точная причина неизвестна."
        }
        "Webpage unavailable" => {
            "Загрузчик не смог загрузить HTML-страницу публикации; точная причина неизвестна."
        }
        "API unavailable" => {
            "Загрузчик не смог получить ответ от внутреннего API сайта; точная причина неизвестна."
        }
        "No sendable media" | "Media not found" => {
            "В публикации не найдено медиа для отправки (пост содержит только текст или неподдерживаемые вложения)."
        }
        "Metadata failed" => {
            "Не удалось получить JSON-метаданные публикации через yt-dlp; точная причина неизвестна."
        }
        "Photo metadata failed" => {
            "Не удалось извлечь список фото со страницы публикации; точная причина неизвестна."
        }
        "Photo download failed" => "Не удалось скачать файл фото с CDN сайта; точная причина неизвестна.",
        "Launch failed" => "Не удалось запустить процесс загрузчика медиа (yt-dlp).",
        "Attempt failed" => {
            "Процесс загрузчика медиа завершился с ошибкой при повторной попытке; точная причина неизвестна."
        }
        "Download failed" => {
            let code = diagnostic
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "неизвестен".into());
            return format!(
                "Загрузчик ({stage}) не смог скачать медиа (код выхода {code}); причина не распознана."
            );
        }
        "Preparation failed" => "Не удалось подготовить рабочий каталог для повторной загрузки медиа.",
        "Resolution failed" => "Не удалось раскрыть короткую ссылку (редирект не привёл на поддерживаемый пост); точная причина неизвестна.",
        "Media skipped" => return format!("Некоторые медиафайлы в публикации пропущены (неподдерживаемый формат или превышен лимит в {} файлов).", crate::config::config().media.max_files),
        _ => "Не удалось загрузить медиа; причина неизвестна.",
    };
    message.to_owned()
}

pub(crate) fn request_diagnostic(error: &RequestError) -> FailureDiagnostic {
    let detail = match error {
        RequestError::Api(error) => api_detail(error),
        RequestError::RetryAfter(_) => "Rate limit",
        RequestError::Network(error) if error.is_timeout() => "Timeout",
        RequestError::Network(_) => "Network error",
        RequestError::InvalidJson { .. } => "Invalid response",
        RequestError::Io(_) => "File I/O error",
        RequestError::MigrateToChatId(_) => "Chat migrated",
    };
    FailureDiagnostic::new("delivery", detail)
}

pub(crate) fn request_message(error: &RequestError, max_bytes: u64) -> String {
    if let RequestError::RetryAfter(seconds) = error {
        return format!(
            "Telegram временно ограничил отправку медиа (429). Повторите попытку через {} сек.",
            seconds.seconds(),
        );
    }
    diagnostic_message(request_diagnostic(error), max_bytes)
}

pub(crate) fn invalid_cached_file(error: &RequestError) -> bool {
    // Only teloxide's definitive typed ID errors permit a fresh-download restart.
    // An unrecognised API rejection may be ambiguous even if it mentions file IDs.
    matches!(
        error,
        RequestError::Api(
            ApiError::WrongFileId | ApiError::WrongFileIdOrUrl | ApiError::FileIdInvalid
        )
    )
}

pub(crate) fn rejected_media_upload(error: &RequestError) -> bool {
    match error {
        RequestError::Api(ApiError::ImageProcessFailed) => true,
        RequestError::Api(ApiError::Unknown(description)) => format_rejection(description),
        RequestError::Api(_)
        | RequestError::MigrateToChatId(_)
        | RequestError::RetryAfter(_)
        | RequestError::Network(_)
        | RequestError::InvalidJson { .. }
        | RequestError::Io(_) => false,
    }
}

fn format_rejection(description: &str) -> bool {
    [
        "Bad Request: invalid media content type",
        "Bad Request: IMAGE_PROCESS_FAILED",
        "Bad Request: PHOTO_INVALID_DIMENSIONS",
        "Bad Request: PHOTO_SAVE_FILE_INVALID",
        "Bad Request: PHOTO_EXT_INVALID",
        "Bad Request: VIDEO_FILE_INVALID",
        "Bad Request: AUDIO_FILE_INVALID",
    ]
    .iter()
    .any(|known| description.eq_ignore_ascii_case(known))
}

fn api_detail(error: &ApiError) -> &'static str {
    if matches!(
        error,
        ApiError::WrongFileId | ApiError::WrongFileIdOrUrl | ApiError::FileIdInvalid
    ) {
        return "Invalid file ID";
    }
    if matches!(error, ApiError::RequestEntityTooLarge) {
        return "File too large";
    }
    if matches!(error, ApiError::ImageProcessFailed) {
        return "Media format rejected";
    }
    if matches!(
        error,
        ApiError::NotEnoughRightsToPostMessages
            | ApiError::NotEnoughRightsToPinMessage
            | ApiError::NotEnoughRightsToManagePins
            | ApiError::NotEnoughRightsToChangeChatPermissions
            | ApiError::NotEnoughRightsToRestrict
    ) {
        return "Insufficient rights";
    }
    if matches!(error, ApiError::BotBlocked) {
        return "Bot blocked";
    }
    if matches!(
        error,
        ApiError::BotKicked | ApiError::BotKickedFromSupergroup | ApiError::BotKickedFromChannel
    ) {
        return "Bot kicked";
    }
    if matches!(error, ApiError::ChatNotFound) {
        return "Chat unavailable";
    }
    if matches!(error, ApiError::InvalidToken) {
        return "Bot authorization failed";
    }
    if let ApiError::Unknown(description) = error {
        return if format_rejection(description) {
            "Media format rejected"
        } else {
            unknown_api_detail(description)
        };
    }
    "Unknown Telegram rejection"
}

fn unknown_api_detail(description: &str) -> &'static str {
    // Never format, store, or forward the API description: it may contain URLs,
    // tokens, cookies, or raw JSON. Match only canonical leading cause phrases.
    let description = ["Bad Request:", "Forbidden:", "Unauthorized:"]
        .iter()
        .find_map(|prefix| {
            description
                .get(..prefix.len())
                .filter(|leading| leading.eq_ignore_ascii_case(prefix))
                .map(|_| {
                    description
                        .get(prefix.len()..)
                        .unwrap_or_default()
                        .trim_start()
                })
        })
        .unwrap_or(description);

    for (phrase, detail) in [
        ("not enough rights", "Insufficient rights"),
        ("need administrator rights", "Insufficient rights"),
        ("have no rights to send a message", "Insufficient rights"),
        ("CHAT_WRITE_FORBIDDEN", "Insufficient rights"),
        ("CHAT_SEND_MEDIA_FORBIDDEN", "Insufficient rights"),
        ("CHAT_SEND_PHOTOS_FORBIDDEN", "Insufficient rights"),
        ("CHAT_SEND_VIDEOS_FORBIDDEN", "Insufficient rights"),
        ("CHAT_SEND_AUDIOS_FORBIDDEN", "Insufficient rights"),
        ("CHAT_SEND_DOCS_FORBIDDEN", "Insufficient rights"),
        ("bot was blocked by the user", "Bot blocked"),
        ("bot was kicked from a chat", "Bot kicked"),
        ("bot was kicked from the supergroup chat", "Bot kicked"),
        ("bot was kicked from the channel chat", "Bot kicked"),
        (
            "wrong file identifier/HTTP URL specified",
            "Invalid file ID",
        ),
        ("wrong file id", "Invalid file ID"),
        ("invalid file id", "Invalid file ID"),
        ("wrong remote file identifier specified", "Invalid file ID"),
        ("file is too big", "File too large"),
        ("file is too large", "File too large"),
        ("Request Entity Too Large", "File too large"),
        ("MEDIA_CAPTION_TOO_LONG", "Caption formatting error"),
        ("caption is too long", "Caption formatting error"),
        ("can't parse entities", "Caption formatting error"),
        ("message to be replied not found", "Reply message not found"),
        ("replied message not found", "Reply message not found"),
        ("TOPIC_CLOSED", "Insufficient rights"),
        ("TOPIC_DELETED", "Reply message not found"),
        ("VOICE_MESSAGES_FORBIDDEN", "Voice messages forbidden"),
        ("WEBPAGE_CURL_FAILED", "Media format rejected"),
        ("WEBPAGE_MEDIA_EMPTY", "Media format rejected"),
        ("failed to get HTTP URL content", "Media format rejected"),
    ] {
        if description
            .get(..phrase.len())
            .is_some_and(|leading| leading.eq_ignore_ascii_case(phrase))
            && description
                .get(phrase.len()..)
                .and_then(|tail| tail.chars().next())
                .is_none_or(|next| next.is_ascii_whitespace() || matches!(next, ':' | '.' | ';'))
        {
            return detail;
        }
    }
    tracing::warn!("Unmapped Telegram API media error: {description}");
    "Unknown Telegram rejection"
}
