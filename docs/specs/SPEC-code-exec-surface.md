# Module Specification: `code-exec-surface`

**Module ID:** `code-exec-surface`  
**Crate:** `gemini-bridge-code-exec` (`crates/code-exec-surface`)  
**Phase:** Fase 3  
**Depends On:** `gemini-adapter`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-7, §4.9
**Status:** Approved Draft — enriched for P.7

---

## 1. Objective & Responsibility

The `code-exec-surface` module extracts structured outputs from Gemini Web's code execution feature and web search grounding results. It attaches them to OpenAI-formatted responses as an optional, non-intrusive `gemini_metadata` extension field, leaving all standard OpenAI schema fields completely unmodified. Downstream clients that do not consume `gemini_metadata` remain fully compatible; clients that do can retrieve structured code execution output and source citations.

**In scope:**
- Parsing the `GeminiWebMetadata` carried in `gemini-adapter`'s `StreamChunk.metadata` and `Completion.metadata` into public typed extension structures.
- Extracting code execution blocks: `language`, `code` (source), `stdout`, and `stderr`.
- Extracting web search grounding citations: character start/end offsets, `uri`, and `title`.
- Attaching extracted results to the `gemini_metadata` field of `ChatCompletionResponse` and `ChatCompletionChunk` (final terminating chunk only for streams).
- Sanitizing citation URIs before serialization.
- Treating all upstream metadata as best-effort: absent, partial, or malformed fields are silently omitted without failing the main completion response.

**Out of scope:**
- Executing code contained in upstream blocks locally (NEVER).
- Audio/TTS extraction or capabilities (explicitly excluded).
- Modifying standard OpenAI completion fields (`choices`, `usage`, `finish_reason`, `content`) for metadata content.
- Upstream request generation or wire protocol (→ `gemini-adapter`).
- HTTP routing, authentication, middleware (→ `http-server`, `middleware`).

---

## 2. Public API & Interfaces

### 2.1 Extension Structures

```rust
use serde::{Deserialize, Serialize};
use url::Url;

/// Top-level metadata extension field appended to OpenAI chat responses.
/// Serialized as `gemini_metadata` at the root of the completion or final chunk.
/// All collections default to empty; absent when no metadata is extracted.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GeminiMetadataExtension {
    /// Code execution blocks from the response, one per model code execution turn.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub code_execution: Vec<ExtractedCodeExecution>,
    /// Grounding/web search citations referenced in the response text.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub citations: Vec<ExtractedCitation>,
    /// Upstream conversation ID for this response, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}

/// A single extracted code execution block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedCodeExecution {
    /// Programming language identifier (e.g. "python", "javascript").
    pub language: String,
    /// Source code submitted for execution.
    pub code: String,
    /// Combined stdout output. Absent if execution produced no output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    /// Combined stderr output. Absent if execution produced no error output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

/// A single web search grounding citation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedCitation {
    /// Character offset of the cited text start within the response `content` string.
    pub start_index: usize,
    /// Character offset of the cited text end (exclusive) within the response `content` string.
    pub end_index: usize,
    /// Sanitized HTTPS URI of the cited source.
    pub uri: String,
    /// Optional source page title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
```

### 2.2 Extraction Functions

```rust
use gemini_bridge_adapter_gemini::{CodeExecutionBlock, CitationRef, GeminiWebMetadata};

/// Extract structured metadata from an adapter-provided `GeminiWebMetadata`.
/// Returns `None` when the metadata object contains no code execution or citations.
/// Silently skips any blocks or citations that fail sanitization or validation.
pub fn extract_metadata(meta: &GeminiWebMetadata) -> Option<GeminiMetadataExtension>;

/// Sanitize a citation URI. Returns the URI string if it is absolute HTTPS or HTTP,
/// rejects `file://`, `javascript:`, data URIs, and other unsafe schemes.
pub fn sanitize_uri(raw: &str) -> Option<String>;
```

---

## 3. Response Attachment Behavior

### 3.1 Non-Streaming Responses

For `POST /v1/chat/completions` (`stream: false`):
```
gemini-adapter returns Completion { ..., metadata: Some(GeminiWebMetadata { ... }) }
       │
       ▼
code-exec-surface::extract_metadata(&metadata) → Some(GeminiMetadataExtension { ... })
       │
       ▼
openai-compat serializes ChatCompletionResponse {
    ...,
    gemini_metadata: Some(serialized GeminiMetadataExtension), // appended
}
```

### 3.2 Streaming Responses

For `POST /v1/chat/completions` (`stream: true`):
- Intermediate chunks (`delta`, partial content) contain only OpenAI standard SSE fields.
- The **final terminal chunk** (identified by `is_finished: true` on the `StreamChunk`) carries the extracted `gemini_metadata` as an additional field on the final `ChatCompletionChunk`.
- Clients that do not expect `gemini_metadata` on the final chunk remain compatible because the field is additive.

---

## 4. Behavior & Invariants

1. **Non-Intrusive Attachment:**
   Standard OpenAI fields (`choices`, `usage`, `finish_reason`, `message.content`, `delta.content`) are **never modified** to embed metadata. `gemini_metadata` appears exclusively at the top level of the completion JSON object.

2. **Safe Best-Effort Extraction:**
   - Missing or null upstream metadata → `gemini_metadata` field omitted from response.
   - Single malformed code block or bad citation URI → that item is silently skipped; remaining valid items are still emitted.
   - Extraction failure never causes the response to fail, return 500, or be downgraded.

3. **URI Sanitization Invariant:**
   Only `https://` and `http://` scheme citations are serialized. `file://`, `javascript:`, `data:`, relative URIs, and any other schemes are rejected by `sanitize_uri` and the citation is silently omitted.

