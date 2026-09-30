# Module Specification: `tool-calling`

**Module ID:** `tool-calling`  
**Crate:** `gemini-bridge-tool-calling` (`crates/tool-calling`)  
**Phase:** Fase 2  
**Depends On:** `openai-compat` (declared crate dependency; consumes canonical `llm-service` types)
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-5, §1.3 KPI 5, §3.2 E1 Golden Suite
**Status:** Approved Draft — enriched for P.7

---

## 1. Objective & Responsibility

The `tool-calling` module emulates OpenAI-compatible function and tool calling over Gemini Web. It translates client-supplied `tools[]` (JSON Schema function definitions) and `tool_choice` directives into constrained prompt instructions, parses candidate model output into structured `tool_calls[]` objects, validates argument syntax and types against declared schemas, and formats client-provided tool execution outputs (`role: "tool"`) for continuation turns.

**In scope:**
- Injecting formatted JSON Schema tool definitions into user/system prompts with unambiguous delimiters and formatting constraints.
- Parsing model text output into structured `Vec<ToolCall>` objects, supporting fenced code blocks (` ```json `), inline JSON objects, multiple calls, and surrounding conversational text.
- Validating candidate tool calls against registered function schemas (verifying function names, required properties, and JSON types).
- Non-executable fallback: Converting malformed or schema-violating tool-call outputs into regular text assistant responses accompanied by a structured warning, ensuring invalid calls are never emitted as valid executable tool calls.
- Normalizing and formatting client tool outputs submitted under `role: "tool"` with matching `tool_call_id` into multi-turn context for subsequent model queries.
- Achieving a benchmark success rate of ≥95% across the standard 50-case test suite (where expected behavior is either a clean parsed `ToolCall` or an exact fallback with structured warning).

**Out of scope:**
- Local execution of functions, tools, shell commands, scripts, or network requests (the bridge NEVER executes client-defined tools).
- Gemini Web wire-level `f.req` construction or network transport (→ `gemini-adapter`, `transport`).
- HTTP server routing, authentication, or middleware processing (→ `http-server`, `middleware`).
- Audio, speech, TTS, or unrelated multimodal capabilities (explicitly excluded).

---

## 2. Public API & Interfaces

### 2.1 Canonical Type Re-exports and Directives

The `tool-calling` module builds upon canonical types defined in `gemini-bridge-llm-service` and `gemini-bridge-openai-compat`:

```rust
use gemini_bridge_llm_service::{ToolCall, ToolDefinition, ToolResult};
use gemini_bridge_openai_compat::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Validated directive controlling tool invocation behavior, parsed from OpenAI `tool_choice`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParsedToolChoice {
    /// Omit tool injection entirely or disable invocation ("none").
    None,
    /// Allow model to choose whether to call a tool or reply with text ("auto", default).
    Auto,
    /// Force model to invoke at least one tool ("required").
    Required,
    /// Force model to invoke a specific named function, e.g. `{"type": "function", "function": {"name": "get_weather"}}`.
    Specific { function_name: String },
}

impl ParsedToolChoice {
    /// Parse from an OpenAI `tool_choice` JSON value (`"none"`, `"auto"`, `"required"`, or object).
    pub fn from_value(val: Option<&Value>) -> Result<Self, ToolCallError>;
}

/// Diagnostic warning emitted when model output attempted a tool call but failed validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallWarning {
    pub code: String,
    pub message: String,
    pub raw_candidate: Option<String>,
}

/// Result of parsing model output text for tool calls.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedToolResult {
    /// Successfully parsed and validated one or more tool calls.
    ToolCalls {
        calls: Vec<ToolCall>,
        text_prefix: Option<String>,
    },
    /// Output contains plain conversational text or failed validation (with structured warning).
    PlainContent {
        content: String,
        warning: Option<ToolCallWarning>,
    },
}
```

### 2.2 Error Types

```rust
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum ToolCallError {
    #[error("Tool schema validation failed for '{0}': {1}")]
    InvalidSchema(String, String),

    #[error("Unsupported tool choice format: {0}")]
    InvalidToolChoice(String),

    #[error("Unknown function name: '{0}'")]
    UnknownFunction(String),

    #[error("Malformed JSON in tool call arguments: {0}")]
    MalformedArguments(String),

    #[error("Arguments failed schema validation for '{0}': {1}")]
    SchemaMismatch(String, String),

    #[error("Prompt injection detected in tool schema definition: {0}")]
    InjectionError(String),
}
```

### 2.3 Core Trait

```rust
pub trait ToolEngine: Send + Sync {
    /// Convert OpenAI `ToolSpec` items or neutral `ToolDefinition`s into prompt instructions.
    fn inject_tool_schema(
        &self,
        base_prompt: &str,
        tools: &[ToolDefinition],
        choice: &ParsedToolChoice,
    ) -> Result<String, ToolCallError>;

