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
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;

use gemini_bridge_conversation_store::{ConversationStore, ConversationStoreError, StoredMessage};
use gemini_bridge_llm_service::{
    Completion, ContentPart, LlmError, LlmEvent, LlmRequest, Message, ModelSelector, Role,
    ToolResult,
};
use gemini_bridge_openai_compat::{
    AssistantMessage, ChatChoice, ChatChoiceDelta, ChatCompletionChunk, ChatCompletionRequest,
    ChatCompletionResponse, DeltaContent, OpenAiErrorResponse, ToolCallFunction, ToolCallObject,
    UsageInfo, new_completion_id, parse_model, unix_now,
};
use gemini_bridge_tool_calling::{
    ParsedToolChoice, ParsedToolResult, ToolDefinition, ToolEngine, tool_definitions_from_specs,
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

    if req.stream && req.conversation_id.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(OpenAiErrorResponse::new(
                "streaming conversation persistence is not supported",
                "invalid_request_error",
            )),
        )
            .into_response();
    }

    // ── Tool schema parsing ───────────────────────────────────────────────────

    // Parse tool_choice directive (default: Auto when tools are provided).
    let parsed_tool_choice = match ParsedToolChoice::from_value(req.tool_choice.as_ref()) {
        Ok(choice) => choice,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OpenAiErrorResponse::new(
                    err.to_string(),
                    "invalid_request_error",
                )),
            )
                .into_response();
        }
    };

    // Convert OpenAI ToolSpec list to provider-neutral ToolDefinitions.
    let mut tool_definitions: Vec<ToolDefinition> = if let Some(specs) = &req.tools {
        match tool_definitions_from_specs(specs) {
            Ok(defs) => defs,
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(OpenAiErrorResponse::new(
                        err.to_string(),
                        "invalid_request_error",
                    )),
                )
                    .into_response();
            }
        }
    } else {
        vec![]
    };

    // `none` explicitly disables tool emulation. Streaming continues to use the
    // existing text-event contract; parsing/injection is non-stream only.
    if matches!(parsed_tool_choice, ParsedToolChoice::None) || req.stream {
        tool_definitions.clear();
    }
    let engine = state.tool_engine.clone();

    // ── Message mapping and tool schema injection ─────────────────────────────

    let mut messages = match map_messages(&req.messages, engine.as_ref(), &tool_definitions) {
        Ok(messages) => messages,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OpenAiErrorResponse::new(message, "invalid_request_error")),
            )
                .into_response();
        }
    };
    let mut metadata = std::collections::BTreeMap::new();
    if let Some(level) = thinking_level {
        metadata.insert("thinking_level".to_string(), serde_json::json!(level));
    }

    // Inject tool schema into the first user (or system) message when tools are present.
    if !tool_definitions.is_empty() && !matches!(parsed_tool_choice, ParsedToolChoice::None) {
        inject_schema_into_messages(
            &mut messages,
            engine.as_ref(),
            &tool_definitions,
            &parsed_tool_choice,
        );
    }

    let continuation = match prepare_continuation(&state, req.conversation_id.as_deref()).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    if let Some(context) = &continuation {
        if let Some(conversation_id) = &context.upstream_conversation_id {
            metadata.insert(
                "conversation_id".to_owned(),
                serde_json::json!(conversation_id),
            );
        }
        if let Some(response_id) = &context.upstream_response_id {
            metadata.insert("response_id".to_owned(), serde_json::json!(response_id));
        }
    }

    let request_messages = messages.clone();
    let llm_req = Arc::new(LlmRequest {
        model: ModelSelector {
            provider: "gemini-web".to_string(),
            model: model_name.clone(),
            thinking_level,
        },
        messages: std::mem::take(&mut messages).into(),
        temperature: req.temperature,
        max_output_tokens: req.max_tokens,
        tools: Arc::from(tool_definitions),
        metadata,
    });

    if req.stream {
        stream_response(state, llm_req, model_name).await
    } else {
        non_stream_response(
            state,
            llm_req,
            model_name,
            continuation,
            request_messages,
            engine.clone(),
        )
        .await
    }
}

