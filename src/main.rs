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

mod config;
mod config_sections;
mod config_validate;
mod config_validate_extras;
mod config_validate_media;
mod logging;
mod retry;
pub(crate) use config::*;
pub(crate) use config_sections::*;
pub(crate) use retry::*;

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
mod chat_urls;
mod reaction;
pub(crate) use reaction::*;
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
mod browser;
mod media;
mod read_url;
mod read_url_cache;
mod read_url_html;
mod read_url_sites;
mod read_url_text;
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

const TIKTOK_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0";

pub(crate) fn telegram_bot(token: String) -> Result<Bot, reqwest::Error> {
    // Proxy tunnels close idle sockets before reqwest's 90s pool default.
    // Don't use teloxide's default_reqwest_settings because it hardcodes a 17s timeout.
    let telegram = &config().telegram;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(telegram.connect_timeout_secs))
        .timeout(Duration::from_secs(telegram.request_timeout_secs))
        .pool_idle_timeout(Duration::from_secs(telegram.pool_idle_timeout_secs))
        .tcp_keepalive(Duration::from_secs(telegram.tcp_keepalive_secs))
        .tcp_nodelay(true)
        .build()?;
    Ok(Bot::with_client(token, client))
}

#[tokio::main]
async fn main() {
    let mut cfg = match Settings::load().await {
        Ok(cfg) => cfg,
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    };
    if let Err(reason) = crate::logging::init() {
        eprintln!("{reason}");
        std::process::exit(1);
    }
    let token = match env::var("BOT_TOKEN") {
        Ok(token) if !token.trim().is_empty() => token,
        _ => {
            tracing::error!("BOT_TOKEN is required");
            std::process::exit(1);
        }
    };
    cleanup_old_downloads(&cfg.download_dir);
    let bot = match telegram_bot(token) {
        Ok(bot) => bot,
        Err(_) => {
            tracing::error!("Unable to initialize Telegram HTTP client");
            std::process::exit(1);
        }
    };
    cfg.bot_username = match bot.get_me().await {
        Ok(me) => me.user.username.unwrap_or_default(),
        Err(error) => {
            tracing::error!(%error, "unable to identify bot");
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
mod browser_test;
#[cfg(test)]
mod caption_test;
#[cfg(test)]
mod chat_status_test;
#[cfg(test)]
mod chat_test;
#[cfg(test)]
mod code_inspect_test;
#[cfg(test)]
mod config_test;
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
