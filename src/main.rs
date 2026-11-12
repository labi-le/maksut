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
mod log_fields;
mod log_redact;
mod logging;
mod retry;
pub(crate) use config::*;
pub(crate) use config_sections::*;
pub(crate) use log_fields::*;
pub(crate) use retry::*;

mod media_cache;
mod user_errors;

pub(crate) use media_cache::{
    cache_key, init_cache, invalidate_cache, lookup_cache, store_cache, CachedMedia,
};

mod addressing;
mod prompts;
pub(crate) use prompts::*;
mod chat;
pub(crate) use chat::*;
mod chat_delivery;
pub(crate) use chat_delivery::*;
mod completion;
pub(crate) use completion::*;
mod chat_client;
pub(crate) use chat_client::*;
mod chat_media;
pub(crate) use chat_media::*;
mod chat_doc;
mod chat_profile;
mod chat_status;
pub(crate) use chat_status::*;
mod chat_style;
pub(crate) use chat_style::*;
mod chat_text;
pub(crate) use chat_text::*;
mod chat_tools;
pub(crate) use chat_tools::*;
mod chat_urls;
mod jev;
mod reaction;
mod rich_message;
mod router;
mod voice;
pub(crate) use router::*;
mod typing;
pub(crate) use typing::*;
mod snapcompact;
mod telegraph;
pub(crate) use reaction::*;
mod research;
pub(crate) use research::*;
mod research_dossier;
mod research_registry;
pub(crate) use research_registry::*;
mod research_tools;
mod search;
mod search_parse;
mod search_providers;
pub(crate) use search::*;
mod tool_registry;
pub(crate) use tool_registry::*;
mod github_dispatch;
mod github_render;
mod github_tools;
mod memory;
pub(crate) use memory::*;
mod chat_embeddings;
mod chat_search;
mod chat_search_query;
mod qdrant;
mod qdrant_index;
mod usage;
pub(crate) use usage::*;
mod code_inspect;
mod improve_commands;
mod improve_store;
mod improve_views;
mod memory_guard;
mod self_improve;
mod self_memory;
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
mod read_url_telegram;
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
mod group_settings;
pub(crate) use group_settings::*;
mod cleanup;
pub(crate) use cleanup::*;
mod web;
mod web_api;
mod web_auth;
mod web_config;
mod web_static;

const TIKTOK_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0";

const TELEGRAM_API_HOST: &str = "api.telegram.org";
const TELEGRAM_FALLBACK_ADDRS: &[std::net::SocketAddr] = &[
    std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
        std::net::Ipv4Addr::new(149, 154, 167, 220),
        443,
    )),
    std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
        std::net::Ipv4Addr::new(149, 154, 166, 110),
        443,
    )),
];

/// When a split-tunnel resolver returns an RFC 2544 benchmark fake-IP for
/// `api.telegram.org`, keep the resolved tunnel address first (because direct
/// Telegram DC IPs are blocked when the ISP filters Telegram) and append
/// Telegram's public endpoints after it as a fallback if the local tunnel drops.
pub(crate) fn apply_telegram_dns_fallback(
    mut builder: reqwest::ClientBuilder,
) -> reqwest::ClientBuilder {
    if let Ok(resolved) = std::net::ToSocketAddrs::to_socket_addrs(&(TELEGRAM_API_HOST, 443)) {
        if let Some(addrs) = ordered_telegram_addrs(resolved) {
            builder = builder.resolve_to_addrs(TELEGRAM_API_HOST, &addrs);
        }
    }
    builder
}

pub(crate) fn ordered_telegram_addrs(
    resolved: impl IntoIterator<Item = std::net::SocketAddr>,
) -> Option<Vec<std::net::SocketAddr>> {
    let mut addrs: Vec<std::net::SocketAddr> = Vec::new();
    let mut has_fake_ip = false;
    for addr in resolved {
        if let std::net::IpAddr::V4(ip) = addr.ip() {
            let octets = ip.octets();
            if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
                has_fake_ip = true;
            }
        }
        if !addrs.contains(&addr) {
            addrs.push(addr);
        }
    }
    if !has_fake_ip {
        return None;
    }
    for &fallback in TELEGRAM_FALLBACK_ADDRS {
        if !addrs.contains(&fallback) {
            addrs.push(fallback);
        }
    }
    Some(addrs)
}

