#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::indexing_slicing,
        clippy::string_slice,
        reason = "test scaffolding is not product code, and a failing test should abort"
    )
)]

mod prelude;
pub(crate) use prelude::*;

mod media_cache;
mod user_errors;

pub(crate) use media_cache::{
    cache_key, init_cache, invalidate_cache, lookup_cache, store_cache, CachedMedia,
};

mod chat;
pub(crate) use chat::*;
mod chat_client;
pub(crate) use chat_client::*;
mod chat_media;
pub(crate) use chat_media::*;
mod chat_status;
pub(crate) use chat_status::*;
mod chat_style;
pub(crate) use chat_style::*;
mod chat_text;
pub(crate) use chat_text::*;
mod chat_tools;
pub(crate) use chat_tools::*;
mod research;
pub(crate) use research::*;
mod search;
mod search_parse;
mod search_providers;
pub(crate) use search::*;
mod tool_registry;
pub(crate) use tool_registry::*;
mod memory;
pub(crate) use memory::*;
mod code_inspect;
mod improve_commands;
mod improve_store;
mod improve_views;
mod memory_guard;
mod self_improve;
mod settings;
pub(crate) use settings::*;
mod cookies;
pub(crate) use cookies::*;
mod database;
pub(crate) use database::*;
mod diagnostics;
pub(crate) use diagnostics::*;
mod links;
pub(crate) use links::*;
mod media;
mod read_url;
mod read_url_html;
mod read_url_sites;
pub(crate) use media::*;
mod process;
pub(crate) use process::*;
mod images;
pub(crate) use images::*;
mod twitter_images;
pub(crate) use twitter_images::*;
mod image_fetch;
pub(crate) use image_fetch::*;
mod youtube;
pub(crate) use youtube::*;
mod download;
pub(crate) use download::*;
mod cookie_refresh;
pub(crate) use cookie_refresh::*;
mod stickers;
pub(crate) use stickers::*;
mod download_videos;
pub(crate) use download_videos::*;
mod caption;
pub(crate) use caption::*;
mod video_convert;
pub(crate) use video_convert::*;
mod delivery;
pub(crate) use delivery::*;
mod source_message;
pub(crate) use source_message::*;
mod handler;
pub(crate) use handler::*;
mod cleanup;
pub(crate) use cleanup::*;

const MAX_FILES: usize = 10;
// TikTok photo posts can carry up to 35 images; Telegram albums are split at 10.
const MAX_TIKTOK_PHOTOS: usize = 35;
// Three pending or active downloads allow short bursts without one user filling the queue.
const USER_DOWNLOAD_LIMIT: usize = 3;
const TELEGRAM_FILE_LIMIT: u64 = 50_000_000;
const DEFAULT_MEDIA_LIMIT: u64 = 45 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
// Long videos and live streams are rejected before download; the 180s
// download budget is for normal clips, not hours-long broadcasts.
pub(crate) const MAX_VIDEO_DURATION: u64 = 30 * 60;
const TELEGRAM_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
pub(crate) const TELEGRAM_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const TELEGRAM_TCP_KEEPALIVE: Duration = Duration::from_secs(15);
const COOKIE_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
const YOUTUBE_PROBE_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;
// Reserve room for MP4 muxing; a stream-size estimate alone is not a sendable file size.
const MP4_MUX_HEADROOM: u64 = 64 * 1024;
const MAX_TWITTER_JSON_BYTES: u64 = 16 * 1024;
const MAX_VIDEO_DIMENSION: u32 = 3840;
const MAX_VIDEO_PIXELS: u32 = 3840 * 2160;
const TIKTOK_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0";

pub(crate) fn telegram_bot(token: String) -> Result<Bot, reqwest::Error> {
    // Proxy tunnels close idle sockets before reqwest's 90s pool default.
    // Don't use teloxide's default_reqwest_settings because it hardcodes a 17s timeout.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(TELEGRAM_REQUEST_TIMEOUT)
        .pool_idle_timeout(TELEGRAM_POOL_IDLE_TIMEOUT)
        .tcp_keepalive(TELEGRAM_TCP_KEEPALIVE)
        .tcp_nodelay(true)
        .build()?;
    Ok(Bot::with_client(token, client))
}