    /// Parse and validate tool calls from raw model text output against registered tools.
    fn parse_and_validate(
        &self,
        raw_text: &str,
        tools: &[ToolDefinition],
    ) -> ParsedToolResult;

    /// Format client tool results (from `ChatMessage` with `role: "tool"`) into continuation turn context.
    fn format_tool_continuation(
        &self,
        results: &[ToolResult],
        tools: &[ToolDefinition],
    ) -> String;
}
```

---

## 3. Request Lifecycle & Routing Integration

```
Client Request (POST /v1/chat/completions with tools[])
       │
       ▼
[openai-compat] Deserializes ChatCompletionRequest (tools: Option<Vec<ToolSpec>>, tool_choice: Option<Value>, messages)
       │
       ▼
[tool-calling] ParsedToolChoice::from_value(tool_choice) parses directive
       │
       ▼
[tool-calling] inject_tool_schema() embeds formatted tool schema block into prompt
       │
       ▼
[gemini-adapter] Sends query to Gemini Web upstream & receives response
       │
       ▼
[tool-calling] parse_and_validate() extracts and validates tool calls:
       ├─ Valid tool calls ──→ Choice finish_reason = "tool_calls", message.tool_calls = [...]
       └─ Plain / Invalid  ──→ Choice finish_reason = "stop", message.content = text + warning header
       │
       ▼
Client executes tools locally & sends next request with role: "tool"
       │
       ▼
