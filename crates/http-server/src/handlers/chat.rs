//! Handler for `POST /v1/chat/completions` — both streaming (SSE) and non-streaming.
//!
//! When `stream: true` the response is an SSE text/event-stream where each
//! `data:` line carries a `ChatCompletionChunk` JSON object, terminated by
//! `data: [DONE]`.  When `stream: false` (the default) the full completion is
//! returned as a single `ChatCompletionResponse` JSON object.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;

use gemini_bridge_llm_service::{
    ContentPart, LlmError, LlmEvent, LlmRequest, Message, ModelSelector, Role,
};
use gemini_bridge_openai_compat::{
    AssistantMessage, ChatChoice, ChatChoiceDelta, ChatCompletionChunk, ChatCompletionRequest,
    ChatCompletionResponse, DeltaContent, OpenAiErrorResponse, UsageInfo, new_completion_id,
    parse_model, unix_now,
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

    if req.stream {
        stream_response(state, llm_req, model_name).await
    } else {
        non_stream_response(state, llm_req, model_name).await
    }
}

// ── Non-streaming path ────────────────────────────────────────────────────────

async fn non_stream_response(
    state: AppState,
    llm_req: Arc<LlmRequest>,
    model_name: String,
) -> Response {
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

// ── Streaming SSE path ────────────────────────────────────────────────────────

async fn stream_response(
    state: AppState,
    llm_req: Arc<LlmRequest>,
    model_name: String,
) -> Response {
    let event_stream = match state.adapter.stream(llm_req).await {
        Ok(s) => s,
        Err(err) => return map_llm_error(err).into_response(),
    };

    let completion_id = new_completion_id();
    let created = unix_now();

    // Translate each LlmEvent into an SSE `Event`.
    let sse_stream = event_stream.map(move |ev| -> Result<Event, Infallible> {
        let data = match ev {
            Ok(LlmEvent::TextDelta(text)) => {
                let chunk = ChatCompletionChunk {
                    id: completion_id.clone(),
                    object: "chat.completion.chunk",
                    created,
                    model: model_name.clone(),
                    choices: vec![ChatChoiceDelta {
                        index: 0,
                        delta: DeltaContent {
                            role: None,
                            content: Some(text),
                        },
                        finish_reason: None,
                    }],
                };
                serde_json::to_string(&chunk).unwrap_or_default()
            }
            Ok(LlmEvent::Completed(summary)) => {
                let chunk = ChatCompletionChunk {
                    id: completion_id.clone(),
                    object: "chat.completion.chunk",
                    created,
                    model: model_name.clone(),
                    choices: vec![ChatChoiceDelta {
                        index: 0,
                        delta: DeltaContent {
                            role: None,
                            content: None,
                        },
                        finish_reason: Some(summary.finish_reason),
                    }],
                };
                serde_json::to_string(&chunk).unwrap_or_default()
            }
            Ok(_) => return Ok(Event::default().comment("skip")),
            Err(err) => {
                // Surface provider errors as a final data event before close.
                let msg = format!("{{\"error\":\"{}\"}}", err);
                return Ok(Event::default().data(msg));
            }
        };
        Ok(Event::default().data(data))
    });

    // Append the `[DONE]` sentinel required by the OpenAI SSE protocol.
    let done =
        futures::stream::once(async { Ok::<_, Infallible>(Event::default().data("[DONE]")) });
    let combined = sse_stream.chain(done);

    Sse::new(combined)
        .keep_alive(KeepAlive::default())
        .into_response()
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
