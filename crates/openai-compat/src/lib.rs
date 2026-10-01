//! OpenAI API JSON schemas and normalization.
//!
//! This crate owns the request/response types for `/v1/chat/completions`,
//! `/v1/models`, and error envelopes. It does NOT import Axum, reqwest, or
//! any provider-specific crate.

pub mod metadata;
pub use metadata::{CitationMeta, CodeExecutionMeta, GeminiMetadata};
pub mod models;
pub mod tools;

pub use tools::{ToolCallFunction, ToolCallObject};

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum OpenAiCompatError {
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

// ── OpenAI error envelope ─────────────────────────────────────────────────────

/// Standard OpenAI error response body.
#[derive(Debug, Clone, Serialize)]
pub struct OpenAiErrorResponse {
    pub error: OpenAiErrorDetails,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAiErrorDetails {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: Option<String>,
}

impl OpenAiErrorResponse {
    pub fn new(message: impl Into<String>, error_type: impl Into<String>) -> Self {
        Self {
            error: OpenAiErrorDetails {
                message: message.into(),
                error_type: error_type.into(),
                code: None,
            },
        }
    }

    pub fn with_code(
        message: impl Into<String>,
        error_type: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            error: OpenAiErrorDetails {
                message: message.into(),
                error_type: error_type.into(),
                code: Some(code.into()),
            },
        }
    }
}

// ── Chat request types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub tools: Option<Vec<ToolSpec>>,
    pub tool_choice: Option<serde_json::Value>,
    /// Optional internal conversation ID for continuation.
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<ChatMessageContent>,
    /// Present on `role: "tool"` messages to pair results with a prior call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Present on `role: "tool"` messages: the function name that produced the result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Present on assistant messages when tool calls were invoked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallObject>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatMessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: serde_json::Value,
}

