//! Robust tool-call extraction, schema validation, and fallback logic.
//!
//! Extraction strategies (in order):
//! 1. Markdown code fences: ```` ```json ... ``` ```` or ```` ``` ... ``` ````
//! 2. XML-style delimiters: `<tool_call>...</tool_call>`
//! 3. Raw inline JSON objects/arrays (including embedded in prose)
//!
//! Non-executable fallback invariant:
//! If model output appears to call a tool but is invalid, it is NEVER emitted
//! as a valid `ToolCall`. It is converted to `ParsedToolResult::PlainContent`
//! with a structured `ToolCallWarning`.

use crate::{ParsedToolResult, ToolCall, ToolCallWarning, ToolDefinition};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use uuid::Uuid;

// ── Warning code constants ───────────────────────────────────────────────────

pub const WARN_UNKNOWN_FUNCTION: &str = "UNKNOWN_FUNCTION";
pub const WARN_MALFORMED_ARGUMENTS: &str = "MALFORMED_ARGUMENTS";
pub const WARN_SCHEMA_MISMATCH: &str = "SCHEMA_MISMATCH";
pub const WARN_MALFORMED_CALL: &str = "MALFORMED_CALL";

// ── Regex patterns ───────────────────────────────────────────────────────────

static FENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Matches ```json ... ``` or ``` ... ```
    Regex::new(r"(?s)```(?:json)?\s*([\s\S]*?)\s*```").expect("invalid fence regex")
});

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Matches <tool_call>...</tool_call>
    Regex::new(r"(?s)<tool_call>\s*([\s\S]*?)\s*</tool_call>").expect("invalid tag regex")
});

// ── Internal representation ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CandidateCall {
    raw_snippet: String,
    name: String,
    arguments_raw: Value,
}

// ── Extraction ───────────────────────────────────────────────────────────────

/// Attempts to parse a JSON Value into one or more `CandidateCall`s.
fn try_value_to_candidates(val: &Value, raw: &str) -> Option<Vec<CandidateCall>> {
    match val {
        Value::Object(map) => {
            let name = map.get("name")?.as_str()?.to_owned();
            let args_raw = map.get("arguments")?.clone();
            Some(vec![CandidateCall {
                raw_snippet: raw.to_owned(),
                name,
                arguments_raw: args_raw,
            }])
        }
        Value::Array(arr) if !arr.is_empty() => {
            let mut candidates = Vec::with_capacity(arr.len());
            for item in arr {
                let sub = try_value_to_candidates(item, raw)?;
                candidates.extend(sub);
            }
            Some(candidates)
        }
        _ => None,
    }
}

/// Tries to parse one or more JSON values (single, array, or stream of objects) from a string.
fn try_parse_all_candidates(text: &str, raw: &str) -> Option<Vec<CandidateCall>> {
    let stream = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let mut candidates = Vec::new();
    let mut any = false;
    for item in stream {
        match item {
            Ok(val) => {
                any = true;
                candidates.extend(try_value_to_candidates(&val, raw)?);
            }
            Err(_) => return None,
        }
    }
    if any && !candidates.is_empty() {
        Some(candidates)
    } else {
        None
    }
}

/// Strategy 1: Fenced code blocks.
fn extract_fenced(text: &str) -> Option<(Vec<CandidateCall>, Option<String>)> {
    let mut all = Vec::new();
    let mut prefix = None;
    for cap in FENCE_RE.captures_iter(text) {
        let inside = cap.get(1)?.as_str().trim();
        let range = cap.get(0)?.range();
        if let Some(candidates) = try_parse_all_candidates(inside, inside) {
            if all.is_empty() {
                let before = text[..range.start].trim();
                prefix = (!before.is_empty()).then(|| before.to_owned());
            }
            all.extend(candidates);
        }
    }
    (!all.is_empty()).then_some((all, prefix))
}

