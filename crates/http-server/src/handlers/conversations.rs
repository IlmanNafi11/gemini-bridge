//! Conversation lifecycle and history handlers.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use gemini_bridge_conversation_store::{ConversationStore, ConversationStoreError, StoredMessage};
use gemini_bridge_openai_compat::OpenAiErrorResponse;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct ListConversationsQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListConversationsResponse {
    pub object: &'static str,
    pub data: Vec<gemini_bridge_conversation_store::Conversation>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListMessagesResponse {
    pub object: &'static str,
    pub data: Vec<StoredMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BranchRequestBody {
    pub from_message_id: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchResponseBody {
    pub conversation: gemini_bridge_conversation_store::Conversation,
    pub inherited_message_count: usize,
}

fn require_store(state: &AppState) -> Result<&Arc<dyn ConversationStore>, Box<Response>> {
    state.conversation_store.as_ref().ok_or_else(|| {
        Box::new(
            (
                StatusCode::NOT_IMPLEMENTED,
                Json(OpenAiErrorResponse::new(
                    "conversation store is not enabled",
                    "service_unavailable",
                )),
            )
                .into_response(),
        )
    })
}

fn map_store_error(err: ConversationStoreError) -> Response {
    match err {
        ConversationStoreError::NotFound(id) => (
            StatusCode::NOT_FOUND,
            Json(OpenAiErrorResponse::new(
                format!("conversation not found: {id}"),
                "invalid_request_error",
            )),
        )
            .into_response(),
        ConversationStoreError::MessageNotFound(id) => (
            StatusCode::NOT_FOUND,
            Json(OpenAiErrorResponse::new(
                format!("message not found: {id}"),
                "invalid_request_error",
            )),
        )
            .into_response(),
        ConversationStoreError::InvalidBranchPoint(msg, conv) => (
            StatusCode::BAD_REQUEST,
            Json(OpenAiErrorResponse::new(
                format!(
                    "invalid branch point: message {msg} does not belong to conversation {conv}"
                ),
                "invalid_request_error",
            )),
        )
            .into_response(),
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OpenAiErrorResponse::new(
                other.to_string(),
                "internal_error",
            )),
        )
            .into_response(),
    }
}

pub async fn list_conversations(
    State(state): State<AppState>,
    Query(query): Query<ListConversationsQuery>,
) -> Response {
    let store = match require_store(&state) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };

    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);

    match store.list_conversations(limit + 1, offset).await {
        Ok(mut list) => {
            let has_more = list.len() > limit;
            if has_more {
                list.pop();
            }
            (
                StatusCode::OK,
                Json(ListConversationsResponse {
                    object: "list",
                    data: list,
                    has_more,
                }),
            )
                .into_response()
        }
        Err(e) => map_store_error(e),
    }
}

pub async fn get_messages(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let store = match require_store(&state) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };

    // Ensure conversation exists
    if let Err(e) = store.get_conversation(&id).await {
        return map_store_error(e);
    }

    match store.get_history(&id).await {
        Ok(messages) => (
            StatusCode::OK,
            Json(ListMessagesResponse {
                object: "list",
                data: messages,
            }),
        )
            .into_response(),
        Err(e) => map_store_error(e),
    }
}

pub async fn branch_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<BranchRequestBody>,
) -> Response {
    let store = match require_store(&state) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };

    match store
        .branch_from(&id, &req.from_message_id, req.title)
        .await
    {
        Ok(conversation) => {
            let count = store
                .get_history(&conversation.id)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            (
                StatusCode::CREATED,
                Json(BranchResponseBody {
                    conversation,
                    inherited_message_count: count,
                }),
            )
                .into_response()
        }
        Err(e) => map_store_error(e),
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct RegenerateRequestBody {
    pub from_message_id: String,
    pub model: Option<String>,
}

/// `POST /v1/conversations/{id}/regenerate`
///
/// Creates a branch at `from_message_id` and returns the new conversation ID.
/// The caller is expected to then issue a `/v1/chat/completions` request with
/// the returned `conversation_id` to get the regenerated assistant response.
pub async fn regenerate_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RegenerateRequestBody>,
) -> Response {
    let store = match require_store(&state) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };

    match store.branch_from(&id, &req.from_message_id, None).await {
        Ok(conversation) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "conversation_id": conversation.id,
                "parent_id": conversation.parent_id,
                "parent_message_id": conversation.parent_message_id,
                "model": req.model.as_deref().unwrap_or("gemini-web-flash"),
            })),
        )
            .into_response(),
        Err(e) => map_store_error(e),
    }
}
