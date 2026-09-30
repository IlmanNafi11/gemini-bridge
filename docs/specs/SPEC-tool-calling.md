# Module Specification: `tool-calling`

**Module ID:** `tool-calling`  
**Crate:** `gemini-bridge-tool-calling` (`crates/tool-calling`)  
**Phase:** Fase 2  
**Depends On:** `openai-compat` (integrates with `llm-service`)  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-5  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Emulate OpenAI-compatible function/tool calling over Gemini Web. Convert `tools[]` and `tool_choice` into constrained prompt instructions, parse model output into structured `tool_calls[]`, validate the result, and continue the conversation with tool outputs supplied by the client under role `tool`.

---

## 2. Public API & Interfaces

```rust
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolCallError {
    #[error("No valid tool call could be parsed")]
    ParseFailure,
    #[error("Malformed tool schema: {0}")]
    InvalidSchema(String),
    #[error("Tool output is malformed")]
    InvalidOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub r#type: String, // "function"
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

pub fn inject_tool_schema(prompt: &str, tools: &[ToolDefinition]) -> Result<String, ToolCallError>;
pub fn parse_tool_calls(text: &str, tools: &[ToolDefinition]) -> Result<Vec<ToolCall>, ToolCallError>;
```

---

## 3. Behavior & Invariants

1. **Schema Injection:** JSON Schema tool definitions are serialized deterministically and injected into a clearly marked prompt section.
2. **Parsing:** Model output may contain prose surrounding JSON; parser extracts candidate tool-call JSON blocks only when they validate against the supplied schema.
3. **Validation:** Reject unknown function names, malformed JSON, missing required properties, and invalid argument types.
4. **Fallback:** Invalid or ambiguous tool-call output is returned as assistant text with a structured warning; it must not be executed as a tool.
5. **Tool Result Loop:** Client tool outputs sent as role `tool` are normalized and included in the next model request; this bridge never executes arbitrary client-defined tools itself.
6. **Quality Metric:** ≥95% parse success on the designated 50-case test suite.

---

## 4. Testing Strategy

- 50 deterministic model-output fixtures spanning valid calls, multiple calls, escaping, prose, malformed JSON, unknown names, and required-field failures.
- Schema injection snapshots to ensure stable prompt construction.
- End-to-end chat test verifying tool call response followed by client result continuation.
- Invalid parsed results must never produce executable `ToolCall` values.

---

## 5. Boundaries

- **Always:** Validate parsed calls against declared schemas; expose errors/warnings in structured form.
- **Ask First:** Supporting non-function tool types or executing tools inside the bridge.
- **Never:** Execute arbitrary model output, shell commands, or code as a tool; do not accept unknown function names.
