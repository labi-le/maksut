use crate::prelude::*;
use crate::read_url::truncate_chars;

pub(crate) fn rewrite_telegram_url(url: &Url, path: &str) -> Option<Url> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [channel] if is_telegram_channel_ident(channel) => {
            let mut out = Url::parse(&format!("https://t.me/s/{channel}")).ok()?;
            out.set_query(url.query());
            Some(out)
        }
        [channel, post_id]
            if is_telegram_channel_ident(channel)
                && !post_id.is_empty()
                && post_id.chars().all(|c| c.is_ascii_digit()) =>
        {
            Url::parse(&format!("https://t.me/s/{channel}/{post_id}")).ok()
        }
        _ => None,
    }
}

fn is_telegram_channel_ident(seg: &str) -> bool {
    seg.len() >= 4
        && !matches!(
            seg,
            "s" | "c"
                | "joinchat"
                | "addstickers"
                | "addemoji"
                | "proxy"
                | "socks"
                | "share"
                | "iv"
                | "login"
                | "dl"
        )
        && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(crate) fn extract_telegram_channel(
    document: &dom_query::Document,
    url: &Url,
) -> Option<(Option<String>, String)> {
    let host = url.host_str()?.to_ascii_lowercase();
    if !matches!(
        host.as_str(),
        "t.me" | "www.t.me" | "telegram.me" | "www.telegram.me"
    ) {
        return None;
    }
    let posts = document.select("div.tgme_widget_message[data-post]");
    if posts.is_empty() {
        return None;
    }
    let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    let target_post_id = match segments.as_slice() {
        ["s", _, id] | [_, id] if id.chars().all(|c| c.is_ascii_digit()) => Some(*id),
        _ => None,
    };
    let ch_title = crate::search::clean_text(
        &document
            .select_single(".tgme_channel_info_header_title")
            .text(),
    );
    let ch_user = crate::search::clean_text(
        &document
            .select_single(".tgme_channel_info_header_username")
            .text(),
    );
    let ch_desc = crate::search::clean_text(
        &document
            .select_single(".tgme_channel_info_description")
            .text(),
    );
    let mut out = String::new();
    if !ch_title.is_empty() || !ch_user.is_empty() {
        out.push_str(&format!("# {ch_title} {ch_user}\n"));
    }
    if !ch_desc.is_empty() {
        out.push_str(&format!("{ch_desc}\n"));
    }
    let mut target_block = None;
    let mut other_blocks = Vec::new();
    for post in posts.iter().take(25) {
        let data_post = post
            .attr("data-post")
            .map(|s| s.to_string())
            .unwrap_or_default();
        let post_id = data_post
            .rsplit_once('/')
            .map_or(data_post.as_str(), |(_, id)| id);
        let is_target = target_post_id == Some(post_id);
        let block = format_telegram_post(&post, post_id, is_target);
        if is_target {
            target_block = Some(block);
        } else {
            other_blocks.push(block);
        }
    }
    if let Some(target) = target_block {
        out.push_str(&format!("\n{target}\n"));
        if !other_blocks.is_empty() {
            out.push_str("\n---\nДругие посты в ленте канала:\n");
        }
    }
    let start = other_blocks.len().saturating_sub(10);
    for block in other_blocks.into_iter().skip(start) {
        out.push_str(&format!("\n{block}\n"));
    }
    let title = (!ch_title.is_empty()).then_some(ch_title);
    Some((title, out.trim().to_owned()))
}

fn format_telegram_post(post: &dom_query::Selection<'_>, post_id: &str, is_target: bool) -> String {
    let class = post
        .attr("class")
        .map(|s| s.to_string())
        .unwrap_or_default();
    let mut media: Vec<String> = Vec::new();
    if class.contains("sticker_media")
        || post
            .select_single(".tgme_widget_message_sticker_wrap")
            .exists()
    {
        media.push("стикер".into());
    }
    if class.contains("roundvideo_media")
        || post
            .select_single(".tgme_widget_message_roundvideo_player")
            .exists()
    {
        let dur = crate::search::clean_text(
            &post
                .select_single(".tgme_widget_message_roundvideo_duration")
                .text(),
        );
        media.push(if dur.is_empty() {
            "видеокружок".into()
        } else {
            format!("видеокружок {dur}")
        });
    } else if post
        .select_single(".tgme_widget_message_video_player")
        .exists()
    {
        let dur = crate::search::clean_text(
            &post
                .select_single(".tgme_widget_message_video_duration")
                .text(),
        );
        media.push(if dur.is_empty() {
            "видео".into()
        } else {
            format!("видео {dur}")
        });
    }
    if post
        .select_single(".tgme_widget_message_photo_wrap")
        .exists()
    {
        media.push("фото".into());
    }
    if post
        .select_single(".tgme_widget_message_voice_player")
        .exists()
    {
        media.push("голосовое".into());
    }
    if post
        .select_single(".tgme_widget_message_document_wrap")
        .exists()
    {
        let doc_title = crate::search::clean_text(
            &post
                .select_single(".tgme_widget_message_document_title")
                .text(),
        );
        media.push(if doc_title.is_empty() {
            "файл".into()
        } else {
            format!("файл: {doc_title}")
        });
    }
    let fwd = crate::search::clean_text(
        &post
            .select_single(".tgme_widget_message_forwarded_from")
            .text(),
    );
    let reply_text = crate::search::clean_text(
        &post
            .select_single(".tgme_widget_message_reply .js-message_reply_text")
            .text(),
    );
    let msg_text =
        crate::search::clean_text(&post.select_single(".js-message_text").formatted_text());
    let views = crate::search::clean_text(&post.select_single(".tgme_widget_message_views").text());
    let date = post
        .select_single(".tgme_widget_message_date time")
        .attr("datetime")
        .map(|s| s.to_string())
        .unwrap_or_default();
    let heading = if is_target {
        format!("## [ЦЕЛЕВОЙ ПОСТ #{post_id}]")
    } else {
        format!("### [Пост #{post_id}]")
    };
    let mut block = heading;
    if !date.is_empty() || !views.is_empty() {
        block.push_str(&format!(" ({date}, просмотров: {views})"));
    }
    block.push('\n');
    if !fwd.is_empty() {
        block.push_str(&format!("{fwd}\n"));
    }
    if !media.is_empty() {
        block.push_str(&format!("Медиа: [{}]\n", media.join(", ")));
    }
    if !reply_text.is_empty() {
        block.push_str(&format!(
            "> В ответ на: {}\n",
            truncate_chars(&reply_text, 240).0
        ));
    }
    if !msg_text.is_empty() {
        block.push_str(&format!("Текст: {msg_text}\n"));
    } else if !media.is_empty() {
        block.push_str("Текст: [без текста]\n");
    }
    block.trim_end().to_owned()
}
