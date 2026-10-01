//! Parser-focused behavior tests: validation invariants, warning codes, and
//! multi-strategy extraction edge cases.
//!
//! Tests the non-executable fallback invariant, validation error codes, and
//! the multi-call "all-or-nothing" contract from the spec.

use gemini_bridge_llm_service::ToolDefinition;
use gemini_bridge_tool_calling::{DefaultToolEngine, ParsedToolResult, ToolEngine};
use serde_json::json;

fn weather_tool() -> ToolDefinition {
    ToolDefinition {
        name: "get_weather".into(),
        description: "Get current weather".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "location": { "type": "string" },
                "unit": { "type": "string" }
            },
            "required": ["location"]
        }),
    }
}

fn typed_tool() -> ToolDefinition {
    ToolDefinition {
        name: "typed_fn".into(),
        description: "Strictly typed".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer" },
                "name": { "type": "string" },
                "active": { "type": "boolean" }
            },
            "required": ["count", "name", "active"]
        }),
    }
}

fn two_tools() -> Vec<ToolDefinition> {
    vec![weather_tool(), typed_tool()]
}

// ── Plain conversational text (no tool call) ──────────────────────────────────

#[test]
fn plain_prose_with_no_tool_attempt_returns_plain_content_no_warning() {
    let engine = DefaultToolEngine;
    let result = engine.parse_and_validate(
        "The weather in San Francisco is typically mild and foggy.",
        &two_tools(),
    );
    match result {
        ParsedToolResult::PlainContent { content, warning } => {
            assert_eq!(
                content,
                "The weather in San Francisco is typically mild and foggy."
            );
            assert!(warning.is_none(), "Pure prose must not generate a warning");
        }
        other => panic!("Expected PlainContent, got {:?}", other),
    }
}

#[test]
fn empty_string_input_returns_plain_content_no_warning() {
    let engine = DefaultToolEngine;
    let result = engine.parse_and_validate("", &two_tools());
    match result {
        ParsedToolResult::PlainContent { warning, .. } => {
            assert!(warning.is_none(), "Empty input must not trigger a warning");
        }
        other => panic!("Expected PlainContent, got {:?}", other),
    }
}

#[test]
fn plain_content_preserves_exact_raw_input_text() {
    let engine = DefaultToolEngine;
    let input = "This is a multi-line reply.\n\nWith two paragraphs.";
    let result = engine.parse_and_validate(input, &two_tools());
    match result {
        ParsedToolResult::PlainContent { content, .. } => {
            assert_eq!(content, input, "PlainContent.content must equal raw input");
        }
        other => panic!("Expected PlainContent, got {:?}", other),
    }
}

// ── Validation: Unknown Function Name ─────────────────────────────────────────

#[test]
fn unknown_function_name_returns_plain_content_with_unknown_function_warning() {
    let engine = DefaultToolEngine;
    let input = "```json\n{\"name\": \"nonexistent_function\", \"arguments\": {\"x\": 1}}\n```";
    let result = engine.parse_and_validate(input, &two_tools());
    match result {
        ParsedToolResult::PlainContent { content, warning } => {
            assert_eq!(content, input, "Fallback content must equal raw input");
            let w = warning.expect("UNKNOWN_FUNCTION must produce a warning");
            assert_eq!(
                w.code, "UNKNOWN_FUNCTION",
                "Warning code must be UNKNOWN_FUNCTION, got: {}",
                w.code
            );
            assert!(
                w.raw_candidate.is_some(),
                "raw_candidate must be Some for a recognized-but-unknown function attempt"
            );
        }
        other => panic!(
            "Expected PlainContent fallback for unknown function, got {:?}",
            other
        ),
    }
}

#[test]
fn unknown_function_fallback_never_emits_tool_calls() {
    let engine = DefaultToolEngine;
    let input = "```json\n{\"name\": \"definitely_does_not_exist\", \"arguments\": {}}\n```";
    let result = engine.parse_and_validate(input, &two_tools());
    assert!(
        !matches!(result, ParsedToolResult::ToolCalls { .. }),
        "Unknown function must NEVER produce ToolCalls"
    );
}

// ── Validation: Malformed JSON Arguments ─────────────────────────────────────

#[test]
fn malformed_arguments_fenced_returns_malformed_arguments_or_call_warning() {
    let engine = DefaultToolEngine;
    // Valid JSON outer object but arguments value is a broken pre-encoded string.
    let input = "```json\n{\"name\": \"get_weather\", \"arguments\": \"{location: broken}\"}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match result {
        ParsedToolResult::PlainContent { warning, .. } => {
            let w = warning.expect("Malformed arguments must produce a warning");
            assert!(
                w.code == "MALFORMED_ARGUMENTS" || w.code == "MALFORMED_CALL",
                "Expected MALFORMED_ARGUMENTS or MALFORMED_CALL, got: {}",
                w.code
            );
        }
        other => panic!(
            "Expected PlainContent for malformed arguments, got {:?}",
            other
        ),
    }
}

