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
    user_message: Option<usize>,
    conversation_id: Option<usize>,
    response_id: Option<usize>,
    candidate_text_path: Option<Vec<String>>,
}

/// One segment in the path to cumulative candidate text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Field(String),
    Index(usize),
}

/// Positional fields used to build `f.req` and read Gemini Web responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiWebSchema {
    pub user_message: usize,
    pub conversation_id: usize,
    pub response_id: usize,
    pub candidate_text_path: Vec<PathSegment>,
}

impl Default for GeminiWebSchema {
    fn default() -> Self {
        Self {
            user_message: 0,
            conversation_id: 1,
            response_id: 2,
            candidate_text_path: vec![
                PathSegment::Field("candidates".to_owned()),
                PathSegment::Index(0),
                PathSegment::Field("parts".to_owned()),
                PathSegment::Index(0),
                PathSegment::Field("text".to_owned()),
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
        let user_message = positions
            .user_message
            .ok_or(SchemaLoadError::MissingField("positions.user_message"))?;
        let conversation_id = positions
            .conversation_id
            .ok_or(SchemaLoadError::MissingField("positions.conversation_id"))?;
        let response_id = positions
            .response_id
            .ok_or(SchemaLoadError::MissingField("positions.response_id"))?;
        let raw_path = positions
            .candidate_text_path
            .ok_or(SchemaLoadError::MissingField(
                "positions.candidate_text_path",
            ))?;

        if [user_message, conversation_id, response_id]
            .iter()
            .enumerate()
            .any(|(offset, index)| {
                [user_message, conversation_id, response_id][offset + 1..].contains(index)
            })
        {
            return Err(SchemaLoadError::InvalidPath(
                "request positions must be distinct".to_owned(),
            ));
        }

        let candidate_text_path = parse_candidate_path(raw_path)?;
        Ok(Self {
            user_message,
            conversation_id,
            response_id,
            candidate_text_path,
        })
    }

    /// Load the checked-in schema used by production constructors.
    pub fn bundled() -> Result<Self, SchemaLoadError> {
        Self::from_toml(include_str!("../../../schema/gemini-web.toml"))
    }

    /// Construct the positional envelope using only schema-owned indices.
    pub(crate) fn build_request_envelope(
        &self,
        user_message: String,
        conversation_id: Option<String>,
        response_id: Option<String>,
    ) -> Result<Value, GeminiAdapterError> {
        let max_index = self
            .user_message
            .max(self.conversation_id)
            .max(self.response_id);
        let mut envelope = vec![Value::Null; max_index + 1];
        set_unique(
            &mut envelope,
            self.user_message,
            Value::String(user_message),
        )?;
        set_unique(
            &mut envelope,
            self.conversation_id,
            conversation_id.map_or(Value::Null, Value::String),
        )?;
        set_unique(
            &mut envelope,
            self.response_id,
            response_id.map_or(Value::Null, Value::String),
        )?;
        Ok(Value::Array(envelope))
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

fn set_unique(
    envelope: &mut [Value],
    index: usize,
    value: Value,
) -> Result<(), GeminiAdapterError> {
    if !envelope[index].is_null() {
        return Err(GeminiAdapterError::SchemaMismatch(format!(
            "request schema reuses position {index}"
        )));
    }
    envelope[index] = value;
    Ok(())
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
