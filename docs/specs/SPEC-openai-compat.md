# Module Specification: `openai-compat`

**Module ID:** `openai-compat`  
**Crate:** `gemini-bridge-openai-compat` (`crates/openai-compat`)  
**Phase:** Fase 0 (core completions/models), Fase 1 (images/files), Fase 2 (tools/conversations), Fase 3 (metadata)  
**Depends On:** `llm-service`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-1 (full), US-2 (partial: image schema), US-3 (partial: file types), US-5 (partial: tool schema), §4.5  
**Status:** Approved Draft — enriched for P.4

---

## 1. Objective & Responsibility

Owns all OpenAI REST API JSON schemas and normalization logic for `gemini-bridge`. This crate translates between the wire representation of OpenAI-shaped requests/responses and the neutral `llm-service` contract. It does **not** import Axum, reqwest, or any provider-specific crate.

**In scope:**
- Request deserialization: `/v1/chat/completions`, `/v1/models`, `/v1/images/generations`, `/v1/files`
- Response serialization: completion, stream chunk, image, file object, model list, error envelope
- Model alias parsing and `@think=N` suffix routing
- Error mapping: provider `LlmError` variants → OpenAI HTTP status + JSON body

**Out of scope:**
- Route wiring, SSE transport, auth enforcement (→ `http-server`)
- Actual LLM calls, provider protocol, upstream session (→ `gemini-adapter`)
- Rate limiting, request tracing, secret redaction (→ `middleware`)

---

## 2. Public API & Interfaces

### 2.1 Chat

```rust
// ── Request ────────────────────────────────────────────────────────────────────
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,                          // "system" | "user" | "assistant" | "tool"
    pub content: Option<ChatMessageContent>,
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
    pub part_type: String,                     // "text" | "image_url"
    pub text: Option<String>,
    pub image_url: Option<ImageUrl>,           // added Fase 1 (upload)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: serde_json::Value,
}

// ── Non-streaming response ─────────────────────────────────────────────────────
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,                            // "chatcmpl-{uuid}"
    pub object: &'static str,                 // "chat.completion"
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<UsageInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini_metadata: Option<serde_json::Value>, // Fase 3 extension
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: AssistantMessage,
    pub finish_reason: String,                 // "stop" | "length" | "tool_calls"
}

#[derive(Debug, Clone, Serialize)]
pub struct AssistantMessage {
    pub role: &'static str,                   // always "assistant"
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>, // Fase 2 extension
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageInfo {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ── Streaming delta ───────────────────────────────────────────────────────────
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: &'static str,                 // "chat.completion.chunk"
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoiceDelta>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatChoiceDelta {
    pub index: u32,
    pub delta: DeltaContent,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeltaContent {
    pub role: Option<&'static str>,
    pub content: Option<String>,
}
```

### 2.2 Models

```rust
pub struct ModelObject {
    pub id: &'static str,
    pub object: &'static str,                 // "model"
    pub created: i64,
    pub owned_by: &'static str,               // "google"
}

pub struct ModelList {
    pub object: &'static str,                 // "list"
    pub data: Vec<ModelObject>,
}

pub const VIRTUAL_MODELS: &[&str] = &[
    "gemini-web-flash",
    "gemini-web-pro",
    "gemini-web-thinking",
    "gemini-web-auto",
];

pub fn list_models() -> ModelList;
```

