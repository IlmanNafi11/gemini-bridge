//! Typed positional contract for the Gemini Web wire shape.

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::GeminiAdapterError;

#[derive(Debug, Error)]
pub enum SchemaLoadError {
    #[error("schema TOML could not be parsed: {0}")]
    ParseError(String),
    #[error("schema is missing required field: {0}")]
    MissingField(&'static str),
    #[error("schema candidate path is invalid: {0}")]
    InvalidPath(String),
}

#[derive(Deserialize)]
struct SchemaDocument {
    positions: Option<RawPositions>,
}

#[derive(Deserialize)]
struct RawPositions {
    inner_len: Option<usize>,
    inner_message_index: Option<usize>,
    inner_language_index: Option<usize>,
    inner_context_index: Option<usize>,
    inner_conversation_index: Option<usize>,
    candidate_text_path: Option<Vec<String>>,
}

/// One segment in the path to cumulative candidate text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Field(String),
    Index(usize),
}

/// Positional contract for the Gemini Web wire shape.
///
/// `inner_*` indices address the JSON-decoded inner payload of the `f.req`
/// envelope (see [`GeminiWebSchema::build_request_envelope`]);
/// `candidate_text_path` addresses the decoded response payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiWebSchema {
    /// Inner payload length. The web client emits 99 slots; upstream accepts
    /// any length that places the threaded fields correctly.
    pub inner_len: usize,
    /// Inner slot holding the message tuple `[text, 0, null, null, null, null, 0]`.
    pub inner_message_index: usize,
    /// Inner slot holding the language tag `["en"]`.
    pub inner_language_index: usize,
    /// Inner slot holding `[[1]]` (continuation marker observed in the browser).
    pub inner_context_index: usize,
    /// Inner slot holding the upstream conversation ID on continuation turns.
    pub inner_conversation_index: usize,
    /// Path to cumulative candidate text inside the decoded response payload.
    pub candidate_text_path: Vec<PathSegment>,
}

impl Default for GeminiWebSchema {
    fn default() -> Self {
        Self {
            inner_len: 72,
            inner_message_index: 0,
            inner_language_index: 1,
            inner_context_index: 17,
            inner_conversation_index: 71,
            candidate_text_path: vec![
                PathSegment::Index(4),
                PathSegment::Index(0),
                PathSegment::Index(1),
                PathSegment::Index(0),
            ],
        }
    }
}

impl GeminiWebSchema {
    /// Parse and validate the external Gemini Web positional schema.
    pub fn from_toml(source: &str) -> Result<Self, SchemaLoadError> {
        if source.trim().is_empty() {
            return Err(SchemaLoadError::ParseError("schema is empty".to_owned()));
        }

        let document: SchemaDocument = toml::from_str(source)
            .map_err(|error| SchemaLoadError::ParseError(error.to_string()))?;
        let positions = document
            .positions
            .ok_or(SchemaLoadError::MissingField("positions"))?;
        let raw_path = positions
            .candidate_text_path
            .ok_or(SchemaLoadError::MissingField(
                "positions.candidate_text_path",
            ))?;

        let fallback = Self::default();
        let inner_len = positions.inner_len.unwrap_or(fallback.inner_len);
        let inner_message_index = positions
            .inner_message_index
            .unwrap_or(fallback.inner_message_index);
        let inner_language_index = positions
            .inner_language_index
            .unwrap_or(fallback.inner_language_index);
        let inner_context_index = positions
            .inner_context_index
            .unwrap_or(fallback.inner_context_index);
        let inner_conversation_index = positions
            .inner_conversation_index
            .unwrap_or(fallback.inner_conversation_index);

        let inner_indices = [
            inner_message_index,
            inner_language_index,
            inner_context_index,
            inner_conversation_index,
        ];
        if inner_indices.iter().enumerate().any(|(offset, index)| {
            inner_indices[offset + 1..].contains(index) || *index >= inner_len
        }) {
            return Err(SchemaLoadError::InvalidPath(
                "inner payload positions must be distinct and within inner_len".to_owned(),
            ));
        }

        let candidate_text_path = parse_candidate_path(raw_path)?;
        Ok(Self {
            inner_len,
            inner_message_index,
            inner_language_index,
            inner_context_index,
            inner_conversation_index,
            candidate_text_path,
        })
    }

