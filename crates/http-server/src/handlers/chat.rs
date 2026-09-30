//! Handler for `POST /v1/chat/completions` (non-streaming).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use gemini_bridge_llm_service::{ContentPart, LlmError, LlmRequest, Message, ModelSelector, Role};
use gemini_bridge_openai_compat::{
    AssistantMessage, ChatChoice, ChatCompletionRequest, ChatCompletionResponse,
    OpenAiErrorResponse, UsageInfo, new_completion_id, parse_model, unix_now,
};

use crate::AppState;

pub async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    // Validate and parse the model alias.
    let (model_name, thinking_level) = match parse_model(&req.model) {
        Ok(pair) => pair,
        Err(err) => {
            let body = OpenAiErrorResponse::new(err.to_string(), "invalid_request_error");
            return (StatusCode::BAD_REQUEST, Json(body)).into_response();
        }
    };

    // Map OpenAI messages to the neutral LLM contract.
    let messages = map_messages(&req.messages);

    let mut metadata = std::collections::BTreeMap::new();
    if let Some(level) = thinking_level {
        metadata.insert("thinking_level".to_string(), serde_json::json!(level));
    }

    let llm_req = Arc::new(LlmRequest {
        model: ModelSelector {
            provider: "gemini-web".to_string(),
            model: model_name.clone(),
            thinking_level,
        },
        messages: messages.into(),
        temperature: req.temperature,
        max_output_tokens: req.max_tokens,
        tools: Arc::from(vec![]),
        metadata,
    });

    match state.adapter.complete(llm_req).await {
        Ok(completion) => {
            let usage = completion.usage.map(|u| UsageInfo {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.prompt_tokens + u.completion_tokens,
            });

            let resp = ChatCompletionResponse {
                id: new_completion_id(),
                object: "chat.completion",
                created: unix_now(),
                model: model_name,
                choices: vec![ChatChoice {
                    index: 0,
                    message: AssistantMessage {
                        role: "assistant",
                        content: completion.text,
                    },
                    finish_reason: completion.finish_reason,
                }],
                usage,
                gemini_metadata: None,
            };

            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(err) => map_llm_error(err).into_response(),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn map_messages(messages: &[gemini_bridge_openai_compat::ChatMessage]) -> Vec<Message> {
    use gemini_bridge_openai_compat::ChatMessageContent;

    messages
        .iter()
        .map(|m| {
            let role = match m.role.as_str() {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "system" => Role::System,
                "tool" => Role::Tool,
                _ => Role::User,
            };

            let text = match &m.content {
                Some(ChatMessageContent::Text(t)) => t.clone(),
                Some(ChatMessageContent::Parts(parts)) => parts
                    .iter()
                    .filter_map(|p| p.text.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n"),
                None => String::new(),
            };

            Message {
                role,
                parts: Arc::from(vec![ContentPart::Text(text)]),
            }
        })
        .collect()
}

fn map_llm_error(err: LlmError) -> (StatusCode, Json<OpenAiErrorResponse>) {
    match err {
        LlmError::Authentication => (
            StatusCode::UNAUTHORIZED,
            Json(OpenAiErrorResponse::with_code(
                "Authentication required — run `gemini-bridge auth login`",
                "authentication_error",
                "invalid_api_key",
            )),
        ),
        LlmError::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(OpenAiErrorResponse::with_code(
                "Upstream rate limited",
                "rate_limit_exceeded",
                "rate_limit_exceeded",
            )),
        ),
        LlmError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(OpenAiErrorResponse::new(
                "Provider unavailable",
                "service_unavailable",
            )),
        ),
        LlmError::Unsupported(cap) => (
            StatusCode::BAD_REQUEST,
            Json(OpenAiErrorResponse::new(
                format!("Unsupported capability: {cap}"),
                "invalid_request_error",
            )),
        ),
        LlmError::Protocol(msg) => (
            StatusCode::BAD_GATEWAY,
            Json(OpenAiErrorResponse::new(msg, "provider_error")),
        ),
    }
}
