# Module Specification: `code-exec-surface`

**Module ID:** `code-exec-surface`  
**Crate:** `gemini-bridge-code-exec` (`crates/code-exec-surface`)  
**Phase:** Fase 3  
**Depends On:** `gemini-adapter`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-7  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Extract upstream Gemini Web structured execution outputs (`code_execution` stdout, stderr, language, and source) and web search grounding citations. Attach them to responses as an optional `gemini_metadata` field without breaking OpenAI standard schema compatibility.

---

## 2. Public API & Interfaces

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiMetadataExtension {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub code_execution: Vec<ExtractedCodeExecution>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub citations: Vec<ExtractedCitation>,
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedCodeExecution {
    pub language: String,
    pub code: String,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedCitation {
    pub start_index: usize,
    pub end_index: usize,
    pub uri: String,
    pub title: Option<String>,
}

pub fn extract_metadata(response_tree: &serde_json::Value) -> Option<GeminiMetadataExtension>;
```

---

## 3. Behavior & Invariants

1. **Non-Intrusive:** Metadata appears exclusively in the `gemini_metadata` top-level field of the OpenAI response. All standard OpenAI fields remain unmodified.
2. **Safe Extraction:** Missing, partial, or malformed metadata elements in upstream trees are omitted without failing the main response.
3. **Streaming Support:** Cumulative metadata emitted with the final terminal chunk; partial intermediate code output is preserved if upstream emits it incrementally.

---

## 4. Testing Strategy

- Parser tests with upstream payload fixtures containing code execution blocks, citations, both, or neither.
- Snapshot tests verifying standard OpenAI SDKs successfully parse responses containing `gemini_metadata`.
- Stream termination test verifying `gemini_metadata` is attached to the final completion chunk.

---

## 5. Boundaries

- **Always:** Keep extensions in the namespaced `gemini_metadata` field; sanitize URIs before serialization.
- **Ask First:** Promoting metadata fields into the core `choices[0].message` payload.
- **Never:** Break standard OpenAI SDK deserialization; fail a completion request solely because metadata parsing failed.
