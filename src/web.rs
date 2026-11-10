//! HTTP surface for the owner-only Telegram Mini App: router, state, startup,
//! and the `/settings` chat button. Parsing and signing live in
//! `src/web_auth.rs`, the JSON endpoints in `src/web_api.rs`, the config file in
//! `src/web_config.rs`, and the embedded page in `src/web_static.rs`.

use crate::*;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::Router;
use teloxide::types::{
    InlineKeyboardButton, InlineKeyboardButtonKind, InlineKeyboardMarkup, WebAppInfo,
};

/// Every endpoint answers from SQLite or one small file; ten seconds is already
/// far past a healthy response and bounds a wedged handler.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct WebState {
    pub(crate) settings: Arc<Settings>,
    pub(crate) bot_token: Arc<str>,
    /// Serializes `config.toml` rewrites so two tabs cannot interleave a
    /// backup and a rename.
    pub(crate) config_lock: Arc<tokio::sync::Mutex<()>>,
}

pub(crate) fn router(state: WebState) -> Router {
    let limit = config().web.max_body_bytes;
    Router::new()
        .route("/", get(crate::web_static::index))
        .route("/app.js", get(crate::web_static::app_js))
        .route("/style.css", get(crate::web_static::style_css))
        .route(
            "/favicon.ico",
            get(|| async { StatusCode::NO_CONTENT.into_response() }),
        )
        .route("/api/memory", get(crate::web_api::list_memory))
        .route(
            "/api/memory/{topic}",
            put(crate::web_api::update_memory).delete(crate::web_api::delete_memory),
        )
        .route(
            "/api/config",
            get(crate::web_api::get_config).put(crate::web_api::put_config),
        )
        .layer(middleware::from_fn(request_timeout))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(state)
}

async fn request_timeout(request: Request, next: Next) -> Response {
    match time::timeout(REQUEST_TIMEOUT, next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            tracing::warn!(stage = "web-timeout", "mini app request timed out");
            (StatusCode::GATEWAY_TIMEOUT, "request timed out").into_response()
        }
    }
}

/// Starts the listener when `web.enabled`; a failure leaves the bot running
/// without the Mini App instead of crashing the process.
pub(crate) fn start(settings: Arc<Settings>, bot_token: String) {
    if !config().web.enabled {
        return;
    }
    let Ok(address) = config().web.listen.parse::<SocketAddr>() else {
        tracing::error!(stage = "web-listen", "web.listen is not a socket address");
        return;
    };
    let state = WebState {
        settings,
        bot_token: Arc::from(bot_token),
        config_lock: Arc::new(tokio::sync::Mutex::new(())),
    };
    let app = router(state);
    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => listener,
            Err(_) => {
                tracing::error!(stage = "web-bind", address = %address, "unable to bind mini app listener");
                return;
            }
        };
        tracing::info!(address = %address, "mini app listening");
        if axum::serve(listener, app).await.is_err() {
            tracing::error!(stage = "web-serve", "mini app server stopped");
        }
    });
}

/// What `/settings` answers: the button, or the one line that says what to
/// configure. Split out from the send so the decision is testable without a
/// Bot API server.
pub(crate) enum SettingsReply {
    Button(InlineKeyboardMarkup),
    Text(&'static str),
}

pub(crate) fn settings_reply(web: &WebSettings) -> SettingsReply {
    if !web.enabled {
        return SettingsReply::Text(
            "Мини-апп выключен: включите секцию [web] (enabled = true) в config.toml и перезапустите бота.",
        );
    }
    let Some(raw) = web.public_url.as_deref() else {
        return SettingsReply::Text(
            "Задайте web.public_url (https-адрес мини-аппа) в config.toml и перезапустите бота.",
        );
    };
    let Ok(url) = Url::parse(raw) else {
        return SettingsReply::Text("web.public_url в config.toml некорректен.");
    };
    SettingsReply::Button(InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::new(
            "Открыть настройки",
            InlineKeyboardButtonKind::WebApp(WebAppInfo { url }),
        ),
    ]]))
}

/// Owner-only `/settings` in a private chat: reply with a `web_app` button.
/// Every other sender, chat kind, or command is left alone.
pub(crate) async fn handle_settings_command(bot: &Bot, message: &Message, cfg: &Settings) -> bool {
    let Some(text) = message.text() else {
        return false;
    };
    if !is_settings_command(text, &cfg.bot_username) {
        return false;
    }
    let owner = message
        .from
        .as_ref()
        .is_some_and(|user| user.id == cfg.owner_id);
    if !owner || !message.chat.is_private() {
        return true;
    }
    match settings_reply(&config().web) {
        SettingsReply::Button(keyboard) => {
            if bot
                .send_message(message.chat.id, "Настройки Максута:")
                .reply_markup(keyboard)
                .disable_notification(true)
                .await
                .is_err()
            {
                tracing::warn!(stage = "web-button", "unable to send web app button");
            }
        }
        SettingsReply::Text(text) => send_text(bot, message, text).await,
    }
    true
}

async fn send_text(bot: &Bot, message: &Message, text: &str) {
    if bot
        .send_message(message.chat.id, text)
        .disable_notification(true)
        .await
        .is_err()
    {
        tracing::warn!(stage = "web-button", "unable to send mini app notice");
    }
}

pub(crate) fn is_settings_command(text: &str, username: &str) -> bool {
    let Some(name) = text.split_whitespace().next() else {
        return false;
    };
    name == "/settings"
        || name
            .strip_prefix("/settings@")
            .is_some_and(|target| !target.is_empty() && target.eq_ignore_ascii_case(username))
}
