# Module Specification: `llm-service`

**Module ID:** `llm-service`  
**Crate:** `gemini-bridge-llm-service` (`crates/llm-service`)  
**Phase:** Fase 0  
**Depends On:** `plugin-context`  
**Parent Spec:** `SPEC.md` §2.1, PRD §4.1  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Provide provider-neutral request, response, stream event, error, and adapter contracts for chat and media-capable LLM providers. `openai-compat` maps external requests into this contract; provider adapters implement it. The module must not contain Gemini field positions, cookies, URL paths, OpenAI HTTP details, or storage behavior.

---

## 2. Public API & Interfaces

```rust
use async_trait::async_trait;
use futures::Stream;
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub model: ModelSelector,
    pub messages: Arc<[Message]>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub tools: Arc<[ToolDefinition]>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role { System, User, Assistant, Tool }

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub parts: Arc<[ContentPart]>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContentPart {
    Text(String),
    ImageRef(String),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelector {
    pub provider: String,
    pub model: String,
    pub thinking_level: Option<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LlmEvent {
    TextDelta(String),
    ToolCall(ToolCall),
    Metadata(ProviderMetadata),
    Completed(CompletionSummary),
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("authentication required")]
    Authentication,
    #[error("upstream rate limited")]
    RateLimited,
    #[error("provider unavailable")]
    Unavailable,
    #[error("unsupported capability: {0}")]
    Unsupported(&'static str),
    #[error("provider protocol error: {0}")]
    Protocol(String),
}

pub type LlmEventStream = Pin<Box<dyn Stream<Item = Result<LlmEvent, LlmError>> + Send>>;

#[async_trait]
pub trait LlmAdapter: Send + Sync {
    fn provider_id(&self) -> &'static str;
    async fn complete(&self, request: Arc<LlmRequest>) -> Result<Completion, LlmError>;
    async fn stream(&self, request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError>;
}

pub trait LlmRouter: Send + Sync {
    fn adapter_for(&self, model: &ModelSelector) -> Result<Arc<dyn LlmAdapter>, LlmError>;
}
```

---

## 3. Behavior & Invariants

1. Normalized requests are immutable after construction and shared by `Arc` without clone-heavy transformation.
2. Streaming uses neutral semantic events, not SSE strings or Gemini response frames.
3. Provider errors map into the stable `LlmError` taxonomy; HTTP code mapping remains in `openai-compat`/server boundary.
4. Metadata accepts optional provider extensions while core message/result fields remain provider-neutral.
5. Adding another adapter must not require changing the `LlmRequest`/`LlmEvent` core for provider-specific details.

---

## 4. Testing Strategy

- Contract tests with a mock adapter for non-stream and stream event ordering.
- Request immutability and model-routing tests.
- Error taxonomy tests verifying provider errors can map without leaking provider types.
- Fase 3 multi-adapter test proves the same request path can route to a second adapter.

---

## 5. Boundaries

- **Always:** Keep public data structures provider-neutral and request objects immutable.
- **Ask First:** Adding a core field needed by only one provider or changing stable error variants.
- **Never:** Import Gemini adapter, Axum/OpenAI HTTP models, credential, or storage crates.