// ── Tool schema injection helper ──────────────────────────────────────────────

/// Injects the tool schema block into the first system or user message in place.
/// If no system or user message is present, appends a new user message containing only
/// the schema. Silently skips injection on engine error (schema was already validated).
fn inject_schema_into_messages(
    messages: &mut Vec<Message>,
    engine: &dyn ToolEngine,
    tools: &[ToolDefinition],
    choice: &ParsedToolChoice,
) {
    // The Gemini adapter forwards user/tool text; prefer the initial user message so
    // the injected contract is guaranteed to reach the upstream prompt.
    let target_idx = messages
        .iter()
        .position(|m| m.role == Role::User)
        .or_else(|| messages.iter().position(|m| m.role == Role::System));

    match target_idx {
        Some(idx) => {
            let existing_text = messages[idx]
                .parts
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if let Ok(injected) = engine.inject_tool_schema(&existing_text, tools, choice) {
                let role = messages[idx].role.clone();
                let other_parts = messages[idx]
                    .parts
                    .iter()
                    .filter(|part| !matches!(part, ContentPart::Text(_)))
                    .cloned();
                let parts = std::iter::once(ContentPart::Text(injected))
                    .chain(other_parts)
                    .collect::<Vec<_>>();
                messages[idx] = Message {
                    role,
                    parts: parts.into(),
                };
            }
        }
        None => {
            if let Ok(schema_block) = engine.inject_tool_schema("", tools, choice)
                && !schema_block.trim().is_empty()
            {
                messages.push(Message {
                    role: Role::User,
                    parts: Arc::from([ContentPart::Text(schema_block)]),
                });
            }
        }
    }
}

// ── Non-streaming path ────────────────────────────────────────────────────────

async fn non_stream_response(
    state: AppState,
    llm_req: Arc<LlmRequest>,
    model_name: String,
    continuation: Option<ContinuationContext>,
    request_messages: Vec<Message>,
    engine: Arc<dyn ToolEngine>,
) -> Response {
    let mut continuity_status = "active";
    let completion = match state.adapter.complete(llm_req.clone()).await {
        Ok(comp) => comp,
        Err(LlmError::ContinuityRejected) => {
            if let Some(context) = &continuation {
                match replay_fallback(&state, context, &llm_req, &request_messages).await {
                    Ok(replayed) => {
                        continuity_status = "degraded";
                        replayed
                    }
                    Err(error) => return map_llm_error(error).into_response(),
                }
            } else {
                return map_llm_error(LlmError::ContinuityRejected).into_response();
            }
        }
        Err(error) => return map_llm_error(error).into_response(),
    };

    // Persist messages if conversation store is active.
    if let Some(context) = &continuation
        && let Some(store) = &state.conversation_store
    {
        let _ = persist_turn(
            store.as_ref(),
            &context.conversation_id,
            &request_messages,
            &completion,
        )
        .await;
    }

    let usage = completion.usage.map(|u| UsageInfo {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.prompt_tokens + u.completion_tokens,
    });

    // ── Tool calling parse and validation ─────────────────────────────────
    let (assistant_message, finish_reason, tool_warning) = if !llm_req.tools.is_empty() {
        match engine.parse_and_validate(&completion.text, &llm_req.tools) {
            ParsedToolResult::ToolCalls { calls, text_prefix } => {
                let tool_calls = calls
                    .into_iter()
                    .map(|call| ToolCallObject {
                        id: call.id,
                        call_type: "function".to_string(),
                        function: ToolCallFunction {
                            name: call.name,
                            arguments: call.arguments,
                        },
                    })
                    .collect();
                (
                    AssistantMessage {
                        role: "assistant",
                        content: text_prefix,
                        tool_calls: Some(tool_calls),
                    },
                    "tool_calls".to_string(),
                    None,
                )
            }
            ParsedToolResult::PlainContent { content, warning } => {
                let finish_reason = if warning.is_some() {
                    "stop".to_string()
                } else {
                    completion.finish_reason
                };
                (
                    AssistantMessage {
                        role: "assistant",
                        content: Some(content),
                        tool_calls: None,
                    },
                    finish_reason,
                    warning,
                )
            }
        }
    } else {
        (
            AssistantMessage {
                role: "assistant",
                content: Some(completion.text),
                tool_calls: None,
            },
            completion.finish_reason,
            None,
        )
    };

    let resp = ChatCompletionResponse {
        id: new_completion_id(),
        object: "chat.completion",
        created: unix_now(),
        model: model_name,
        choices: vec![ChatChoice {
            index: 0,
            message: assistant_message,
            finish_reason,
        }],
        usage,
        gemini_metadata: continuation
            .as_ref()
            .map(|context| serde_json::json!({ "conversation_id": context.conversation_id })),
    };

    let mut response = (StatusCode::OK, Json(resp)).into_response();
    if let Some(warning) = &tool_warning {
        let warning_json =
            serde_json::to_string(warning).unwrap_or_else(|_| warning.message.clone());
        let safe_warning: String = warning_json
            .chars()
            .map(|c| {
                if c.is_ascii() && !c.is_ascii_control() {
                    c
                } else {
                    ' '
                }
            })
            .collect();
        if let Ok(val) = HeaderValue::from_str(&safe_warning) {
            response.headers_mut().insert(
                header::HeaderName::from_static("x-gemini-bridge-tool-warning"),
                val,
            );
        }
    }
    if let Some(context) = &continuation {
        response.headers_mut().insert(
            header::HeaderName::from_static("x-gemini-bridge-continuity"),
            HeaderValue::from_static(continuity_status),
        );
        if let Ok(val) = HeaderValue::from_str(&context.conversation_id) {
            response.headers_mut().insert(
                header::HeaderName::from_static("x-gemini-bridge-conversation-id"),
                val,
            );
        }
    }
    response
}
#[derive(Clone)]
struct ContinuationContext {
    conversation_id: String,
    upstream_conversation_id: Option<String>,
    upstream_response_id: Option<String>,
}

