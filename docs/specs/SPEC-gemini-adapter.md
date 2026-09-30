# Module Specification: `gemini-adapter`

**Module ID:** `gemini-adapter`  
**Crate:** `gemini-bridge-adapter-gemini` (`crates/gemini-adapter`)  
**Phase:** Fase 0 (non-stream), Fase 1 (stream/prefix-diff/405 recovery)  
**Depends On:** `llm-service`, `identity`, `transport`, `config`  
**Parent Spec:** `SPEC.md` §2.1, §4.9; PRD §3.1, §4.3, §4.9  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Translate provider-neutral `llm-service` requests into Gemini Web wire protocol interactions (`StreamGenerate`, `batchexecute`, positional `f.req` envelopes, `hl`, `_reqid`, `rt=c`, and headers). Parse newline-framed upstream payloads and candidate response trees; compute incremental streaming deltas via prefix difference; map upstream errors (`405`, `429`, `401`); and drive externalized schema indexes from `schema/gemini-web.toml`.

This module is the only owner of Gemini Web wire details. It implements the provider-neutral `LlmAdapter` contract from `llm-service`; `llm-service` never imports this crate, so the dependency remains acyclic.

---

## 2. Public API & Interfaces

```rust
use async_trait::async_trait;
use futures::Stream;
use gemini_bridge_llm_service::{Completion, LlmAdapter, LlmEventStream, LlmRequest};
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GeminiAdapterError {
    #[error("Upstream returned 405 Method Not Allowed / Stale BL")]
    StaleBuildLabel,
    #[error("Upstream rate limited (429)")]
    RateLimited,
    #[error("Session authentication required (401/expired)")]
    NeedsAuth,
    #[error("Upstream schema parsing failed: {0}")]
    SchemaMismatch(String),
    #[error("Gemini Web flagged IP / bot protection")]
    IpFlagged,
    #[error("Network/transport failure: {0}")]
    Transport(String),
}

pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<StreamChunk, GeminiAdapterError>> + Send>>;
pub type NormalizedLlmRequest = Arc<LlmRequest>;
pub type NormalizedLlmResponse = Completion;


pub struct StreamChunk {
    pub delta_text: Option<String>,
    pub is_finished: bool,
    pub finish_reason: Option<String>,
    pub metadata: Option<GeminiWebMetadata>,
}

pub struct GeminiWebMetadata {
    pub conversation_id: Option<String>,
    pub response_id: Option<String>,
    pub candidate_id: Option<String>,
    pub code_execution: Vec<CodeExecutionBlock>,
    pub citations: Vec<CitationRef>,
}

#[derive(Debug, Clone)]
pub struct CodeExecutionBlock {
    pub language: String,
    pub code: String,
    pub output: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CitationRef {
    pub start_index: usize,
    pub end_index: usize,
    pub uri: String,
    pub title: Option<String>,
}

#[async_trait]
pub trait GeminiAdapter: Send + Sync {
    async fn generate_non_stream(&self, req: NormalizedLlmRequest) -> Result<NormalizedLlmResponse, GeminiAdapterError>;
    async fn generate_stream(&self, req: NormalizedLlmRequest) -> Result<ChunkStream, GeminiAdapterError>;
}

#[async_trait]
impl LlmAdapter for DefaultGeminiAdapter {
    fn provider_id(&self) -> &'static str;
    async fn complete(&self, request: Arc<LlmRequest>) -> Result<Completion, LlmError>;
    async fn stream(&self, request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError>;
}
```

---

## 3. Behavior & Invariants

1. **Positional Payload Generation:** `f.req` positional arrays must use externalized indices defined in `schema/gemini-web.toml`.
2. **Schema Self-Check:** Startup and periodic lightweight probes validate indices against response shapes. If structural drift is detected, transition to `Degraded` mode with actionable logging.
3. **Prefix-Diff Engine:**
   - Parse cumulative text from streaming chunks.
   - Emit only new suffix (`current_text[last_len..]`).
   - If upstream rewrites earlier text (e.g. thinking model branch revisions), safe reset resets the buffer and emits a replacement delta.
4. **405 Auto-Recovery:** Detect stale build label (`bl`), trigger single background bootstrap refresh via `identity`, and retry the request once silently. Never loop retries on 405.
5. **Error Mapping:** Upstream status `429` maps to `RateLimited`; `302` to `sorry/index` maps to `IpFlagged`; unauthorized/expired session maps to `NeedsAuth`.
6. **Dependency Direction:** Public consumers route through `llm_service::LlmAdapter`; Gemini-specific request framing, response trees, metadata extraction, and error types do not leak into `llm-service`.

---

## 4. Acceptance Criteria

1. A normalized request fixture serializes into the configured Gemini Web `f.req` envelope with required query fields and authenticated headers, without embedding positional indexes outside `schema/gemini-web.toml`.
2. Representative non-stream and newline-framed stream fixtures parse into provider-neutral completions/events; malformed frames and schema drift return a typed protocol error without panic.
3. Monotonic cumulative snapshots emit suffix-only deltas, and a non-prefix rewrite emits the specified reset/replacement behavior before streaming completes exactly once.
4. Upstream authentication, rate-limit, stale-build-label, flagged-IP, and transport failures map deterministically into the stable `LlmError` taxonomy.
5. A stale build label triggers at most one identity bootstrap refresh and one request retry; other failures do not enter a retry loop.
6. The crate depends on `llm-service` to implement `LlmAdapter`; `llm-service` has no dependency on this crate or on Gemini-specific types.

---

## 5. Testing Strategy

- **Snapshot Testing (`insta`):** Parse sanitized recorded Gemini Web response trees and verify exact completion/event extraction.
- **Wire Contract Tests:** Verify query parameters, byte-preserved `f.req` form body, schema-index use, identity-produced auth headers, and model/thinking selection.
- **Prefix-Diff Tests:** Verify suffix emission on monotonic growth and safe reset on rewritten prefixes.
- **Resilience Tests (`wiremock`):**
  - Upstream `405` triggers one session re-bootstrap and succeeds on the second attempt.
  - Repeated `405`, upstream `429`, authentication failure, and flagged-IP responses yield their mapped errors without extra requests.
  - Malformed payload produces `SchemaMismatch` without panic.
- **Quality Gate:** All focused tests and workspace clippy pass with zero warnings.

---

## 6. Boundaries

- **Always:** Use externalized `schema/gemini-web.toml` for positional arrays, accept/return provider-neutral service types at the application boundary, redact secret headers, and bound retries to one on 405.
- **Ask First:** Changing schema file format, adding new upstream RPC endpoints, or exposing a Gemini-only field in the neutral service contract.
- **Never:** Hardcode magic array indices across the codebase, make `llm-service` depend on this crate, or swallow upstream errors silently.
