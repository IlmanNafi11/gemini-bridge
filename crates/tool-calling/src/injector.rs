//! Schema injection and continuation formatting.
//!
//! The injector is deterministic: same inputs → same output bytes.
//! User-supplied strings are serialized as inert JSON values; any delimiter
//! occurrences inside them are escaped so the model cannot break out of the
//! `<<<TOOLS_SCHEMA>>>` block.

use crate::{ParsedToolChoice, ToolCallError, ToolDefinition, ToolResult};
use serde_json::Value;

// ── Public delimiters ────────────────────────────────────────────────────────

/// Opening delimiter for the tool schema block inserted into the prompt.
pub const SCHEMA_OPEN: &str = "<<<TOOLS_SCHEMA>>>";
/// Closing delimiter for the tool schema block.
pub const SCHEMA_CLOSE: &str = "<<<END_TOOLS_SCHEMA>>>";
/// Opening delimiter for the tool results block.
pub const RESULTS_OPEN: &str = "<<<TOOL_RESULTS>>>";
/// Closing delimiter for the tool results block.
pub const RESULTS_CLOSE: &str = "<<<END_TOOL_RESULTS>>>";

// ── Injection defense ────────────────────────────────────────────────────────

/// Checks whether a raw (non-JSON-serialized) user string contains a delimiter.
/// If so, returns an `InjectionError`.  Descriptions are only checked here when
/// they will be embedded verbatim; JSON-encoded paths are safe by construction.
fn check_injection(value: &str, context: &str) -> Result<(), ToolCallError> {
    const DELIMITERS: &[&str] = &[SCHEMA_OPEN, SCHEMA_CLOSE, RESULTS_OPEN, RESULTS_CLOSE];
    for delim in DELIMITERS {
        if value.contains(delim) {
            return Err(ToolCallError::InjectionError(format!(
                "delimiter `{}` found in {context}",
                delim
            )));
        }
    }
    Ok(())
}

/// Escapes delimiter substrings in plain-text prompt content.
fn escape_delimiters(s: &str) -> String {
    s.replace("<<<", "<<\\<")
}

// ── Schema validation ────────────────────────────────────────────────────────

pub fn validate_tool_definition(def: &ToolDefinition) -> Result<(), ToolCallError> {
    // Function names appear in protocol positions and cannot safely contain delimiters.
    check_injection(&def.name, "tool name")?;
    // Descriptions and schema values are JSON-serialized and delimiter-escaped below.
    match &def.parameters {
        Value::Object(_) => {}
        other => {
            return Err(ToolCallError::InvalidSchema(
                def.name.clone(),
                format!("parameters must be a JSON object, got {}", other),
            ));
        }
    }
    Ok(())
}

// ── Prompt injection ─────────────────────────────────────────────────────────

/// Builds the instruction block appended to the base prompt.
fn build_schema_block(tools: &[ToolDefinition], choice: &ParsedToolChoice) -> String {
    let mut buf = String::new();

    buf.push_str(SCHEMA_OPEN);
    buf.push('\n');
    buf.push_str("You have access to the following functions. ");
    match choice {
        ParsedToolChoice::None => {}
        ParsedToolChoice::Auto => {
            buf.push_str("You may call one or more functions if helpful. ");
        }
        ParsedToolChoice::Required => {
            buf.push_str("You MUST call at least one function. ");
        }
        ParsedToolChoice::Specific { function_name } => {
            buf.push_str(&format!(
                "You MUST call the function `{}`. ",
                escape_delimiters(function_name)
            ));
        }
    }
    buf.push_str(
        "To invoke a function, output ONLY a JSON object (or a JSON array of objects for multiple calls) \
with keys \"name\" and \"arguments\", followed by nothing else:\n",
    );
    buf.push_str(
        "```json\n{\"name\": \"function_name\", \"arguments\": {\"param\": \"value\"}}\n```\n",
    );
    buf.push('\n');

    for def in tools {
        // Serialize each definition as compact JSON; delimiter escaping applied
        // at the string level after serialization.
        let raw = serde_json::json!({
            "name": def.name,
            "description": def.description,
            "parameters": def.parameters,
        });
        let serialized = escape_delimiters(&raw.to_string());
        buf.push_str(&serialized);
        buf.push('\n');
    }

    buf.push_str(SCHEMA_CLOSE);
    buf
}

/// Appends the tool schema block to `base_prompt`, or returns it unchanged
/// when `choice` is `None`.
pub fn inject_tool_schema(
    base_prompt: &str,
    tools: &[ToolDefinition],
    choice: &ParsedToolChoice,
) -> Result<String, ToolCallError> {
    if matches!(choice, ParsedToolChoice::None) || tools.is_empty() {
        return Ok(base_prompt.to_owned());
    }
    for def in tools {
        validate_tool_definition(def)?;
    }
    let block = build_schema_block(tools, choice);
    Ok(format!("{base_prompt}\n\n{block}"))
}