pub(crate) fn telegram_bot(token: String) -> Result<Bot, reqwest::Error> {
    // Proxy tunnels close idle sockets before reqwest's 90s pool default.
    // Don't use teloxide's default_reqwest_settings because it hardcodes a 17s timeout.
    let telegram = &config().telegram;
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(telegram.connect_timeout_secs))
        .timeout(Duration::from_secs(telegram.request_timeout_secs))
        .pool_idle_timeout(Duration::from_secs(telegram.pool_idle_timeout_secs))
        .tcp_keepalive(Duration::from_secs(telegram.tcp_keepalive_secs))
        .tcp_nodelay(true);
    let client = apply_telegram_dns_fallback(builder).build()?;
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
    let token = match env::var("BOT_TOKEN") {
        Ok(token) if !token.trim().is_empty() => token,
        _ => {
            tracing::error!("BOT_TOKEN is required");
            std::process::exit(1);
        }
    };
    cleanup_old_downloads(&cfg.download_dir);
    let bot = match telegram_bot(token.clone()) {
        Ok(bot) => bot,
        Err(_) => {
            tracing::error!("Unable to initialize Telegram HTTP client");
            std::process::exit(1);
        }
    };
    cfg.bot_username = loop {
        match bot.get_me().await {
            Ok(me) => break me.user.username.unwrap_or_default(),
            Err(teloxide::RequestError::Network(err)) => {
                tracing::warn!(
                    timeout = err.is_timeout(),
                    connect = err.is_connect(),
                    "unable to identify bot, retrying in 3s"
                );
                time::sleep(Duration::from_secs(3)).await;
            }
            Err(teloxide::RequestError::RetryAfter(wait)) => {
                time::sleep(wait.duration()).await;
            }
            Err(_) => {
                tracing::error!(kind = "request", "unable to identify bot");
                std::process::exit(1);
            }
        }
    };
    cfg.sticker_replies = load_sticker_replies(&bot).await;
    tracing::info!(
        username = %cfg.bot_username,
        owner_id = cfg.owner_id.0,
        chat_enabled = cfg.chat.is_some(),
        router = ?cfg.router.mode(),
        "bot identified"
    );
    let cfg = Arc::new(cfg);
    let catalog_cfg = Arc::clone(&cfg);
    let catalog_bot = bot.clone();
    tokio::spawn(async move {
        let catalog = load_sticker_catalog(&catalog_bot).await;
        *catalog_cfg.sticker_catalog.write().await = catalog;
    });
    tokio::spawn(memory_compactor(Arc::clone(&cfg)));
    tokio::spawn(crate::chat_embeddings::chat_indexer(Arc::clone(&cfg)));
    tokio::spawn(crate::self_improve::improvement_notifier(
        bot.clone(),
        Arc::clone(&cfg),
    ));
    tracing::info!("background tasks started");
    crate::web::start(Arc::clone(&cfg), token);
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
mod addressing_test;
#[cfg(test)]
mod browser_test;
#[cfg(test)]
mod caption_test;
#[cfg(test)]
mod chat_embeddings_test;
#[cfg(test)]
mod chat_profile_test;
#[cfg(test)]
mod chat_status_test;
#[cfg(test)]
mod chat_test;
#[cfg(test)]
mod chat_tools_test;
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
mod github_tools_test;
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
mod log_fields_test;
#[cfg(test)]
mod log_redact_test;
#[cfg(test)]
mod logging_test;
#[cfg(test)]
mod media_cache_test;
#[cfg(test)]
mod media_test;
#[cfg(test)]
mod memory_test;
#[cfg(test)]
mod process_test;
#[cfg(test)]
mod qdrant_test;
#[cfg(test)]
mod read_url_test;
#[cfg(test)]
mod research_registry_test;
#[cfg(test)]
mod research_test;
#[cfg(test)]
mod router_test;
#[cfg(test)]
mod self_improve_test;
#[cfg(test)]
mod stickers_test;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tool_registry_test;
#[cfg(test)]
mod usage_test;
#[cfg(test)]
mod user_errors_test;
#[cfg(test)]
mod video_convert_test;
#[cfg(test)]
mod voice_test;
#[cfg(test)]
mod web_api_test;
#[cfg(test)]
mod web_auth_test;
#[cfg(test)]
mod web_config_test;
#[cfg(test)]
mod youtube_test;
