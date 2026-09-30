//! Schema self-check: validates the positional schema against a representative
//! Gemini Web response body. Call this at startup (or on schema load) to catch
//! index drift before it causes runtime `SchemaMismatch` errors.
//!
//! Self-check is intentionally pure — it does not touch the network. Callers
//! supply the probe text (typically the checked-in fixture).

use thiserror::Error;

use crate::parse_non_stream_response;
use crate::schema::GeminiWebSchema;

/// Errors surfaced when the positional schema does not match the probe text.
#[derive(Debug, Error)]
pub enum SelfCheckError {
    /// No JSON frames were found in the probe — the probe itself may be corrupt.
    #[error("self-check probe contains no parseable JSON frames")]
    NoFramesParsed,
    /// Candidate text extraction failed: the configured path did not navigate
    /// to a string node inside the probe.
    #[error("candidate text path did not match probe response: {0}")]
    CandidatePathMismatch(String),
}

/// Run the self-check against the supplied probe text.
///
/// `probe` is a Gemini Web newline-framed response body — the same format as a
/// real upstream response. The checked-in `fixtures/sample_response.txt` is the
/// canonical probe for unit tests and startup validation.
///
/// Returns `Ok(())` when the schema resolves at least one candidate text string
/// from the probe. Returns `Err(SelfCheckError::NoFramesParsed)` when the probe
/// body has no parseable JSON frames. Returns `Err(SelfCheckError::CandidatePathMismatch)`
/// when frames exist but the configured path does not reach a string value.
pub fn run_self_check(schema: &GeminiWebSchema, probe: &str) -> Result<(), SelfCheckError> {
    use crate::GeminiAdapterError;

    match parse_non_stream_response(probe.as_bytes(), schema) {
        Ok(_text) => Ok(()),
        Err(GeminiAdapterError::SchemaMismatch(msg)) => {
            if msg.contains("no JSON frames") {
                Err(SelfCheckError::NoFramesParsed)
            } else {
                Err(SelfCheckError::CandidatePathMismatch(msg))
            }
        }
        Err(other) => Err(SelfCheckError::CandidatePathMismatch(other.to_string())),
    }
}
