//! OpenAI response extension types for Gemini-specific metadata.
//!
//! Standard OpenAI client SDKs ignore unknown top-level fields. The
//! `gemini_metadata` field provides code execution blocks, citations, and
//! conversation identifiers without altering standard fields.

use serde::{Deserialize, Serialize};

/// Top-level metadata extension field attached to OpenAI chat responses.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GeminiMetadata {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub code_execution: Vec<CodeExecutionMeta>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub citations: Vec<CitationMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}

/// A code execution block in `gemini_metadata`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodeExecutionMeta {
    pub language: String,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

/// A web search grounding citation in `gemini_metadata`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CitationMeta {
    pub start_index: usize,
    pub end_index: usize,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
