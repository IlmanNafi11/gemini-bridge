# Module Specification: `openai-compat`

**Module ID:** `openai-compat`  
**Crate:** `gemini-bridge-openai-compat` (`crates/openai-compat`)  
**Phase:** Fase 0 (core completions/models), Fase 1 (images/files), Fase 2 (tools/conversations), Fase 3 (metadata)  
**Depends On:** `llm-service`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-1, §4.5  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Implement OpenAI API JSON schemas and normalization for `/v1/chat/completions`, `/v1/models`, `/v1/images/generations`, and `/v1/files`. Map model identifiers (e.g., `gemini-web-flash`, `gemini-web-pro`, `gemini-web-thinking`, `gemini-web-auto` with optional `@think=N`), format OpenAI chat choices/deltas, and map provider errors into standard OpenAI error envelopes.

---

## 2. Public API & Interfaces

```rust
use serde::{Deserialize, Serialize};

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
    pub role: String,
    pub content: Option<ChatMessageContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatMessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

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
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoiceDelta>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAIErrorResponse {
    pub error: OpenAIErrorDetails,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAIErrorDetails {
    pub message: String,
    pub r#type: String,
    pub code: Option<String>,
}
```

---

## 3. Behavior & Invariants

1. **Schema Compliance:** Responses strictly adhere to standard OpenAI shapes (`choices[0].message.content`, `finish_reason`, `usage`).
2. **Streaming Delta Format:** Stream chunks emit `data: {"choices":[{"delta":{...}}]}` with terminal `data: [DONE]`.
3. **Model Routing:** Model strings map to the neutral `ModelSelector`. Suffix `@think=0..4` maps to the thinking level.
4. **Non-Intrusive Extensions:** Non-standard fields (`gemini_metadata`, citations, code execution) appear under namespaced fields that standard SDKs ignore.
5. **Error Mapping:** Upstream `401` -> `invalid_api_key` / `authentication_error`; `429` -> `rate_limit_exceeded`; `503` -> `service_unavailable`.

---

## 4. Testing Strategy

- Round-trip serialization tests against OpenAI Python/Node SDK fixture JSONs.
- Model string parsing matrix (`gemini-web-pro@think=2`, `gemini-web-auto`).
- SSE stream formatter tests ensuring valid line framing and `[DONE]` termination.
- Error response serialization matching standard OpenAI HTTP error schema.

---

## 5. Boundaries

- **Always:** Preserve full OpenAI field compatibility; validate required fields; return standard error envelopes.
- **Ask First:** Adding non-standard request parameters or modifying default model alias mapping.
- **Never:** Break standard OpenAI SDK deserialization; never return plain text error strings instead of JSON error objects.
