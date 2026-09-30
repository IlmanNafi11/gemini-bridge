# Module Specification: `gemini-adapter`

**Module ID:** `gemini-adapter`  
**Crate:** `gemini-bridge-adapter-gemini` (`crates/gemini-adapter`)  
**Phase:** Fase 0 (non-stream), Fase 1 (stream/prefix-diff/405 recovery)  
**Depends On:** `identity`, `transport`, `config`  
**Parent Spec:** `SPEC.md` §2.1, §4.9; PRD §3.1, §4.3, §4.9  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Translate normalized LLM requests into Gemini Web wire protocol interactions (`StreamGenerate`, `batchexecute`, positional `f.req` envelopes, `hl`, `_reqid`, `rt=c`, and headers). Parse newline-framed upstream payloads and candidate response trees; compute incremental streaming deltas via prefix difference; map upstream errors (`405`, `429`, `401`); and drive externalized schema indexes from `schema/gemini-web.toml`.

This module isolates Gemini Web specifics. It depends on `llm-service` data contracts, not the reverse.

---

## 2. Public API & Interfaces

```rust
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
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

---

## 4. Testing Strategy

- **Snapshot Testing (`insta`):** Parse recorded real Gemini Web response trees and verify exact chunk extraction.
- **Prefix-Diff Tests:** Verify correct suffix emission on monotonic growth and proper safe-reset on rewritten prefixes.
- **Resilience Tests (`wiremock`):**
  - Upstream `405` triggers session re-bootstrap and succeeds on second attempt.
  - Upstream `429` yields mapped rate-limit error.
  - Malformed payload produces `SchemaMismatch` without panic.
- **Quality Gate:** 100% test pass on clippy and snapshot suites.

---

## 5. Boundaries

- **Always:** Use externalized `schema/gemini-web.toml` for positional arrays; redact secret headers; bound retries to 1 on 405.
- **Ask First:** Changing schema file format or adding new upstream RPC endpoints.
- **Never:** Hardcode magic array indices across codebase; never swallow upstream errors silently.
