//! Streaming response parser for Gemini Web's newline-framed protocol.
//!
//! Gemini `StreamGenerate` returns the same newline-framed, anti-XSSI format
//! as the non-streaming path, but may contain multiple JSON frames in one
//! body, each holding a cumulative text snapshot. This module parses the
//! full body and emits [`LlmEvent`]s by driving [`PrefixDiff`] over
//! successive frames.
//!
//! The result is an in-memory stream — no incremental network I/O is needed
//! because `TransportResponse.body` already contains the complete buffered
//! bytes.

use futures::stream;
use gemini_bridge_llm_service::{CompletionSummary, LlmError, LlmEvent, LlmEventStream};
use serde_json::Value;

use crate::prefix_diff::{Delta, PrefixDiff};
use crate::schema::GeminiWebSchema;
use crate::{GeminiAdapterError, find_candidate_text};

/// Parse a complete Gemini `StreamGenerate` body into an ordered sequence of
/// [`LlmEvent`]s and box it as an [`LlmEventStream`].
///
/// * Iterates over newline-framed JSON, extracting cumulative text per frame.
/// * Runs all snapshots through [`PrefixDiff`] to produce minimal deltas.
/// * Appends a terminal [`LlmEvent::Completed`] with finish reason `"stop"`.
/// * Malformed frames are skipped; if no frame matched, returns
///   `GeminiAdapterError::SchemaMismatch`.
pub fn parse_stream_body(
    body: &[u8],
    schema: &GeminiWebSchema,
) -> Result<LlmEventStream, GeminiAdapterError> {
    let response = std::str::from_utf8(body).map_err(|e| {
        GeminiAdapterError::SchemaMismatch(format!("stream body is not UTF-8: {e}"))
    })?;

    let mut diff = PrefixDiff::new();
    let mut events: Vec<Result<LlmEvent, LlmError>> = Vec::new();
    let mut parsed_frame = false;

    for line in response.lines().map(str::trim) {
        if line.is_empty() || line == ")]}'" || line.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let mut latest: Option<String> = None;
        find_candidate_text(&value, schema, &mut latest);

        if let Some(snapshot) = latest {
            parsed_frame = true;
            if let Some(delta) = diff.update(&snapshot) {
                let text = match delta {
                    Delta::Suffix(s) => s,
                    Delta::Reset(s) => s,
                };
                events.push(Ok(LlmEvent::TextDelta(text)));
            }
        }
    }

    if !parsed_frame {
        return Err(GeminiAdapterError::SchemaMismatch(
            "stream body contained no parseable candidate frames".to_owned(),
        ));
    }

    events.push(Ok(LlmEvent::Completed(CompletionSummary {
        finish_reason: "stop".to_owned(),
        usage: None,
    })));

    Ok(Box::pin(stream::iter(events)))
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use gemini_bridge_llm_service::LlmEvent;
    use serde_json::json;

    use crate::schema::GeminiWebSchema;

    use super::*;

    fn frame(text: &str) -> String {
        let inner = json!({
            "candidates": [{"parts": [{"text": text}]}]
        });
        format!(")]}}'\n{inner}\n")
    }

    fn multi_frame(texts: &[&str]) -> String {
        texts.iter().map(|t| frame(t)).collect::<Vec<_>>().join("")
    }

    #[tokio::test]
    async fn single_frame_emits_text_delta_then_completed() {
        let body = frame("Hello!");
        let stream = parse_stream_body(body.as_bytes(), &GeminiWebSchema::default()).unwrap();
        let events: Vec<_> = stream.collect().await;

        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Ok(LlmEvent::TextDelta(t)) if t == "Hello!"));
        assert!(matches!(&events[1], Ok(LlmEvent::Completed(s)) if s.finish_reason == "stop"));
    }

    #[tokio::test]
    async fn multiple_cumulative_frames_produce_suffix_deltas() {
        let body = multi_frame(&["Hello", "Hello world", "Hello world!"]);
        let stream = parse_stream_body(body.as_bytes(), &GeminiWebSchema::default()).unwrap();
        let events: Vec<_> = stream.collect().await;

        // 3 deltas + 1 Completed
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], Ok(LlmEvent::TextDelta(t)) if t == "Hello"));
        assert!(matches!(&events[1], Ok(LlmEvent::TextDelta(t)) if t == " world"));
        assert!(matches!(&events[2], Ok(LlmEvent::TextDelta(t)) if t == "!"));
        assert!(matches!(&events[3], Ok(LlmEvent::Completed(_))));
    }

    #[tokio::test]
    async fn non_prefix_snapshot_emits_reset_text() {
        let body = multi_frame(&["Thinking...", "Final answer"]);
        let stream = parse_stream_body(body.as_bytes(), &GeminiWebSchema::default()).unwrap();
        let events: Vec<_> = stream.collect().await;

        // 2 deltas + 1 Completed
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], Ok(LlmEvent::TextDelta(t)) if t == "Thinking..."));
        assert!(matches!(&events[1], Ok(LlmEvent::TextDelta(t)) if t == "Final answer"));
        assert!(matches!(&events[2], Ok(LlmEvent::Completed(_))));
    }

    #[tokio::test]
    async fn malformed_body_returns_schema_mismatch() {
        let result = parse_stream_body(b"not json at all", &GeminiWebSchema::default());
        assert!(matches!(result, Err(GeminiAdapterError::SchemaMismatch(_))));
    }

    #[tokio::test]
    async fn duplicate_frame_is_deduplicated_by_prefix_diff() {
        let body = multi_frame(&["Hello", "Hello"]);
        let stream = parse_stream_body(body.as_bytes(), &GeminiWebSchema::default()).unwrap();
        let events: Vec<_> = stream.collect().await;

        // Only one TextDelta (the duplicate is a no-op) + Completed
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Ok(LlmEvent::TextDelta(t)) if t == "Hello"));
        assert!(matches!(&events[1], Ok(LlmEvent::Completed(_))));
    }
}
