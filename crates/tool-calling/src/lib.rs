//! Safe emulation of OpenAI-compatible function/tool calls over text models.
//!
//! This crate only translates and validates tool calls. It never executes one.

mod injector;
mod parser;

pub use gemini_bridge_llm_service::{ToolCall, ToolDefinition, ToolResult};
use gemini_bridge_openai_compat::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Validated directive controlling whether the model may invoke a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParsedToolChoice {
    None,
    Auto,
    Required,
    Specific { function_name: String },
}

impl ParsedToolChoice {
    /// Parses OpenAI's `tool_choice`; an omitted value means `auto`.
    pub fn from_value(value: Option<&Value>) -> Result<Self, ToolCallError> {
        match value {
            None => Ok(Self::Auto),
            Some(Value::String(choice)) => match choice.as_str() {
                "none" => Ok(Self::None),
                "auto" => Ok(Self::Auto),
                "required" => Ok(Self::Required),
                other => Err(ToolCallError::InvalidToolChoice(other.to_owned())),
            },
            Some(val @ Value::Object(choice)) => {
                if choice.get("type").and_then(Value::as_str) != Some("function") {
                    return Err(ToolCallError::InvalidToolChoice(val.to_string()));
                }
                let function_name = choice
                    .get("function")
                    .and_then(Value::as_object)
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .ok_or_else(|| ToolCallError::InvalidToolChoice(val.to_string()))?;
                Ok(Self::Specific {
                    function_name: function_name.to_owned(),
                })
            }
            Some(other) => Err(ToolCallError::InvalidToolChoice(other.to_string())),
        }
    }
}

/// A diagnostic emitted when output that resembles a tool call is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallWarning {
    pub code: String,
    pub message: String,
    pub raw_candidate: Option<String>,
}

/// Parsed tool calls, or the original assistant text with a safe warning.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedToolResult {
    ToolCalls {
        calls: Vec<ToolCall>,
        text_prefix: Option<String>,
    },
    PlainContent {
        content: String,
        warning: Option<ToolCallWarning>,
    },
}

/// Errors encountered while parsing directives, schemas, or candidate calls.
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

/// Tool-call translation and validation contract.
pub trait ToolEngine: Send + Sync {
    fn inject_tool_schema(
        &self,
        base_prompt: &str,
        tools: &[ToolDefinition],
        choice: &ParsedToolChoice,
    ) -> Result<String, ToolCallError>;

    fn parse_and_validate(&self, raw_text: &str, tools: &[ToolDefinition]) -> ParsedToolResult;

    fn format_tool_continuation(&self, results: &[ToolResult], tools: &[ToolDefinition]) -> String;
}

/// Stateless deterministic implementation of [`ToolEngine`].
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultToolEngine;

impl ToolEngine for DefaultToolEngine {
    fn inject_tool_schema(
        &self,
        base_prompt: &str,
        tools: &[ToolDefinition],
        choice: &ParsedToolChoice,
    ) -> Result<String, ToolCallError> {
        injector::inject_tool_schema(base_prompt, tools, choice)
    }

    fn parse_and_validate(&self, raw_text: &str, tools: &[ToolDefinition]) -> ParsedToolResult {
        parser::parse_and_validate(raw_text, tools)
    }

    fn format_tool_continuation(&self, results: &[ToolResult], tools: &[ToolDefinition]) -> String {
        let _ = tools;
        injector::format_tool_continuation(results)
    }
}

/// Converts OpenAI function `ToolSpec`s to provider-neutral tool definitions.
pub fn tool_definitions_from_specs(
    specs: &[ToolSpec],
) -> Result<Vec<ToolDefinition>, ToolCallError> {
    specs
        .iter()
        .map(|spec| {
            if spec.tool_type != "function" {
                return Err(ToolCallError::InvalidSchema(
                    spec.tool_type.clone(),
                    "only function tools are supported".to_owned(),
                ));
            }
            let function = spec.function.as_object().ok_or_else(|| {
                ToolCallError::InvalidSchema(
                    "<unnamed>".to_owned(),
                    "function definition must be a JSON object".to_owned(),
                )
            })?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| {
                    ToolCallError::InvalidSchema(
                        "<unnamed>".to_owned(),
                        "function name must be a non-empty string".to_owned(),
                    )
                })?;
            let description = function
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let parameters = function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type":"object"}));
            injector::validate_tool_definition(&ToolDefinition {
                name: name.to_owned(),
                description: description.to_owned(),
                parameters: parameters.clone(),
            })?;
            Ok(ToolDefinition {
                name: name.to_owned(),
                description: description.to_owned(),
                parameters,
            })
        })
        .collect()
}
