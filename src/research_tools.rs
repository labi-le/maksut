use crate::chat_client::ChatConfig;
use crate::prelude::*;
use crate::research_registry::{CancelRequest, ResearchSnapshot, SnapshotFilter};
use serde_json::json;

/// `research_status` payload: bounded JSON snapshot of the chat's passes. The
/// row cap and question truncation live in the registry.
pub(crate) async fn status_payload(
    chat: &ChatConfig,
    chat_id: ChatId,
    owner: bool,
    args: &Value,
) -> String {
    let research_id = args
        .get("research_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    // A non-owner asking for every chat silently gets their own chat only.
    let all_chats = owner
        && args
            .get("all_chats")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let (snapshots, truncated) = chat
        .research_registry
        .snapshots(SnapshotFilter {
            chat_id,
            all_chats,
            owner,
            research_id,
        })
        .await;
    let rows: Vec<Value> = snapshots
        .into_iter()
        .map(ResearchSnapshot::into_json)
        .collect();
    json!({"research": rows, "truncated": truncated}).to_string()
}

/// `research_cancel` result: either the canceled pass or the reason it could
/// not be canceled. Authorization is the registry's, not the dispatcher's.
pub(crate) async fn cancel_payload(
    chat: &ChatConfig,
    chat_id: ChatId,
    user_id: UserId,
    owner: bool,
    args: &Value,
) -> String {
    let research_id = args
        .get("research_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match chat
        .research_registry
        .cancel(CancelRequest {
            chat_id,
            user_id,
            owner,
            research_id,
        })
        .await
    {
        Ok(snapshot) => json!({
            "canceled": true,
            "id": snapshot.id,
            "status": snapshot.status.name(),
            "completed_queries": snapshot.completed_queries,
            "total_queries": snapshot.total_queries,
            "sources": snapshot.sources,
        })
        .to_string(),
        Err(reason) => json!({"canceled": false, "error": reason}).to_string(),
    }
}
