//! End-to-end HTTP tool-calling round trip through the public chat endpoint.
//!
//! The test adapter returns a deterministic model tool-call candidate on the
//! first turn, then verifies the client-submitted `role: "tool"` result reaches
//! the provider-neutral LLM request on the continuation turn. No tool is
//! executed by the bridge or test server.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use gemini_bridge_health_admin::DefaultHealthAdminService;
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_llm_service::{
    Completion, ContentPart, LlmAdapter, LlmError, LlmEventStream, LlmRequest, Role,
};
use gemini_bridge_tool_calling::{DefaultToolEngine, ToolEngine};
use parking_lot::Mutex;
use serde_json::{Value, json};

const TOOL_CALL_CANDIDATE: &str =
    "```json\n{\"name\": \"lookup_order\", \"arguments\": {\"order_id\": \"ord-42\"}}\n```";
const CLIENT_TOOL_RESULT: &str = "Order ord-42 is shipped";

#[derive(Default)]
struct ToolRoundTripAdapter {
    requests: Mutex<Vec<Arc<LlmRequest>>>,
}

#[async_trait]
impl LlmAdapter for ToolRoundTripAdapter {
    fn provider_id(&self) -> &'static str {
        "tool-round-trip-test"
    }

    async fn complete(&self, request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        let is_tool_continuation = request.messages.iter().any(|message| {
            message.role == Role::Tool
                && message.parts.iter().any(|part| {
                    matches!(part, ContentPart::ToolResult(result) if result.content == CLIENT_TOOL_RESULT)
                })
        });
        self.requests.lock().push(request);

        Ok(Completion {
            text: if is_tool_continuation {
                "Order ord-42 has shipped.".into()
            } else {
                TOOL_CALL_CANDIDATE.into()
            },
            finish_reason: "stop".into(),
            usage: None,
            metadata: None,
        })
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        Err(LlmError::Unsupported("streaming"))
    }
}

async fn start_tool_server() -> (
    String,
    tokio::task::JoinHandle<()>,
    Arc<ToolRoundTripAdapter>,
) {
    let adapter = Arc::new(ToolRoundTripAdapter::default());
    let state = AppState {
        adapter: adapter.clone(),
        upload_service: None,
        image_service: None,
        health_admin: Arc::new(DefaultHealthAdminService::new(None)),
        conversation_store: None,
        tool_engine: Arc::new(DefaultToolEngine) as Arc<dyn ToolEngine>,
        gallery_service: None,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let router: Router = build_router(
        ServerConfig {
            bind_addr: addr,
            api_key: None,
            require_key_for_admin: false,
            cors_enabled: false,
            rate_limit: None,
        },
        state,
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), server, adapter)
}

fn lookup_order_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "lookup_order",
            "description": "Look up an order by ID",
            "parameters": {
                "type": "object",
                "properties": {
                    "order_id": { "type": "string" }
                },
                "required": ["order_id"]
            }
        }
    })
}

fn chat_request(messages: Value) -> Value {
    json!({
        "model": "gemini-web-flash",
        "messages": messages,
        "stream": false,
        "tools": [lookup_order_tool()],
        "tool_choice": "auto"
    })
}

