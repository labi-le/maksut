//! JSON endpoints behind `Authorization: tma <initData>`. Authorization is the
//! first statement in every handler; nothing in this module writes SQL against
//! `bot_self_memory` directly, because the chat tool and the UI must share one
//! write path (safety filters, eviction, Qdrant write-through).

use crate::web::WebState;
use crate::web_config::{EditKind, EditValue};
use crate::*;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(serde::Serialize)]
struct ErrorBody {
    error: String,
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorBody {
            error: message.into(),
        }),
    )
        .into_response()
}

fn authenticate(state: &WebState, headers: &HeaderMap) -> Option<Response> {
    let header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let Some(init_data) = crate::web_auth::init_data_from_authorization(header) else {
        return Some(error(
            StatusCode::UNAUTHORIZED,
            "нужна авторизация Telegram",
        ));
    };
    match crate::web_auth::verify(
        init_data,
        &state.bot_token,
        state.settings.owner_id,
        config().web.auth_max_age_secs,
    ) {
        Ok(_) => None,
        Err(crate::web_auth::AuthError::NotOwner { user_id }) => {
            // A valid signed payload for another user cannot be forged, so this
            // is the one rejection worth a warning.
            tracing::warn!(user_id, "mini app auth rejected: not owner");
            Some(error(StatusCode::FORBIDDEN, "доступ только владельцу бота"))
        }
        Err(reason) => {
            // Anyone who can reach the listener can produce these, and every
            // formatted event lands in the bounded ring behind `read_logs`:
            // warn-level here would let a prober flush the owner's diagnostics.
            // The payload itself is never logged: it is a bearer credential.
            tracing::debug!(reason = ?reason, "mini app auth rejected");
            Some(error(
                StatusCode::UNAUTHORIZED,
                "нужна авторизация Telegram",
            ))
        }
    }
}

#[derive(serde::Serialize)]
struct MemoryItem {
    topic: String,
    fact: String,
    source_url: String,
    updated_at: i64,
    fact_max_chars: usize,
}

impl MemoryItem {
    fn from_fact(fact: crate::self_memory::SelfFact) -> Self {
        Self {
            topic: fact.topic,
            fact: fact.fact,
            source_url: fact.source_url,
            updated_at: fact.updated_at,
            fact_max_chars: config().code.fact_max_chars,
        }
    }
}

pub(crate) async fn list_memory(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Some(response) = authenticate(&state, &headers) {
        return response;
    }
    match crate::self_memory::list_self_facts(
        &state.settings.database,
        config().code.memory_max_rows,
    )
    .await
    {
        Ok(rows) => {
            let items: Vec<MemoryItem> = rows.into_iter().map(MemoryItem::from_fact).collect();
            Json(items).into_response()
        }
        Err(_) => {
            tracing::warn!(stage = "web-memory-list", "memory read failed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "не удалось прочитать память",
            )
        }
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct MemoryEdit {
    pub(crate) fact: String,
}

pub(crate) async fn update_memory(
    State(state): State<WebState>,
    Path(topic): Path<String>,
    headers: HeaderMap,
    Json(edit): Json<MemoryEdit>,
) -> Response {
    if let Some(response) = authenticate(&state, &headers) {
        return response;
    }
    let Some(topic) = topic_key(&topic) else {
        return error(StatusCode::NOT_FOUND, "запись не найдена");
    };
    let Some(chat) = state.settings.chat.as_ref() else {
        return error(
            StatusCode::CONFLICT,
            "правка памяти недоступна: LLM не настроен",
        );
    };
    let existing = match crate::self_memory::get_self_fact(&state.settings.database, topic).await {
        Ok(Some(row)) => row,
        Ok(None) => return error(StatusCode::NOT_FOUND, "запись не найдена"),
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "не удалось прочитать память",
            )
        }
    };
    if existing.source_url.trim().is_empty() {
        return error(
            StatusCode::CONFLICT,
            "записи владельца правятся только через remember_self",
        );
    }
    if edit.fact.trim().is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "пустой факт; для удаления есть DELETE",
        );
    }
    if let Err(reason) = crate::self_memory::record_self_fact(
        chat,
        &state.settings.database,
        &chat.code_dir,
        topic,
        &edit.fact,
        &existing.source_url,
    )
    .await
    {
        // `record_self_fact` has no error type: its two storage failures are
        // fixed strings, everything else is a rejected value.
        let status = match reason.as_str() {
            crate::self_memory::STORE_FACT_FAILURE | crate::self_memory::MEMORY_UPDATE_FAILURE => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            _ => StatusCode::BAD_REQUEST,
        };
        return error(status, reason);
    }
    match crate::self_memory::get_self_fact(&state.settings.database, topic).await {
        Ok(Some(row)) => Json(MemoryItem::from_fact(row)).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "запись не найдена"),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "не удалось прочитать память",
        ),
    }
}