/// Strategy 2: <tool_call> XML tags.
fn extract_tagged(text: &str) -> Option<(Vec<CandidateCall>, Option<String>)> {
    let mut all = Vec::new();
    let mut prefix = None;
    for cap in TAG_RE.captures_iter(text) {
        let inside = cap.get(1)?.as_str().trim();
        let range = cap.get(0)?.range();
        if let Some(candidates) = try_parse_all_candidates(inside, inside) {
            if all.is_empty() {
                let before = text[..range.start].trim();
                prefix = (!before.is_empty()).then(|| before.to_owned());
            }
            all.extend(candidates);
        }
    }
    (!all.is_empty()).then_some((all, prefix))
}

/// Strategy 3: Raw JSON objects embedded in text (balanced-brace scanner).
fn extract_raw_json(text: &str) -> Option<(Vec<CandidateCall>, Option<String>)> {
    let mut all = Vec::new();
    let mut prefix = None;
    let mut idx = 0;
    while idx < text.len() {
        let start = text[idx..]
            .char_indices()
            .find(|(_, ch)| *ch == '{' || *ch == '[')
            .map(|(offset, _)| idx + offset)?;
        if let Some(end) = scan_balanced_json(&text[start..]) {
            let raw = &text[start..start + end];
            if let Some(candidates) = try_parse_all_candidates(raw, raw) {
                if all.is_empty() {
                    let before = text[..start].trim();
                    prefix = (!before.is_empty()).then(|| before.to_owned());
                }
                all.extend(candidates);
                idx = start + end;
                continue;
            }
        }
        idx = start + 1;
    }
    (!all.is_empty()).then_some((all, prefix))
}

/// Scans a balanced `{ ... }` or `[ ... ]` block from the start of `s`.
fn scan_balanced_json(s: &str) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escaped = false;
    let open_char = s.chars().next()?;
    let close_char = match open_char {
        '{' => '}',
        '[' => ']',
        _ => return None,
    };
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && in_str {
            escaped = true;
            continue;
        }
        if c == '"' {
            in_str = !in_str;
            continue;
        }
        if in_str {
            continue;
        }
        if c == open_char {
            depth += 1;
        } else if c == close_char {
            depth -= 1;
            if depth == 0 {
                return Some(i + c.len_utf8());
            }
        }
    }
    None
}

// ── Validation ───────────────────────────────────────────────────────────────

/// Validates a single `CandidateCall` against the registered tools.
fn validate_candidate(
    candidate: &CandidateCall,
    tools: &[ToolDefinition],
) -> Result<ToolCall, ToolCallWarning> {
    // 1. Name must match a registered tool.
    let def = tools
        .iter()
        .find(|t| t.name == candidate.name)
        .ok_or_else(|| ToolCallWarning {
            code: WARN_UNKNOWN_FUNCTION.to_owned(),
            message: format!("Function `{}` is not in registered tools", candidate.name),
            raw_candidate: Some(candidate.raw_snippet.clone()),
        })?;

    // 2. Arguments may be either a JSON object/array or a JSON-encoded string.
    let arguments_val = match &candidate.arguments_raw {
        Value::String(raw) => serde_json::from_str(raw).map_err(|err| ToolCallWarning {
            code: WARN_MALFORMED_ARGUMENTS.to_owned(),
            message: err.to_string(),
            raw_candidate: Some(candidate.raw_snippet.clone()),
        })?,
        value => value.clone(),
    };

    validate_arguments(&arguments_val, &def.parameters, &def.name).map_err(|msg| {
        ToolCallWarning {
            code: WARN_SCHEMA_MISMATCH.to_owned(),
            message: msg,
            raw_candidate: Some(candidate.raw_snippet.clone()),
        }
    })?;

    let arguments_str = serde_json::to_string(&arguments_val).map_err(|e| ToolCallWarning {
        code: WARN_MALFORMED_ARGUMENTS.to_owned(),
        message: e.to_string(),
        raw_candidate: Some(candidate.raw_snippet.clone()),
    })?;

    Ok(ToolCall {
        id: format!("call_{}", Uuid::new_v4().simple()),
        name: candidate.name.clone(),
        arguments: arguments_str,
    })
}