#[test]
fn malformed_arguments_raw_candidate_is_some() {
    let engine = DefaultToolEngine;
    let input = "```json\n{\"name\": \"get_weather\", \"arguments\": \"{badly broken}\"}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match result {
        ParsedToolResult::PlainContent { warning, .. } => {
            if let Some(w) = warning {
                assert!(w.raw_candidate.is_some(), "raw_candidate must be Some");
            }
            // No warning is fine if parser classifies it as non-call.
        }
        other => panic!("Expected PlainContent, got {:?}", other),
    }
}

// ── Validation: Missing Required Property ────────────────────────────────────

#[test]
fn missing_required_property_returns_schema_mismatch_warning() {
    let engine = DefaultToolEngine;
    // `location` is required but absent.
    let input = "```json\n{\"name\": \"get_weather\", \"arguments\": {\"unit\": \"celsius\"}}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match result {
        ParsedToolResult::PlainContent { content, warning } => {
            assert_eq!(content, input);
            let w = warning.expect("Missing required field must produce a warning");
            assert_eq!(
                w.code, "SCHEMA_MISMATCH",
                "Expected SCHEMA_MISMATCH warning code, got: {}",
                w.code
            );
        }
        other => panic!("Expected PlainContent for schema mismatch, got {:?}", other),
    }
}

#[test]
fn missing_required_property_raw_candidate_is_some() {
    let engine = DefaultToolEngine;
    let input = "```json\n{\"name\": \"get_weather\", \"arguments\": {}}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match result {
        ParsedToolResult::PlainContent { warning, .. } => {
            let w = warning.expect("Missing required 'location' must produce a warning");
            assert!(
                w.raw_candidate.is_some(),
                "raw_candidate must be Some for schema mismatch"
            );
        }
        other => panic!("Expected PlainContent, got {:?}", other),
    }
}

// ── Validation: Type Mismatch ─────────────────────────────────────────────────

#[test]
fn string_supplied_where_integer_required_returns_schema_mismatch() {
    let engine = DefaultToolEngine;
    // `count` requires integer, but "three" is a string.
    let input = "```json\n{\"name\": \"typed_fn\", \"arguments\": {\"count\": \"three\", \"name\": \"Alice\", \"active\": true}}\n```";
    let result = engine.parse_and_validate(input, &[typed_tool()]);
    match result {
        ParsedToolResult::PlainContent { warning, .. } => {
            let w = warning.expect("Type mismatch must produce a warning");
            assert_eq!(
                w.code, "SCHEMA_MISMATCH",
                "Expected SCHEMA_MISMATCH for type mismatch, got: {}",
                w.code
            );
        }
        other => panic!("Expected PlainContent for type mismatch, got {:?}", other),
    }
}

#[test]
fn number_supplied_where_boolean_required_returns_schema_mismatch() {
    let engine = DefaultToolEngine;
    // `active` requires boolean, 1 (integer) is not valid.
    let input = "```json\n{\"name\": \"typed_fn\", \"arguments\": {\"count\": 5, \"name\": \"Bob\", \"active\": 1}}\n```";
    let result = engine.parse_and_validate(input, &[typed_tool()]);
    match result {
        ParsedToolResult::PlainContent { warning, .. } => {
            let w = warning.expect("Type mismatch must produce a warning");
            assert_eq!(w.code, "SCHEMA_MISMATCH");
        }
        other => panic!("Expected PlainContent for type mismatch, got {:?}", other),
    }
}

// ── Multi-call: All-or-Nothing Fallback Invariant ────────────────────────────

#[test]
fn multi_call_one_fails_validation_returns_fallback_not_partial_calls() {
    let engine = DefaultToolEngine;
    // Two calls in an array; second references an unknown function.
    let input = "```json\n[\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Portland\"}},\n  {\"name\": \"nonexistent_tool\", \"arguments\": {}}\n]\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match &result {
        ParsedToolResult::PlainContent { warning, .. } => {
            let w = warning
                .as_ref()
                .expect("Mixed-validity array must produce a warning");
            assert_eq!(w.code, "UNKNOWN_FUNCTION");
        }
        ParsedToolResult::ToolCalls { calls, .. } => {
            panic!(
                "Must NEVER emit partial ToolCalls when any call fails; got {} calls",
                calls.len()
            );
        }
    }
}

#[test]
fn multi_call_one_missing_required_field_returns_plain_fallback() {
    let engine = DefaultToolEngine;
    // Second call missing required `location`.
    let input = "```json\n[\n  {\"name\": \"get_weather\", \"arguments\": {\"location\": \"Miami\"}},\n  {\"name\": \"get_weather\", \"arguments\": {}}\n]\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    assert!(
        !matches!(result, ParsedToolResult::ToolCalls { .. }),
        "Partial validity in multi-call array must fall back to PlainContent"
    );
}

// ── Extraction Strategy: Arguments Forms ──────────────────────────────────────