pub(crate) async fn delete_memory(
    State(state): State<WebState>,
    Path(topic): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = authenticate(&state, &headers) {
        return response;
    }
    let Some(topic) = topic_key(&topic) else {
        return error(StatusCode::NOT_FOUND, "запись не найдена");
    };
    match crate::self_memory::get_self_fact(&state.settings.database, topic).await {
        Ok(Some(row)) if !row.source_url.trim().is_empty() => {}
        Ok(Some(_)) => {
            return error(
                StatusCode::CONFLICT,
                "записи владельца удаляются только через remember_self",
            )
        }
        Ok(None) => return error(StatusCode::NOT_FOUND, "запись не найдена"),
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "не удалось прочитать память",
            )
        }
    }
    match crate::self_memory::delete_self_topic(&state.settings.database, topic).await {
        Ok(true) => Json(serde_json::json!({ "deleted": true, "topic": topic })).into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, "запись не найдена"),
        Err(reason) => error(StatusCode::INTERNAL_SERVER_ERROR, reason),
    }
}

pub(crate) async fn get_config(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Some(response) = authenticate(&state, &headers) {
        return response;
    }
    Json(crate::web_config::ConfigEditor::resolve().describe(config())).into_response()
}

#[derive(serde::Deserialize)]
pub(crate) struct ConfigEdit {
    pub(crate) key: String,
    pub(crate) value: Value,
}

pub(crate) async fn put_config(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(edit): Json<ConfigEdit>,
) -> Response {
    if let Some(response) = authenticate(&state, &headers) {
        return response;
    }
    let Some(entry) = crate::web_config::EDITABLE
        .iter()
        .find(|entry| entry.key == edit.key)
    else {
        return error(StatusCode::BAD_REQUEST, "неизвестный ключ");
    };
    let value = match entry.kind {
        EditKind::Int => match edit.value.as_i64() {
            Some(number) if number >= 0 => EditValue::Int(number),
            _ => {
                return error(
                    StatusCode::BAD_REQUEST,
                    format!("{} ожидает неотрицательное целое число", entry.key),
                )
            }
        },
        EditKind::Str => match edit.value.as_str() {
            Some(text) => EditValue::Str(text.to_owned()),
            None => {
                return error(
                    StatusCode::BAD_REQUEST,
                    format!("{} ожидает строку", entry.key),
                )
            }
        },
    };
    let _guard = state.config_lock.lock().await;
    let editor = crate::web_config::ConfigEditor::resolve();
    match editor.write(entry.key, &value).await {
        Ok(()) => {
            tracing::info!(key = entry.key, "config.toml edited from mini app");
            Json(serde_json::json!({
                "key": entry.key,
                "value": value,
                "restart_required": true,
            }))
            .into_response()
        }
        Err(failure) => {
            let internal = failure.is_internal();
            if internal {
                tracing::error!(
                    key = entry.key,
                    reason = failure.message(),
                    "config.toml write failed"
                );
            }
            let status = if internal {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::BAD_REQUEST
            };
            error(status, failure.message().to_owned())
        }
    }
}

/// Existing rows have at most `code.topic_max_chars` characters, so anything
/// longer cannot be a row and must not reach the database.
fn topic_key(raw: &str) -> Option<&str> {
    let topic = raw.trim();
    (!topic.is_empty() && topic.chars().count() <= config().code.topic_max_chars).then_some(topic)
}