/// Checks required properties and primitive types against the schema.
fn validate_arguments(args: &Value, schema: &Value, fn_name: &str) -> Result<(), String> {
    let args_obj = match args {
        Value::Object(m) => m,
        _ => {
            return Err(format!(
                "Arguments for `{fn_name}` must be a JSON object, got: {args}"
            ));
        }
    };

    let schema_obj = match schema {
        Value::Object(m) => m,
        _ => return Ok(()), // Non-object schema; treat as unconstrained.
    };

    // Check required fields.
    if let Some(Value::Array(reqs)) = schema_obj.get("required") {
        for req in reqs {
            if let Some(key) = req.as_str()
                && !args_obj.contains_key(key)
            {
                return Err(format!(
                    "Missing required parameter `{key}` for function `{fn_name}`"
                ));
            }
        }
    }

    // Check types of provided properties.
    if let Some(Value::Object(props)) = schema_obj.get("properties") {
        for (key, val) in args_obj {
            if val.is_null() && !is_required_property(schema_obj, key) {
                continue;
            }

            if let Some(prop_schema) = props.get(key)
                && let Some(expected_type) = prop_schema.get("type").and_then(Value::as_str)
                && !type_matches(val, expected_type)
            {
                return Err(format!(
                    "Parameter `{key}` for function `{fn_name}` expected type `{expected_type}`, got `{}`",
                    json_type_name(val)
                ));
            }
        }
    }

    Ok(())
}

fn is_required_property(schema: &serde_json::Map<String, Value>, key: &str) -> bool {
    schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| required.iter().any(|value| value.as_str() == Some(key)))
}

fn type_matches(val: &Value, expected: &str) -> bool {
    match expected {
        "string" => val.is_string(),
        "number" => val.is_number(),
        "integer" => val.as_i64().is_some() || val.as_u64().is_some(),
        "boolean" => val.is_boolean(),
        "array" => val.is_array(),
        "object" => val.is_object(),
        "null" => val.is_null(),
        _ => true, // Unknown type name; don't reject.
    }
}