[tool-calling] format_tool_continuation() formats prior tool execution results into conversation history
```

---

## 4. Behavior & Invariants

1. **Deterministic Schema Injection & Injection Defense:**
   - Tool definitions are serialized into a standard, deterministic instruction block appended to system or initial user instructions.
   - Clear opening and closing delimiters (e.g. `<<<TOOLS_SCHEMA>>>` and `<<<END_TOOLS_SCHEMA>>>`) isolate schema declarations from conversational content.
   - User-supplied tool names, descriptions, and JSON schema properties are serialized strictly as inert JSON data strings; any delimiter substrings inside descriptions are escaped to prevent prompt injection.
   - When `tool_choice = ParsedToolChoice::None`, tool schemas are completely omitted from the prompt.

2. **Robust Multi-Strategy JSON Extraction:**
   - The parser inspects model output using sequential extraction strategies:
     a. Markdown code blocks marked with ` ```json ` or ` ``` ` containing function call objects.
     b. XML/delimited tags such as `<tool_call>...</tool_call>`.
     c. Top-level raw JSON objects containing `name` and `arguments` keys.
   - Surrounding text is preserved as leading or accompanying content when appropriate.

3. **Schema Validation & Integrity Invariant:**
   - Every candidate function name must match a registered `ToolDefinition.name`.
   - Function arguments must parse as valid JSON.
   - If parameter schemas specify required properties or types, candidate arguments are checked against those constraints.

4. **Non-Executable Fallback Invariant:**
   - If candidate output attempts a tool call but contains unparseable JSON, references an unknown function, or violates required parameter constraints, the engine **never** emits a partial or invalid `ToolCall`.
   - Instead, the entire output is returned as standard assistant text (`message.content`), and a structured `ToolCallWarning` is attached for logging and observability (`x-gemini-bridge-tool-warning` header).

5. **Multi-Turn Role `tool` Continuation:**
   - When the client submits subsequent turns with `role: "tool"`, the bridge pairs each result with its corresponding `call_id`.
   - Results are rendered in the Gemini Web conversation payload as structured tool output observations so the model can generate its final synthesis.

6. **Safety & Zero-Execution Policy:**
   - The bridge acts purely as a translator and validator. It **never** executes functions, runs subprocesses, calls external APIs, or interprets tool logic locally.

7. **Benchmark Parse Rate:**
   - The extraction engine must achieve a **≥95% success rate** on the 50-case benchmark test suite (where a test passes if valid calls are parsed into `ToolCall`s and malformed calls safely fall back to text + structured warnings).

---

## 5. Acceptance Criteria & Traceability

### US-5 Traceability (Tool Calling Emulation)

| PRD US-5 Acceptance Criterion | Module Specification Coverage |
|---|---|
| `tools[]` + `tool_choice` received; bridge injects schema instructions and parses output into structured `tool_calls[]` | `ToolEngine::inject_tool_schema` and `ToolEngine::parse_and_validate`; typed `ParsedToolChoice`, `ToolDefinition`, and `ToolCall` models |
| Tool results sent back as role `tool` and forwarded to model | `ToolEngine::format_tool_continuation` formats `role: "tool"` inputs with matching `call_id` into continuation context |
| Validator rejects malformed `tool_calls` (fallback to text + structured warning) | `ParsedToolResult::PlainContent` with `ToolCallWarning`; non-executable fallback invariant |
| Metric: Parse success rate ≥ 95% on 50-case test suite | Section 4.7 & 6.2 benchmark test suite covering 50 standard tool-calling scenarios |

---

## 6. Testing Strategy

### 6.1 Unit Tests
- **Schema Injection Tests:** Verify deterministic prompt generation across `Auto`, `None`, `Required`, and `Specific` function choices.
- **JSON Parser Extraction Tests:**
  - Standard JSON fenced code blocks.
  - Raw un-fenced JSON objects.
  - JSON embedded inside conversational prose (before, after, or interleaved).
  - Multiple simultaneous tool calls in one turn.
  - Escaped characters, unicode strings, nested objects, and arrays.
- **Validation Tests:**
  - Unknown function names rejected.
  - Malformed/truncated JSON converted to text fallback with warning.
  - Missing required fields caught and routed to fallback.
  - Type mismatches (e.g. string supplied where integer required) caught.
- **Continuation Formatter Tests:**
  - Matching `call_id` pairs correctly.
  - Multi-tool result sequences formatted without loss.

### 6.2 50-Case Golden Benchmark Suite (`E1`)
- 20 single-function valid calls with varying argument types (expected: valid `ToolCall`).
- 10 multi-function calls in a single response (expected: multiple `ToolCall` items).
- 5 nested object / complex array schema calls (expected: valid `ToolCall`).
- 5 calls wrapped in prose / explanatory text (expected: valid `ToolCall` + text prefix).
- 5 malformed JSON cases (expected: safe fallback to plain content + structured warning).
- 5 edge cases (empty args, unicode keys, extreme string lengths).
- Target: ≥ 95% pass rate (≥ 48/50 matching expected test outcome).

### 6.3 Integration Tests
- End-to-end multi-turn conversation test: Client submits prompt with `tools` → receives `tool_calls` → submits `tool` result → receives final answer.

---

## 7. Boundaries

- **Always:** Validate parsed tool calls against declared JSON schemas; return unparseable or unknown tool calls as assistant text with a structured warning; sanitize injected schemas to prevent prompt injection; ensure ≥95% pass rate on benchmark suite.
- **Ask First:** Adding support for non-function tool types (e.g. custom agents); altering default prompt delimiter tags.
- **Never:** Execute arbitrary client tools, scripts, or system commands in the bridge; emit unvalidated/malformed tool calls as executable `ToolCall` structures; include audio/TTS capabilities in tool calling scope.

---

## 8. Implementation Reference

- **OpenAI Compatibility Layer:** `docs/specs/SPEC-openai-compat.md` (`ToolSpec`, `AssistantMessage.tool_calls`).
- **LLM Service Contract:** `docs/specs/SPEC-llm-service.md` (`ToolDefinition`, `ToolCall`, `ToolResult`, `LlmRequest`).
- **Gemini Adapter Protocol:** `docs/specs/SPEC-gemini-adapter.md` (multi-turn conversation payload framing).
- **Task Implementation:** Task 2.3 (`crates/tool-calling/src/lib.rs`, `injector.rs`, `parser.rs`).