### 2.3 Images (Fase 1)

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ImageGenerationRequest {
    pub prompt: String,
    pub n: Option<u32>,                        // default 1
    pub size: Option<String>,                  // ignored for Gemini Web; pass-through
    pub response_format: Option<String>,       // "url" | "b64_json"; default "url"
    pub reference_images: Option<Vec<String>>, // URL or base64 data URIs
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageGenerationResponse {
    pub created: i64,
    pub data: Vec<ImageObject>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageObject {
    pub url: Option<String>,
    pub b64_json: Option<String>,
}
```

### 2.4 Files (Fase 1)

```rust
#[derive(Debug, Clone, Serialize)]
pub struct FileObject {
    pub id: String,
    pub object: &'static str,                 // "file"
    pub bytes: u64,
    pub created_at: i64,
    pub filename: String,
    pub purpose: &'static str,                // "assistants"
}
```

### 2.5 Error Envelope

```rust
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
    pub fn new(message: impl Into<String>, error_type: &'static str) -> Self;
    pub fn with_code(message: impl Into<String>, error_type: &'static str, code: &'static str) -> Self;
}
```

### 2.6 Utility Functions

```rust
pub fn parse_model(model: &str) -> Result<(String, Option<u8>), OpenAiCompatError>;
// Returns (canonical_alias, thinking_level 0..=4)
// Valid aliases: gemini-web-flash, gemini-web-pro, gemini-web-thinking, gemini-web-auto
// @think=N suffix: N in 0..=4 only; anything else → UnknownModel

pub fn new_completion_id() -> String;  // "chatcmpl-{uuid}"
pub fn unix_now() -> i64;              // seconds since UNIX epoch
```

### 2.7 Errors

```rust
#[derive(Debug, Error)]
pub enum OpenAiCompatError {
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}
```

---

## 3. Behavior & Invariants

1. **Schema Compliance:** Non-streaming responses always carry `choices[0].message.content`, `finish_reason`, and `usage`; streaming chunks carry `choices[0].delta.content` or `choices[0].finish_reason`.

2. **Stream Frame Format:** Each SSE data line is `data: <JSON>\n\n`; the terminal sentinel is `data: [DONE]\n\n`. No bare text lines.

3. **Model Routing:** `parse_model` accepts the four virtual aliases with optional `@think=N` (N ∈ 0–4). Unknown aliases return `UnknownModel`; callers must surface this as HTTP 400.

4. **Non-Intrusive Extensions:** `gemini_metadata`, `citations`, and `tool_calls` are optional fields serialized with `#[serde(skip_serializing_if = "Option::is_none")]` so standard OpenAI SDKs are unaffected.

5. **Error Mapping Contract (reference for `http-server`):**

   | Provider error | HTTP status | `type` | `code` |
   |---|---|---|---|
   | `LlmError::Authentication` | 401 | `authentication_error` | `invalid_api_key` |
   | `LlmError::RateLimited` | 429 | `rate_limit_exceeded` | `rate_limit_exceeded` |
   | `LlmError::Unavailable` | 503 | `service_unavailable` | — |
   | `LlmError::Unsupported(_)` | 400 | `invalid_request_error` | — |
   | `LlmError::Protocol(_)` | 502 | `provider_error` | — |
   | Unknown model | 400 | `invalid_request_error` | `model_not_found` |

6. **No Side Effects:** This crate performs pure serialization/deserialization and computation. It makes no I/O calls.

---

## 4. Acceptance Criteria

### US-1 Traceability (Chat)

| US-1 AC | Covered by |
|---|---|
| POST `/v1/chat/completions` accepts `model`, `messages[]`, `stream`, `temperature`, `max_tokens` | `ChatCompletionRequest` fields + `parse_model` |
| Non-stream response: `choices[0].message.content`, `finish_reason`, `usage` | `ChatCompletionResponse` / `ChatChoice` invariant |
| Stream: SSE `data: {...}` + `data: [DONE]` | `ChatCompletionChunk` + stream frame format (§3.2) |
| Model aliases: `gemini-web-flash`, `-pro`, `-thinking`, `-auto`, `@think=0..4` | `parse_model` + `VIRTUAL_MODELS` |
| Error mapping: 429, 503, 405→retry (handled in http-server), 401 | Error mapping table (§3.5) |

### Acceptance criteria (module-level)

1. `parse_model` returns the correct canonical alias and thinking level for all four base aliases and for `@think=0`, `@think=4`; rejects `@think=5`, unknown aliases, and malformed suffixes with `UnknownModel`.
2. `ChatCompletionResponse` serializes to OpenAI JSON shape: `object: "chat.completion"`, `choices[0].message.role: "assistant"`, `finish_reason` present, `gemini_metadata` absent when `None`.
3. `ChatCompletionChunk` serializes to `object: "chat.completion.chunk"` with `choices[0].delta.content` for text events and `finish_reason` for terminal events.
4. `OpenAiErrorResponse` JSON always has `error.message`, `error.type`; `error.code` is null when not set.
5. `list_models()` returns all four virtual model IDs; each entry has `object: "model"` and `owned_by: "google"`.
6. Round-trip: a request JSON accepted by the OpenAI Python SDK can be deserialized into `ChatCompletionRequest` without loss of `messages`, `stream`, `temperature`, `max_tokens`.

---

## 5. Testing Strategy

- **Model parsing matrix:** All four base aliases, all valid `@think=N` levels (0–4), boundary `@think=5` (rejects), unknown strings (rejects), malformed suffix.
- **Serialization roundtrip:** `ChatCompletionResponse` and `ChatCompletionChunk` against OpenAI SDK fixture JSONs; assert exact field names (`choices[0].message.content`, `finish_reason`, etc.).
- **Error envelope:** All six error mapping variants serialize correctly; `code` is null-absent when not provided.
- **`gemini_metadata` gating:** Field absent from JSON when `None`; present and correct when `Some(...)`.
- **`new_completion_id`:** Starts with `chatcmpl-`; every call produces a distinct value.
- **Image/file schema (Fase 1):** `ImageGenerationResponse` serializes with `data[0].url` or `data[0].b64_json` exclusively.

---

## 6. Boundaries

- **Always:** Preserve full OpenAI field compatibility; validate required fields; return standard error envelopes; never import Axum or provider crates.
- **Ask First:** Adding non-standard request parameters; modifying default model alias mapping; extending error type strings.
- **Never:** Break standard OpenAI SDK deserialization; return plain text error strings instead of JSON objects; perform I/O.