async fn prepare_continuation(
    state: &AppState,
    conversation_id: Option<&str>,
) -> Result<Option<ContinuationContext>, Response> {
    let Some(store) = &state.conversation_store else {
        if conversation_id.is_none() {
            return Ok(None);
        }
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            Json(OpenAiErrorResponse::new(
                "conversation store is not enabled",
                "service_unavailable",
            )),
        )
            .into_response());
    };
    let Some(id) = conversation_id else {
        return match store.create_conversation(None).await {
            Ok(conversation) => Ok(Some(ContinuationContext {
                conversation_id: conversation.id,
                upstream_conversation_id: None,
                upstream_response_id: None,
            })),
            Err(error) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OpenAiErrorResponse::new(
                    error.to_string(),
                    "internal_error",
                )),
            )
                .into_response()),
        };
    };
    match store.get_conversation(id).await {
        Ok(conversation) => Ok(Some(ContinuationContext {
            conversation_id: conversation.id,
            upstream_conversation_id: conversation.upstream_conversation_id,
            upstream_response_id: conversation.upstream_response_id,
        })),
        Err(ConversationStoreError::NotFound(_)) => Err((
            StatusCode::NOT_FOUND,
            Json(OpenAiErrorResponse::new(
                format!("conversation not found: {id}"),
                "invalid_request_error",
            )),
        )
            .into_response()),
        Err(error) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OpenAiErrorResponse::new(
                error.to_string(),
                "internal_error",
            )),
        )
            .into_response()),
    }
}

async fn replay_fallback(
    state: &AppState,
    context: &ContinuationContext,
    original_request: &Arc<LlmRequest>,
    request_messages: &[Message],
) -> Result<Completion, LlmError> {
    let Some(store) = &state.conversation_store else {
        return Err(LlmError::Unavailable);
    };
    let history = store
        .get_history(&context.conversation_id)
        .await
        .map_err(|err| LlmError::Protocol(err.to_string()))?;

    let mut replay_messages = Vec::with_capacity(history.len() + request_messages.len());
    for record in history {
        replay_messages.push(stored_to_message(&record));
    }
    replay_messages.extend_from_slice(request_messages);

    let mut metadata = original_request.metadata.clone();
    metadata.remove("conversation_id");
    metadata.remove("response_id");
    metadata.remove("candidate_id");

    let replay_request = Arc::new(LlmRequest {
        model: original_request.model.clone(),
        messages: replay_messages.into(),
        temperature: original_request.temperature,
        max_output_tokens: original_request.max_output_tokens,
        tools: original_request.tools.clone(),
        metadata,
    });
    state.adapter.complete(replay_request).await
}