// ── Chat response types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<UsageInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: AssistantMessage,
    pub finish_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssistantMessage {
    pub role: &'static str, // always "assistant"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallObject>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageInfo {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ── Streaming delta types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoiceDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatChoiceDelta {
    pub index: u32,
    pub delta: DeltaContent,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeltaContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// ── Model aliases ──────────────────────────────────────────────────────────────

/// Parse a model string and validate it is a known gemini-web alias.
///
/// Returns `(canonical_model_name, thinking_level)`.
///
/// Supported aliases:
/// - `gemini-web-flash`
/// - `gemini-web-pro`
/// - `gemini-web-thinking`
/// - `gemini-web-auto`
/// - Any of the above with `@think=N` suffix (N = 0..=4).
pub fn parse_model(model: &str) -> Result<(String, Option<u8>), OpenAiCompatError> {
    const KNOWN: &[&str] = &[
        "gemini-web-flash",
        "gemini-web-pro",
        "gemini-web-thinking",
        "gemini-web-auto",
    ];

    let (base, think) = if let Some((base, suffix)) = model.split_once('@') {
        let level = parse_think_suffix(suffix)
            .ok_or_else(|| OpenAiCompatError::UnknownModel(model.to_owned()))?;
        (base, Some(level))
    } else {
        (model, None)
    };

    if KNOWN.contains(&base) {
        Ok((base.to_owned(), think))
    } else {
        Err(OpenAiCompatError::UnknownModel(model.to_owned()))
    }
}

fn parse_think_suffix(suffix: &str) -> Option<u8> {
    let val = suffix.strip_prefix("think=")?;
    let n: u8 = val.parse().ok()?;
    if n <= 4 { Some(n) } else { None }
}

// ── Builders ──────────────────────────────────────────────────────────────────

pub fn new_completion_id() -> String {
    format!("chatcmpl-{}", Uuid::new_v4().simple())
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_known_aliases() {
        let (name, think) = parse_model("gemini-web-flash").unwrap();
        assert_eq!(name, "gemini-web-flash");
        assert_eq!(think, None);

        let (name, think) = parse_model("gemini-web-pro").unwrap();
        assert_eq!(name, "gemini-web-pro");
        assert_eq!(think, None);

        let (name, think) = parse_model("gemini-web-thinking@think=2").unwrap();
        assert_eq!(name, "gemini-web-thinking");
        assert_eq!(think, Some(2));

        let (name, think) = parse_model("gemini-web-auto@think=0").unwrap();
        assert_eq!(name, "gemini-web-auto");
        assert_eq!(think, Some(0));
    }

    #[test]
    fn parse_model_rejects_unknown() {
        assert!(parse_model("gpt-5").is_err());
        assert!(parse_model("claude-opus").is_err());
        assert!(parse_model("gemini-web-flash@think=9").is_err()); // >4
    }

    #[test]
    fn openai_error_response_serializes_correctly() {
        let err = OpenAiErrorResponse::new("model not found", "invalid_request_error");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["error"]["message"], "model not found");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert!(json["error"]["code"].is_null());
    }

    #[test]
    fn completion_id_starts_with_chatcmpl() {
        let id = new_completion_id();
        assert!(id.starts_with("chatcmpl-"), "got: {id}");
    }

    #[test]
    fn chat_message_content_text_roundtrip() {
        let msg: ChatMessage =
            serde_json::from_str(r#"{"role":"user","content":"hello"}"#).unwrap();
        assert!(matches!(
            &msg.content,
            Some(ChatMessageContent::Text(t)) if t == "hello"
        ));
    }

    #[test]
    fn chat_completion_response_serializes_openai_shape() {
        let resp = ChatCompletionResponse {
            id: "chatcmpl-abc".to_string(),
            object: "chat.completion",
            created: 1_700_000_000,
            model: "gemini-web-flash".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: AssistantMessage {
                    role: "assistant",
                    content: Some("Hello!".to_string()),
                    tool_calls: None,
                },
                finish_reason: "stop".to_string(),
            }],
            usage: Some(UsageInfo {
                prompt_tokens: 5,
                completion_tokens: 3,
                total_tokens: 8,
            }),
            gemini_metadata: None,
        };

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["object"], "chat.completion");
        assert_eq!(json["choices"][0]["message"]["role"], "assistant");
        assert_eq!(json["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        // gemini_metadata should be absent (skip_serializing_if)
        assert!(!json.as_object().unwrap().contains_key("gemini_metadata"));
    }

    #[test]
    fn chat_completion_response_serializes_tool_calls() {
        let resp = ChatCompletionResponse {
            id: "chatcmpl-tool".to_string(),
            object: "chat.completion",
            created: 1_700_000_000,
            model: "gemini-web-flash".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: AssistantMessage {
                    role: "assistant",
                    content: None,
                    tool_calls: Some(vec![ToolCallObject {
                        id: "call_123".to_string(),
                        call_type: "function".to_string(),
                        function: ToolCallFunction {
                            name: "get_weather".to_string(),
                            arguments: r#"{"location":"Paris"}"#.to_string(),
                        },
                    }]),
                },
                finish_reason: "tool_calls".to_string(),
            }],
            usage: None,
            gemini_metadata: None,
        };

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["choices"][0]["finish_reason"], "tool_calls");
        assert!(json["choices"][0]["message"]["content"].is_null());
        let tool_call = &json["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tool_call["id"], "call_123");
        assert_eq!(tool_call["type"], "function");
        assert_eq!(tool_call["function"]["name"], "get_weather");
        assert_eq!(
            tool_call["function"]["arguments"],
            r#"{"location":"Paris"}"#
        );
    }

    #[test]
    fn chat_message_with_tool_call_id_roundtrip() {
        let raw = r#"{"role":"tool","content":"{\"temp\":22}","tool_call_id":"call_123","name":"get_weather"}"#;
        let msg: ChatMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_123"));
        assert_eq!(msg.name.as_deref(), Some("get_weather"));
        assert!(matches!(&msg.content, Some(ChatMessageContent::Text(t)) if t == r#"{"temp":22}"#));
    }
}
