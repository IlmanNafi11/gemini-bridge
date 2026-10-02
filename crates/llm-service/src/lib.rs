//! Provider-neutral LLM contract types, traits, and stream primitives.
//!
//! This crate must not import Gemini, Axum, OpenAI-HTTP, or storage crates.

use gemini_bridge_plugin_context::{PluginGeneration, ReloadableSlot};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use thiserror::Error;

// ── Core types ───────────────────────────────────────────────────────────────

/// Identifies which provider + model to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelector {
    pub provider: String,
    pub model: String,
    pub thinking_level: Option<u8>,
}

/// A single tool/function available to the LLM.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the parameters.
    pub parameters: serde_json::Value,
}

/// A tool invocation the model produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON-encoded arguments string.
    pub arguments: String,
}

/// The result of executing a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
}

/// A role that can author a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// One piece of content within a message.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentPart {
    Text(String),
    /// URI or key referencing an image blob.
    ImageRef(String),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

/// A message in the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub parts: Arc<[ContentPart]>,
}

/// The normalized request sent to any LLM adapter.
///
/// Immutable after construction; shared via `Arc` to avoid copies.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub model: ModelSelector,
    pub messages: Arc<[Message]>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub tools: Arc<[ToolDefinition]>,
    /// Provider-specific extension metadata (provider-neutral fields take precedence).
    pub metadata: BTreeMap<String, serde_json::Value>,
}

// ── Events ───────────────────────────────────────────────────────────────────

/// Raw provider metadata bag.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderMetadata {
    pub raw: serde_json::Value,
}

/// Token usage statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// Summary emitted at stream end.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionSummary {
    pub finish_reason: String,
    pub usage: Option<Usage>,
    pub metadata: Option<ProviderMetadata>,
}

/// A discrete event on the LLM event stream.
#[derive(Debug, Clone, PartialEq)]
pub enum LlmEvent {
    TextDelta(String),
    ToolCall(ToolCall),
    Metadata(ProviderMetadata),
    Completed(CompletionSummary),
}

// ── Non-streaming result ─────────────────────────────────────────────────────

/// The full result of a non-streaming completion call.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub text: String,
    pub finish_reason: String,
    pub usage: Option<Usage>,
    /// Provider response metadata kept opaque at this layer.
    pub metadata: Option<ProviderMetadata>,
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Stable, provider-neutral error taxonomy.
///
/// HTTP / transport details must be converted to these variants at the adapter
/// boundary; they must not leak provider types into higher layers.
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
    #[error("provider rejected conversation continuation identifiers")]
    ContinuityRejected,
    #[error("provider protocol error: {0}")]
    Protocol(String),
}

// ── Stream type alias ─────────────────────────────────────────────────────────

pub type LlmEventStream = Pin<Box<dyn Stream<Item = Result<LlmEvent, LlmError>> + Send>>;

// ── Traits ────────────────────────────────────────────────────────────────────

/// A provider-specific adapter that can satisfy non-stream and stream requests.
#[async_trait]
pub trait LlmAdapter: Send + Sync {
    /// Unique identifier for this provider (e.g. `"gemini-web"`).
    fn provider_id(&self) -> &'static str;

    /// Execute a non-streaming completion and return the normalized result.
    async fn complete(&self, request: Arc<LlmRequest>) -> Result<Completion, LlmError>;

    /// Execute a non-streaming completion and preserve the provider payload.
    ///
    /// Adapters that do not expose structured payloads may use the default,
    /// which wraps the normalized text in a JSON string. Media pipelines use
    /// this hook to extract provider-owned attachment URLs without coupling to
    /// a concrete adapter.
    async fn complete_raw(&self, request: Arc<LlmRequest>) -> Result<serde_json::Value, LlmError> {
        self.complete(request)
            .await
            .map(|completion| serde_json::Value::String(completion.text))
    }

    /// Execute a streaming completion; returns an event stream.
    async fn stream(&self, request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError>;
}

/// Provider-neutral adapter proxy backed by a replaceable built-in generation.
///
/// Each operation acquires an owned generation handle before awaiting adapter
/// work. Streaming therefore retains the adapter that created its stream until
/// the stream is complete, even if the current generation is replaced.
pub struct ReloadableAdapter {
    slot: ReloadableSlot<dyn LlmAdapter>,
}

impl ReloadableAdapter {
    pub fn new(initial: Arc<dyn LlmAdapter>) -> Self {
        Self::from_slot(ReloadableSlot::new(initial))
    }

