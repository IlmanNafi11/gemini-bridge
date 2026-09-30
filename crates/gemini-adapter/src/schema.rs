//! Typed positional contract for the Gemini Web wire shape.
//!
//! Task 0.5 keeps this contract injectable for parser/encoder tests. Loading it
//! from an external file and validating it at startup belong to Task 0.6.

use serde_json::Value;

use crate::GeminiAdapterError;

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