    /// Load the checked-in schema used by production constructors.
    pub fn bundled() -> Result<Self, SchemaLoadError> {
        Self::from_toml(include_str!("../../../schema/gemini-web.toml"))
    }

    /// Construct the `f.req` wire envelope.
    ///
    /// Gemini Web's StreamGenerate handler expects a two-element outer array
    /// `[null, "<JSON string>"]` whose inner payload is a JSON-encoded string
    /// (verified against the browser's own requests, 2026-10-13). Sending the
    /// positional triple as a plain array makes upstream ignore the prompt:
    /// the request returns HTTP 200 with an answer for an empty conversation.
    ///
    /// The inner payload places the message tuple at `inner_message_index`,
    /// the language tag at `inner_language_index`, the `[[1]]` context marker
    /// at `inner_context_index`, and — on continuation turns — the upstream
    /// conversation ID at `inner_conversation_index`.
    pub(crate) fn build_request_envelope(
        &self,
        user_message: String,
        conversation_id: Option<String>,
        _response_id: Option<String>,
    ) -> Result<Value, GeminiAdapterError> {
        let mut inner = vec![Value::Null; self.inner_len];
        inner[self.inner_message_index] = serde_json::json!([
            user_message,
            0,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            0,
        ]);
        inner[self.inner_language_index] = serde_json::json!(["en"]);
        inner[self.inner_context_index] = serde_json::json!([[1]]);
        if let Some(conversation_id) = conversation_id {
            inner[self.inner_conversation_index] = serde_json::json!([conversation_id]);
        }
        let inner_json =
            serde_json::to_string(&Value::Array(inner)).map_err(|error| {
                GeminiAdapterError::SchemaMismatch(format!(
                    "cannot encode f.req inner payload: {error}"
                ))
            })?;
        Ok(serde_json::json!([Value::Null, inner_json]))
    }

    pub(crate) fn extract_candidate_text<'a>(
        &self,
        value: &'a Value,
    ) -> Result<&'a str, GeminiAdapterError> {
        let mut current = value;
        for segment in &self.candidate_text_path {
            current = match segment {
                PathSegment::Field(field) => current.get(field),
                PathSegment::Index(index) => current.get(*index),
            }
            .ok_or_else(|| {
                GeminiAdapterError::SchemaMismatch(format!(
                    "candidate text path is missing at {segment:?}"
                ))
            })?;
        }
        current.as_str().ok_or_else(|| {
            GeminiAdapterError::SchemaMismatch("candidate text is not a string".to_owned())
        })
    }
}


fn parse_candidate_path(raw_path: Vec<String>) -> Result<Vec<PathSegment>, SchemaLoadError> {
    if raw_path.is_empty() || !raw_path.len().is_multiple_of(2) {
        return Err(SchemaLoadError::InvalidPath(
            "path must contain non-empty type/value pairs".to_owned(),
        ));
    }

    raw_path
        .chunks_exact(2)
        .map(|pair| match pair[0].as_str() {
            "field" if !pair[1].is_empty() => Ok(PathSegment::Field(pair[1].clone())),
            "field" => Err(SchemaLoadError::InvalidPath(
                "field name must not be empty".to_owned(),
            )),
            "index" => pair[1]
                .parse::<usize>()
                .map(PathSegment::Index)
                .map_err(|_| {
                    SchemaLoadError::InvalidPath(format!(
                        "index value {:?} is not a non-negative integer",
                        pair[1]
                    ))
                }),
            kind => Err(SchemaLoadError::InvalidPath(format!(
                "unknown path segment type {kind:?}"
            ))),
        })
        .collect()
}
