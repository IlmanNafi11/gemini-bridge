# Module Specification: `llm-service`

**Module ID:** `llm-service`  
**Crate:** `gemini-bridge-llm-service` (`crates/llm-service`)  
**Phase:** Fase 0 (contract), Fase 3 (reloadable adapter generations)
**Depends On:** `plugin-context`  
**Parent Spec:** `SPEC.md` §2.1, PRD §4.1  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Provide provider-neutral request, response, stream event, error, adapter, and routing contracts for chat and media-capable LLM providers. `openai-compat` maps external requests into this contract; provider adapters implement it; the composition root registers the selected router/adapter through `plugin-context`. The module must not contain Gemini field positions, cookies, URL paths, OpenAI HTTP details, or storage behavior.

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
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
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
pub struct ProviderMetadata {
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionSummary {
    pub finish_reason: String,
    pub usage: Option<Usage>,
}


#[derive(Debug, Clone, PartialEq)]
pub enum LlmEvent {
    TextDelta(String),
    ToolCall(ToolCall),
    Metadata(ProviderMetadata),
    Completed(CompletionSummary),
}
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub text: String,
    pub finish_reason: String,
    pub usage: Option<Usage>,
    /// Opaque optional provider extension, decoded only by provider-specific surface modules.
    pub metadata: Option<ProviderMetadata>,
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

pub struct ReloadableAdapter;
impl ReloadableAdapter {
    pub fn new(initial: Arc<dyn LlmAdapter>) -> Self;
    pub fn from_slot(slot: ReloadableSlot<dyn LlmAdapter>) -> Self;
    pub fn slot(&self) -> ReloadableSlot<dyn LlmAdapter>;
    pub async fn prepare_replacement<F, Fut>(&self, constructor: F) -> Result<Box<dyn FnOnce() + Send>, LlmError>
    where F: FnOnce() -> Fut + Send, Fut: Future<Output = Result<Arc<dyn LlmAdapter>, LlmError>> + Send;
}

pub trait LlmRouter: Send + Sync {
    fn adapter_for(&self, model: &ModelSelector) -> Result<Arc<dyn LlmAdapter>, LlmError>;
}
```

---

## 3. Behavior & Invariants

1. Normalized requests are immutable after construction and shared by `Arc` without clone-heavy transformation.
2. Streaming uses neutral semantic events, not SSE strings or provider response frames, and emits one terminal `Completed` event on successful completion.
3. Provider errors map into the stable `LlmError` taxonomy; HTTP code mapping remains in the `openai-compat`/server boundary.
4. Metadata accepts optional provider extensions while core message/result fields remain provider-neutral.
5. Adding another adapter must not require changing the `LlmRequest`/`LlmEvent` core for provider-specific details.
6. Dependency direction is `plugin-context` → `llm-service` contract registration and provider adapter → `llm-service`; `llm-service` never imports a provider adapter, `openai-compat`, or the HTTP server.
7. `ReloadableAdapter` clones a generation handle before awaiting work; returned streams retain that handle until completion. Replacement construction completes before synchronous atomic publication, so timed-out preparation cannot expose a partial generation.

---

## 4. Acceptance Criteria

1. One immutable `Arc<LlmRequest>` can be passed to non-streaming and streaming mock adapters without provider-specific conversion or mutation.
2. A mock adapter returns a full `Completion` and an ordered stream of neutral text/tool/metadata/completion events through the same public contract used by production adapters.
3. `LlmRouter` selects an adapter from `ModelSelector` and returns a stable provider-neutral error for an unknown or unavailable provider.
4. Authentication, rate-limit, unavailability, unsupported-capability, and protocol failures can be represented without importing provider or HTTP error types.
5. A second adapter can implement and register the contract without modifications to request/event core types or dependencies from `llm-service` back to either adapter.

---

## 5. Testing Strategy

- Contract tests with a mock adapter for non-stream completion and stream event ordering/terminal behavior.
- Request immutability and model-routing tests, including unknown providers.
- Error taxonomy tests verifying provider failures map without leaking provider types.
- Dependency-boundary review verifies the crate contains no Gemini, Axum/OpenAI HTTP, credential, or storage imports.
- Fase 3 reload tests prove new requests use the new generation while an in-flight stream completes on its original generation.
- Fase 3 multi-adapter test proves the same request path can route to a second adapter.

---

## 6. Boundaries

- **Always:** Keep public data structures provider-neutral, keep request objects immutable, and expose provider selection through `LlmRouter`.
- **Ask First:** Adding a core field needed by only one provider or changing stable error variants/events.
- **Never:** Import Gemini adapter, Axum/OpenAI HTTP models, credential, or storage crates, or make this crate depend on any concrete provider.