#[tokio::test]
async fn http_chat_returns_tool_call_then_accepts_tool_result_for_final_answer() {
    let (base, server, adapter) = start_tool_server().await;
    let client = reqwest::Client::new();

    // The bridge validates the model's candidate and exposes an OpenAI-shaped
    // tool call. It does not execute the tool.
    let first_response = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&chat_request(json!([{
            "role": "user",
            "content": "Where is order ord-42?"
        }])))
        .send()
        .await
        .unwrap();
    assert_eq!(first_response.status(), reqwest::StatusCode::OK);
    let first_body: Value = first_response.json().await.unwrap();
    let first_choice = &first_body["choices"][0];
    assert_eq!(first_choice["finish_reason"], "tool_calls");
    assert_eq!(first_choice["message"]["role"], "assistant");
    let tool_calls = first_choice["message"]["tool_calls"]
        .as_array()
        .expect("assistant response must include tool_calls");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["type"], "function");
    assert_eq!(tool_calls[0]["function"]["name"], "lookup_order");
    assert_eq!(
        serde_json::from_str::<Value>(tool_calls[0]["function"]["arguments"].as_str().unwrap())
            .unwrap(),
        json!({ "order_id": "ord-42" })
    );
    let call_id = tool_calls[0]["id"]
        .as_str()
        .expect("tool call must have an id")
        .to_owned();

    // The client (not the bridge) supplies its tool execution result paired by
    // the exact call_id received in the prior assistant message.
    let second_response = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&chat_request(json!([
            {
                "role": "user",
                "content": "Where is order ord-42?"
            },
            {
                "role": "assistant",
                "content": null,
                "tool_calls": tool_calls
            },
            {
                "role": "tool",
                "tool_call_id": call_id,
                "name": "lookup_order",
                "content": CLIENT_TOOL_RESULT
            }
        ])))
        .send()
        .await
        .unwrap();
    assert_eq!(second_response.status(), reqwest::StatusCode::OK);
    let second_body: Value = second_response.json().await.unwrap();
    let second_choice = &second_body["choices"][0];
    assert_eq!(second_choice["finish_reason"], "stop");
    assert_eq!(
        second_choice["message"]["content"],
        "Order ord-42 has shipped."
    );

    // The real adapter boundary receives the role=tool result with its matched
    // call ID and exact content; the bridge has only forwarded, not executed it.
    let requests = adapter.requests.lock().clone();
    assert_eq!(requests.len(), 2);
    let tool_result = requests[1]
        .messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .find_map(|part| match part {
            ContentPart::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("role=tool continuation must map to ContentPart::ToolResult");
    assert_eq!(tool_result.call_id, call_id);
    assert_eq!(tool_result.content, CLIENT_TOOL_RESULT);

    server.abort();
}

#[derive(Default)]
struct InvalidToolOutputAdapter;

#[async_trait]
impl LlmAdapter for InvalidToolOutputAdapter {
    fn provider_id(&self) -> &'static str {
        "invalid-tool-output-test"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Ok(Completion {
            text: "```json\n{\"name\": \"unknown_tool\", \"arguments\": {}}\n```".into(),
            finish_reason: "stop".into(),
            usage: None,
            metadata: None,
        })
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        Err(LlmError::Unsupported("streaming"))
    }
}

#[tokio::test]
async fn http_chat_invalid_tool_output_falls_back_to_text_with_warning_header() {
    let adapter = Arc::new(InvalidToolOutputAdapter);
    let state = AppState {
        adapter,
        upload_service: None,
        image_service: None,
        health_admin: Arc::new(DefaultHealthAdminService::new(None)),
        conversation_store: None,
        tool_engine: Arc::new(DefaultToolEngine) as Arc<dyn ToolEngine>,
        gallery_service: None,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let router: Router = build_router(
        ServerConfig {
            bind_addr: addr,
            api_key: None,
            require_key_for_admin: false,
            cors_enabled: false,
            rate_limit: None,
        },
        state,
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&chat_request(json!([{
            "role": "user",
            "content": "Perform the action"
        }])))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // Safe non-executable fallback invariant: output remains assistant text,
    // finish_reason is "stop", no executable tool_calls are emitted.
    let warning_header = response
        .headers()
        .get("x-gemini-bridge-tool-warning")
        .expect("x-gemini-bridge-tool-warning header must be present on fallback");
    let warning_str = warning_header.to_str().unwrap();
    let warning_json: Value = serde_json::from_str(warning_str)
        .expect("Warning header must contain valid serialized JSON");
    assert_eq!(warning_json["code"], "UNKNOWN_FUNCTION");
    assert!(warning_json["raw_candidate"].is_string());

    let body: Value = response.json().await.unwrap();
    let choice = &body["choices"][0];
    assert_eq!(choice["finish_reason"], "stop");
    assert_eq!(
        choice["message"]["content"],
        "```json\n{\"name\": \"unknown_tool\", \"arguments\": {}}\n```"
    );
    assert!(
        choice["message"].get("tool_calls").is_none() || choice["message"]["tool_calls"].is_null(),
        "Invalid tool call must never emit executable tool_calls"
    );

    server.abort();
}