    /// Create a proxy backed by a slot shared with an administrative reloader.
    pub fn from_slot(slot: ReloadableSlot<dyn LlmAdapter>) -> Self {
        Self { slot }
    }

    /// Access the replacement slot for an administrative reloader.
    pub fn slot(&self) -> ReloadableSlot<dyn LlmAdapter> {
        self.slot.clone()
    }

    /// Prepare a replacement adapter from an async constructor and return a commit closure.
    pub async fn prepare_replacement<F, Fut>(
        &self,
        constructor: F,
    ) -> Result<Box<dyn FnOnce() + Send>, LlmError>
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<Arc<dyn LlmAdapter>, LlmError>> + Send,
    {
        let replacement = constructor().await?;
        let slot = self.slot.clone();
        Ok(Box::new(move || {
            let _old = slot.publish(replacement);
        }))
    }
}

#[async_trait]
impl LlmAdapter for ReloadableAdapter {
    fn provider_id(&self) -> &'static str {
        "reloadable"
    }

    async fn complete(&self, request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        let generation: PluginGeneration<dyn LlmAdapter> = self.slot.get().await;
        generation.complete(request).await
    }

    async fn complete_raw(&self, request: Arc<LlmRequest>) -> Result<serde_json::Value, LlmError> {
        let generation: PluginGeneration<dyn LlmAdapter> = self.slot.get().await;
        generation.complete_raw(request).await
    }

    async fn stream(&self, request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        let generation: PluginGeneration<dyn LlmAdapter> = self.slot.get().await;
        let stream = generation.stream(request).await?;
        let retained_stream = stream.map(move |event| {
            let _generation = &generation;
            event
        });
        Ok(Box::pin(retained_stream))
    }
}

/// Selects the correct adapter for a given model selector.
pub trait LlmRouter: Send + Sync {
    fn adapter_for(&self, model: &ModelSelector) -> Result<Arc<dyn LlmAdapter>, LlmError>;
}

/// Thread-safe registry that routes normalized requests to provider adapters.
///
/// The registry is intentionally keyed only by the neutral provider identifier;
/// concrete adapters remain outside this crate and are supplied by the
/// composition root.
#[derive(Clone, Default)]
pub struct AdapterRegistry {
    adapters: Arc<RwLock<HashMap<&'static str, Arc<dyn LlmAdapter>>>>,
}

impl AdapterRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an adapter under its stable provider identifier.
    ///
    /// Re-registering a provider replaces the previous adapter, which keeps
    /// instance replacement deterministic for composition and test setup.
    pub fn register(&self, adapter: Arc<dyn LlmAdapter>) {
        let provider_id = adapter.provider_id();
        self.adapters
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(provider_id, adapter);
    }

    /// Return the adapter selected by a normalized model selector.
    pub fn adapter_for_provider(
        &self,
        model: &ModelSelector,
    ) -> Result<Arc<dyn LlmAdapter>, LlmError> {
        self.adapters
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(model.provider.as_str())
            .cloned()
            .ok_or(LlmError::Unsupported("provider"))
    }
}

#[async_trait]
impl LlmAdapter for AdapterRegistry {
    fn provider_id(&self) -> &'static str {
        "registry"
    }

    async fn complete(&self, request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        self.adapter_for_provider(&request.model)?
            .complete(request)
            .await
    }

    async fn complete_raw(&self, request: Arc<LlmRequest>) -> Result<serde_json::Value, LlmError> {
        self.adapter_for_provider(&request.model)?
            .complete_raw(request)
            .await
    }

    async fn stream(&self, request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        self.adapter_for_provider(&request.model)?
            .stream(request)
            .await
    }
}

impl LlmRouter for AdapterRegistry {
    fn adapter_for(&self, model: &ModelSelector) -> Result<Arc<dyn LlmAdapter>, LlmError> {
        self.adapter_for_provider(model)
    }
}