#[tokio::main]
async fn main() {
    let token = match env::var("BOT_TOKEN") {
        Ok(token) if !token.trim().is_empty() => token,
        _ => {
            eprintln!("BOT_TOKEN is required");
            std::process::exit(1);
        }
    };
    let mut cfg = match Settings::from_env().await {
        Ok(cfg) => cfg,
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    };
    cleanup_old_downloads(&cfg.download_dir);
    let bot = match telegram_bot(token) {
        Ok(bot) => bot,
        Err(_) => {
            eprintln!("Unable to initialize Telegram HTTP client");
            std::process::exit(1);
        }
    };
    cfg.bot_username = match bot.get_me().await {
        Ok(me) => me.user.username.unwrap_or_default(),
        Err(error) => {
            eprintln!("Unable to identify bot: {error}");
            std::process::exit(1);
        }
    };
    cfg.sticker_replies = load_sticker_replies(&bot).await;
    let cfg = Arc::new(cfg);
    let catalog_cfg = Arc::clone(&cfg);
    let catalog_bot = bot.clone();
    tokio::spawn(async move {
        let catalog = load_sticker_catalog(&catalog_bot).await;
        *catalog_cfg.sticker_catalog.write().await = catalog;
    });
    tokio::spawn(memory_compactor(Arc::clone(&cfg)));
    tokio::spawn(crate::self_improve::improvement_notifier(
        bot.clone(),
        Arc::clone(&cfg),
    ));
    let handler = teloxide::dptree::entry()
        .branch(teloxide::types::Update::filter_message().endpoint(message_endpoint))
        .branch(teloxide::types::Update::filter_callback_query().endpoint(callback_endpoint));
    teloxide::dispatching::Dispatcher::builder(bot, handler)
        // Distribute by nothing (rather than chat id): a slow download in one
        // chat must not serialize unrelated messages or conversations.
        .distribution_function(|_| None::<i64>)
        .dependencies(teloxide::dptree::deps![cfg])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

async fn message_endpoint(bot: Bot, message: Message, cfg: Arc<Settings>) -> ResponseResult<()> {
    handle_message(bot, message, cfg).await
}

async fn callback_endpoint(
    bot: Bot,
    query: teloxide::types::CallbackQuery,
    cfg: Arc<Settings>,
) -> ResponseResult<()> {
    handle_callback_query(bot, query, cfg).await
}

#[cfg(test)]
mod caption_test;
#[cfg(test)]
mod chat_status_test;
#[cfg(test)]
mod chat_test;
#[cfg(test)]
mod code_inspect_test;
#[cfg(test)]
mod cookie_refresh_test;
#[cfg(test)]
mod cookies_test;
#[cfg(test)]
mod database_test;
#[cfg(test)]
mod delivery_test;
#[cfg(test)]
mod diagnostics_test;
#[cfg(test)]
mod download_test;
#[cfg(test)]
mod download_videos_test;
#[cfg(test)]
mod feature_support;
#[cfg(test)]
mod handler_test;
#[cfg(test)]
mod ignore_window_test;
#[cfg(test)]
mod image_fetch_test;
#[cfg(test)]
mod images_test;
#[cfg(test)]
mod links_test;
#[cfg(test)]
mod media_cache_test;
#[cfg(test)]
mod media_test;
#[cfg(test)]
mod memory_test;
#[cfg(test)]
mod process_test;
#[cfg(test)]
mod read_url_test;
#[cfg(test)]
mod research_test;
#[cfg(test)]
mod self_improve_test;
#[cfg(test)]
mod stickers_test;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tool_registry_test;
#[cfg(test)]
mod user_errors_test;
#[cfg(test)]
mod video_convert_test;
#[cfg(test)]
mod youtube_test;