4. **Stream Metadata Timing:**
   Partial/intermediate streaming chunks carry no `gemini_metadata`. A single metadata attachment on the terminal chunk provides the full accumulated set of code execution and citation data from the entire response.

5. **OpenAI SDK Compatibility:**
   The standard OpenAI Python SDK, TypeScript SDK, and JSON parsers must successfully deserialize responses containing a non-null `gemini_metadata` field. Extra unknown fields are ignored by standard compliant JSON parsers.

6. **Audio/TTS Exclusion:**
   No audio output, TTS transcription, or speech-related fields are extracted or surfaced. Upstream audio-related blocks, if ever present, are silently skipped.

---

## 5. Acceptance Criteria & Traceability

### US-7 Traceability (Code Execution & Grounding Surfacing)

| PRD US-7 Acceptance Criterion | Module Specification Coverage |
|---|---|
| Code execution blocks (`code_stdout`) extracted to `gemini_metadata.code_execution[]`, not discarded | `ExtractedCodeExecution` with `language`, `code`, `stdout`, `stderr` fields; invariant 1 |
| Citations/grounding returned in `gemini_metadata.citations[]` | `ExtractedCitation` with `start_index`, `end_index`, `uri`, `title`; URI sanitization invariant |
| Extension fields are optional and do not break OpenAI compatibility | `#[serde(skip_serializing_if)]` on all collections; behavior invariant 1 & 5 |

---

## 6. Testing Strategy

### 6.1 Unit Tests

- **Extraction Tests (Fixture-Based):**
  - Upstream payload with a single code execution block → correct `ExtractedCodeExecution` populated.
  - Upstream payload with multiple code blocks → all blocks extracted in order.
  - Upstream payload with web search citations → `ExtractedCitation` array populated.
  - Both code blocks and citations present → both extracted in same `GeminiMetadataExtension`.
  - Upstream payload with no metadata → `extract_metadata` returns `None`.
  - Partially malformed block (missing `language`) → that block skipped, others extracted.
  - Citation with `file://` URI → citation omitted; `http://` URI → citation included.

- **URI Sanitization Tests:**
  - Valid `https://` → accepted.
  - Valid `http://` → accepted.
  - `file:///etc/passwd` → rejected.
  - `javascript:alert(1)` → rejected.
  - Relative path `../../../etc/passwd` → rejected.
  - `data:text/plain;base64,...` → rejected.

### 6.2 Snapshot Tests (`insta`)

- **Non-streaming**: Record several representative Gemini Web response fixture trees including code execution and citation data; verify the full `ChatCompletionResponse` JSON output matches expected `gemini_metadata` shape.
- **Streaming final chunk**: Verify that the terminal `ChatCompletionChunk` carries `gemini_metadata` and that preceding chunks do not.

### 6.3 OpenAI SDK Compatibility Tests

- Parse a `ChatCompletionResponse` containing `gemini_metadata` using the standard SDK deserialization (via a fixture); assert no deserialization errors and that known OpenAI fields are intact.

### 6.4 Stream Metadata Timing Test

- Use a multi-chunk stream fixture; assert `gemini_metadata` is absent on all intermediate chunks and present (once) on the final chunk.

---

## 7. Boundaries

- **Always:** Attach metadata exclusively to the `gemini_metadata` extension field; sanitize citation URIs before serialization; treat missing or malformed metadata as best-effort and never fail the main response.
- **Ask First:** Promoting metadata fields into core `choices[0].message.content` or `finish_reason`; adding metadata fields beyond code execution and citations.
- **Never:** Execute, evaluate, or relay upstream code locally; return audio/TTS output; cause a completion response to fail solely due to metadata extraction errors; serialize `file://`, `javascript:`, or data URIs in citations; modify standard OpenAI response fields.

---

## 8. Implementation Reference

- **Adapter Metadata Source:** `docs/specs/SPEC-gemini-adapter.md` (`GeminiWebMetadata`, `CodeExecutionBlock`, `CitationRef`, `StreamChunk.metadata`).
- **OpenAI Schema Owner:** `docs/specs/SPEC-openai-compat.md` (`ChatCompletionResponse.gemini_metadata`, `ChatCompletionChunk.gemini_metadata`).
- **Task Implementation:** Task 3.1 (`crates/code-exec-surface/src/lib.rs`, `extractor.rs`, `crates/openai-compat/src/metadata.rs`).