#[test]
fn arguments_as_object_produces_valid_json_string_in_tool_call() {
    let engine = DefaultToolEngine;
    let input =
        "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Seattle\"}}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match result {
        ParsedToolResult::ToolCalls { calls, .. } => {
            assert_eq!(calls.len(), 1);
            let parsed: serde_json::Value =
                serde_json::from_str(&calls[0].arguments).expect("arguments must be valid JSON");
            assert_eq!(parsed["location"], "Seattle");
        }
        other => panic!("Expected ToolCalls, got {:?}", other),
    }
}

#[test]
fn arguments_as_pre_encoded_string_produces_valid_json_string_in_tool_call() {
    let engine = DefaultToolEngine;
    let input = "```json\n{\"name\": \"get_weather\", \"arguments\": \"{\\\"location\\\":\\\"Denver\\\"}\"}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match result {
        ParsedToolResult::ToolCalls { calls, .. } => {
            assert_eq!(calls.len(), 1);
            let parsed: serde_json::Value =
                serde_json::from_str(&calls[0].arguments).expect("arguments must be valid JSON");
            assert_eq!(parsed["location"], "Denver");
        }
        other => panic!("Expected ToolCalls, got {:?}", other),
    }
}

#[test]
fn tool_call_id_is_always_non_empty_string() {
    let engine = DefaultToolEngine;
    let input = "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"NYC\"}}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match result {
        ParsedToolResult::ToolCalls { calls, .. } => {
            for call in calls {
                assert!(!call.id.is_empty(), "Every ToolCall.id must be non-empty");
            }
        }
        other => panic!("Expected ToolCalls, got {:?}", other),
    }
}

#[test]
fn two_successive_parses_of_same_valid_input_produce_valid_calls() {
    // IDs will differ per parse, but both results must be ToolCalls.
    let engine = DefaultToolEngine;
    let input =
        "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Boston\"}}\n```";
    let r1 = engine.parse_and_validate(input, &[weather_tool()]);
    let r2 = engine.parse_and_validate(input, &[weather_tool()]);
    assert!(
        matches!(r1, ParsedToolResult::ToolCalls { .. }),
        "First parse must succeed"
    );
    assert!(
        matches!(r2, ParsedToolResult::ToolCalls { .. }),
        "Second parse must succeed"
    );
}

// ── Extraction: Strategy Priority ────────────────────────────────────────────

#[test]
fn fenced_json_strategy_takes_priority_over_raw_json() {
    let engine = DefaultToolEngine;
    // The fenced block is valid; a stray raw JSON-like pattern follows.
    let input = "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Phoenix\"}}\n```\nSome trailing text {\"name\": \"ignored\", \"arguments\": {}}";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match result {
        ParsedToolResult::ToolCalls { calls, .. } => {
            assert_eq!(calls.len(), 1, "Only the fenced call should be extracted");
            assert_eq!(calls[0].name, "get_weather");
        }
        other => panic!(
            "Expected exactly one ToolCall from fenced block, got {:?}",
            other
        ),
    }
}

// ── Extraction: JSON whitespace and key-order invariance ──────────────────────

#[test]
fn arguments_with_varying_key_order_and_formatting_accepted() {
    let engine = DefaultToolEngine;
    let input =
        "```json\n{\"arguments\": {\"location\": \"Lisbon\"}, \"name\": \"get_weather\"}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    assert!(
        matches!(result, ParsedToolResult::ToolCalls { .. }),
        "Reversed key order (\"arguments\" before \"name\") must be accepted"
    );
}

#[test]
fn arguments_with_escaped_unicode_location_accepted() {
    let engine = DefaultToolEngine;
    let input = "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"M\\u00e5l\\u00f8y\"}}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    assert!(
        matches!(result, ParsedToolResult::ToolCalls { .. }),
        "Escaped unicode in arguments must be accepted as valid arguments"
    );
}

// ── ToolCallWarning: field completeness ───────────────────────────────────────

#[test]
fn tool_call_warning_has_non_empty_code_and_message() {
    let engine = DefaultToolEngine;
    let input = "```json\n{\"name\": \"unknown_fn\", \"arguments\": {}}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match result {
        ParsedToolResult::PlainContent { warning, .. } => {
            let w = warning.expect("Must have a warning for unknown function");
            assert!(!w.code.is_empty(), "Warning.code must not be empty");
            assert!(!w.message.is_empty(), "Warning.message must not be empty");
        }
        other => panic!("Expected PlainContent, got {:?}", other),
    }
}

#[test]
fn tool_call_warning_raw_candidate_is_some_for_validation_failures() {
    let engine = DefaultToolEngine;
    let input = "```json\n{\"name\": \"unknown_fn\", \"arguments\": {\"q\": 99}}\n```";
    let result = engine.parse_and_validate(input, &[weather_tool()]);
    match result {
        ParsedToolResult::PlainContent { warning, .. } => {
            if let Some(w) = warning {
                assert!(
                    w.raw_candidate.is_some(),
                    "raw_candidate must be Some for a validation failure"
                );
            }
        }
        other => panic!("Expected PlainContent, got {:?}", other),
    }
}