fn stored_to_message(message: &StoredMessage) -> Message {
    let role = match message.role.as_str() {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    };
    let content = serde_json::from_str::<String>(&message.content_json)
        .unwrap_or_else(|_| message.content_json.clone());
    Message {
        role,
        parts: Arc::from([ContentPart::Text(content)]),
    }
}

async fn persist_turn(
    store: &dyn ConversationStore,
    conversation_id: &str,
    request_messages: &[Message],
    completion: &Completion,
) -> Result<(), ConversationStoreError> {
    let history = store.get_history(conversation_id).await?;
    let mut sequence_number = history.last().map_or(1, |m| m.sequence_number + 1);
    let ids = completion.metadata.as_ref().map(|metadata| &metadata.raw);
    let upstream_conversation_id = ids
        .and_then(|raw| raw.get("conversation_id"))
        .and_then(|value| value.as_str());
    let upstream_response_id = ids
        .and_then(|raw| raw.get("response_id"))
        .and_then(|value| value.as_str());
    let upstream_candidate_id = ids
        .and_then(|raw| raw.get("candidate_id"))
        .and_then(|value| value.as_str());

    for message in request_messages {
        let (role, content_json) = message_to_stored(message);
        store
            .append_message(StoredMessage {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_owned(),
                parent_message_id: None,
                role,
                content_json,
                sequence_number,
                created_at: unix_now(),
                upstream_conversation_id: None,
                upstream_response_id: None,
                upstream_candidate_id: None,
            })
            .await?;
        sequence_number += 1;
    }

    store
        .append_message(StoredMessage {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_owned(),
            parent_message_id: None,
            role: "assistant".to_owned(),
            content_json: serde_json::to_string(&completion.text)?,
            sequence_number,
            created_at: unix_now(),
            upstream_conversation_id: upstream_conversation_id.map(str::to_owned),
            upstream_response_id: upstream_response_id.map(str::to_owned),
            upstream_candidate_id: upstream_candidate_id.map(str::to_owned),
        })
        .await
}

fn message_to_stored(message: &Message) -> (String, String) {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let content = message
        .parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    (
        role.to_owned(),
        serde_json::to_string(&content).unwrap_or_default(),
    )
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

fn map_messages(
    messages: &[gemini_bridge_openai_compat::ChatMessage],
    engine: &dyn ToolEngine,
    tools: &[ToolDefinition],
) -> Result<Vec<Message>, String> {
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

            let parts = if role == Role::Tool {
                let call_id = m
                    .tool_call_id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| "tool message is missing tool_call_id".to_owned())?;
                let tool_result = ToolResult {
                    call_id: call_id.to_owned(),
                    content: text.clone(),
                };
                let formatted =
                    engine.format_tool_continuation(std::slice::from_ref(&tool_result), tools);
                vec![
                    ContentPart::ToolResult(tool_result),
                    ContentPart::Text(formatted),
                ]
            } else if role == Role::Assistant && m.tool_calls.is_some() {
                let mut p = Vec::new();
                if !text.is_empty() {
                    p.push(ContentPart::Text(text));
                }
                if let Some(tcs) = &m.tool_calls {
                    for tc in tcs {
                        p.push(ContentPart::ToolCall(gemini_bridge_llm_service::ToolCall {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            arguments: tc.function.arguments.clone(),
                        }));
                    }
                }
                p
            } else {
                vec![ContentPart::Text(text)]
            };

            Ok(Message {
                role,
                parts: Arc::from(parts),
            })
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
        LlmError::ContinuityRejected => (
            StatusCode::GONE,
            Json(OpenAiErrorResponse::new(
                "Upstream rejected conversation continuation identifiers",
                "invalid_request_error",
            )),
        ),
    }
}