fn json_type_name(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ── Suspected-call detector ──────────────────────────────────────────────────

/// Checks whether raw text looks like a tool-call attempt that failed parsing.
fn detect_suspected_call(text: &str) -> Option<ToolCallWarning> {
    // If it has "name": and "arguments": anywhere, it was almost certainly a call.
    if text.contains("\"name\"") && text.contains("\"arguments\"") {
        return Some(ToolCallWarning {
            code: WARN_MALFORMED_CALL.to_owned(),
            message: "Output contains tool call keys but could not be parsed as valid JSON"
                .to_owned(),
            raw_candidate: Some(text.trim().to_owned()),
        });
    }
    // Code fence with json keyword that couldn't be parsed
    if text.contains("```json") {
        return Some(ToolCallWarning {
            code: WARN_MALFORMED_CALL.to_owned(),
            message: "Fenced JSON block could not be parsed as a tool call".to_owned(),
            raw_candidate: Some(text.trim().to_owned()),
        });
    }
    None
}

// ── Public entry point ───────────────────────────────────────────────────────

/// Parses and validates candidate tool calls from model output.
pub fn parse_and_validate(raw_text: &str, tools: &[ToolDefinition]) -> ParsedToolResult {
    let raw = raw_text.trim();

    // Try extraction strategies in sequence.
    let extracted = extract_fenced(raw)
        .or_else(|| extract_tagged(raw))
        .or_else(|| extract_raw_json(raw));

    if let Some((candidates, text_prefix)) = extracted {
        let mut validated_calls = Vec::with_capacity(candidates.len());
        for cand in &candidates {
            match validate_candidate(cand, tools) {
                Ok(call) => validated_calls.push(call),
                Err(warning) => {
                    // Non-executable fallback invariant: if ANY candidate fails,
                    // return plain text with the warning. Never emit partial calls.
                    return ParsedToolResult::PlainContent {
                        content: raw_text.to_owned(),
                        warning: Some(warning),
                    };
                }
            }
        }
        if !validated_calls.is_empty() {
            return ParsedToolResult::ToolCalls {
                calls: validated_calls,
                text_prefix,
            };
        }
    }

    // No valid calls found. Check if the model tried to call one and failed.
    let warning = detect_suspected_call(raw);
    ParsedToolResult::PlainContent {
        content: raw_text.to_owned(),
        warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weather_tool() -> ToolDefinition {
        ToolDefinition {
            name: "get_weather".to_owned(),
            description: "Gets the weather".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string" },
                    "days": { "type": "integer" }
                },
                "required": ["location"]
            }),
        }
    }

    #[test]
    fn parse_fenced_json_valid() {
        let input =
            "```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Tokyo\"}}\n```";
        let res = parse_and_validate(input, &[weather_tool()]);
        match res {
            ParsedToolResult::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "get_weather");
                assert!(calls[0].arguments.contains("Tokyo"));
            }
            _ => panic!("Expected ToolCalls"),
        }
    }

    #[test]
    fn parse_raw_json_valid() {
        let input = "{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Paris\"}}";
        let res = parse_and_validate(input, &[weather_tool()]);
        match res {
            ParsedToolResult::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "get_weather");
            }
            _ => panic!("Expected ToolCalls"),
        }
    }

    #[test]
    fn parse_with_text_prefix() {
        let input = "Sure, let me check that for you.\n```json\n{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Oslo\"}}\n```";
        let res = parse_and_validate(input, &[weather_tool()]);
        match res {
            ParsedToolResult::ToolCalls {
                calls, text_prefix, ..
            } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(
                    text_prefix.as_deref(),
                    Some("Sure, let me check that for you.")
                );
            }
            _ => panic!("Expected ToolCalls"),
        }
    }

    #[test]
    fn parse_unknown_function_falls_back_with_warning() {
        let input = "{\"name\": \"unknown_fn\", \"arguments\": {}}";
        let res = parse_and_validate(input, &[weather_tool()]);
        match res {
            ParsedToolResult::PlainContent { warning, .. } => {
                let w = warning.expect("expected warning");
                assert_eq!(w.code, WARN_UNKNOWN_FUNCTION);
            }
            _ => panic!("Expected PlainContent fallback"),
        }
    }

    #[test]
    fn parse_missing_required_param_falls_back() {
        let input = "{\"name\": \"get_weather\", \"arguments\": {\"days\": 3}}";
        let res = parse_and_validate(input, &[weather_tool()]);
        match res {
            ParsedToolResult::PlainContent { warning, .. } => {
                let w = warning.expect("expected warning");
                assert_eq!(w.code, WARN_SCHEMA_MISMATCH);
                assert!(w.message.contains("location"));
            }
            _ => panic!("Expected PlainContent fallback"),
        }
    }

    #[test]
    fn parse_type_mismatch_falls_back() {
        // days expected integer, given string
        let input = "{\"name\": \"get_weather\", \"arguments\": {\"location\": \"Rome\", \"days\": \"three\"}}";
        let res = parse_and_validate(input, &[weather_tool()]);
        match res {
            ParsedToolResult::PlainContent { warning, .. } => {
                let w = warning.expect("expected warning");
                assert_eq!(w.code, WARN_SCHEMA_MISMATCH);
                assert!(w.message.contains("days"));
            }
            _ => panic!("Expected PlainContent fallback"),
        }
    }

    #[test]
    fn parse_malformed_json_falls_back() {
        let input = "```json\n{\"name\": \"get_weather\", \"arguments\": {broken\n```";
        let res = parse_and_validate(input, &[weather_tool()]);
        match res {
            ParsedToolResult::PlainContent { warning, .. } => {
                assert!(warning.is_some());
            }
            _ => panic!("Expected PlainContent fallback"),
        }
    }

    #[test]
    fn parse_multiple_calls_in_array() {
        let input = r#"[
            {"name": "get_weather", "arguments": {"location": "London"}},
            {"name": "get_weather", "arguments": {"location": "Madrid"}}
        ]"#;
        let res = parse_and_validate(input, &[weather_tool()]);
        match res {
            ParsedToolResult::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 2);
            }
            _ => panic!("Expected ToolCalls"),
        }
    }

    #[test]
    fn parse_pre_encoded_arguments_string() {
        let input = r#"{"name": "get_weather", "arguments": "{\"location\": \"Berlin\"}"}"#;
        let res = parse_and_validate(input, &[weather_tool()]);
        match res {
            ParsedToolResult::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "get_weather");
            }
            _ => panic!("Expected ToolCalls"),
        }
    }
}