// ── Continuation formatting ──────────────────────────────────────────────────

/// Renders `ToolResult`s into the multi-turn continuation context the model
/// receives in the next turn.
pub fn format_tool_continuation(results: &[ToolResult]) -> String {
    let mut buf = String::new();
    buf.push_str(RESULTS_OPEN);
    buf.push('\n');
    for result in results {
        // call_id and content are user-controlled; escape delimiters defensively.
        let safe_id = escape_delimiters(&result.call_id);
        let safe_content = escape_delimiters(&result.content);
        buf.push_str(&format!(
            "[Tool Result call_id=\"{safe_id}\"]\n{safe_content}\n[/Tool Result]\n"
        ));
    }
    buf.push_str(RESULTS_CLOSE);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolResult;

    fn weather_tool() -> ToolDefinition {
        ToolDefinition {
            name: "get_weather".to_owned(),
            description: "Returns the weather for a city.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string" }
                },
                "required": ["location"]
            }),
        }
    }

    #[test]
    fn inject_none_returns_unchanged() {
        let result =
            inject_tool_schema("Hello", &[weather_tool()], &ParsedToolChoice::None).unwrap();
        assert_eq!(result, "Hello");
    }

    #[test]
    fn inject_empty_tools_returns_unchanged() {
        let result = inject_tool_schema("Hello", &[], &ParsedToolChoice::Auto).unwrap();
        assert_eq!(result, "Hello");
    }

    #[test]
    fn inject_auto_contains_delimiters_and_tool() {
        let result = inject_tool_schema("sys", &[weather_tool()], &ParsedToolChoice::Auto).unwrap();
        assert!(result.contains(SCHEMA_OPEN));
        assert!(result.contains(SCHEMA_CLOSE));
        assert!(result.contains("get_weather"));
    }

    #[test]
    fn inject_required_mentions_must_call() {
        let result =
            inject_tool_schema("sys", &[weather_tool()], &ParsedToolChoice::Required).unwrap();
        assert!(result.contains("MUST call at least one"));
    }

    #[test]
    fn inject_specific_mentions_function_name() {
        let choice = ParsedToolChoice::Specific {
            function_name: "get_weather".to_owned(),
        };
        let result = inject_tool_schema("sys", &[weather_tool()], &choice).unwrap();
        assert!(result.contains("get_weather"));
        assert!(result.contains("MUST call the function"));
    }

    #[test]
    fn delimiter_in_description_is_escaped() {
        let mut def = weather_tool();
        def.description = format!("safe text {SCHEMA_OPEN} injected");
        let result = inject_tool_schema("sys", &[def], &ParsedToolChoice::Auto).unwrap();
        assert!(result.contains("safe text <<\\<TOOLS_SCHEMA>>> injected"));
        assert_eq!(result.matches(SCHEMA_OPEN).count(), 1);
    }

    #[test]
    fn injection_attack_in_name_is_rejected() {
        let mut def = weather_tool();
        def.name = format!("{SCHEMA_CLOSE}injected");
        let err = inject_tool_schema("sys", &[def], &ParsedToolChoice::Auto).unwrap_err();
        assert!(matches!(err, ToolCallError::InjectionError(_)));
    }

    #[test]
    fn continuation_format_correct() {
        let results = vec![
            ToolResult {
                call_id: "call_1".to_owned(),
                content: "sunny, 22°C".to_owned(),
            },
            ToolResult {
                call_id: "call_2".to_owned(),
                content: "rainy".to_owned(),
            },
        ];
        let out = format_tool_continuation(&results);
        assert!(out.starts_with(RESULTS_OPEN));
        assert!(out.ends_with(RESULTS_CLOSE));
        assert!(out.contains("[Tool Result call_id=\"call_1\"]"));
        assert!(out.contains("sunny, 22°C"));
        assert!(out.contains("[Tool Result call_id=\"call_2\"]"));
        assert!(out.contains("[/Tool Result]"));
    }

    #[test]
    fn delimiter_in_result_content_is_escaped() {
        let results = vec![ToolResult {
            call_id: "id".to_owned(),
            content: format!("before{RESULTS_OPEN}after"),
        }];
        let out = format_tool_continuation(&results);
        // Raw delimiter must not appear inside the block body
        // (the open delimiter at the very start is expected, the second one
        // inside content must be escaped)
        let after_open = &out[RESULTS_OPEN.len()..out.len() - RESULTS_CLOSE.len()];
        assert!(!after_open.contains(RESULTS_OPEN));
    }
}
