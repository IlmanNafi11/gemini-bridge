//! Incremental parser for Gemini Web's newline-framed streaming protocol.
//!
//! The parser consumes transport chunks as they arrive, buffers only the current
//! incomplete frame, and converts cumulative text snapshots into deltas.

use std::collections::VecDeque;

use futures::{StreamExt, stream};
use gemini_bridge_llm_service::{
    CompletionSummary, LlmError, LlmEvent, LlmEventStream, ProviderMetadata,
};
use gemini_bridge_transport::{TransportByteStream, TransportError};
use serde_json::Value;

use crate::prefix_diff::{Delta, PrefixDiff};
use crate::schema::GeminiWebSchema;
use crate::{
    GeminiAdapterError, GeminiWebMetadata, find_candidate_text, parse_non_stream_metadata,
};

/// Maximum bytes retained for one newline-delimited upstream frame.
pub const MAX_STREAM_FRAME_BYTES: usize = 1024 * 1024;

/// Parse a complete response body and return its events immediately.
///
/// This retains the original buffered parser contract. Production streaming
/// uses [`parse_stream`] so it does not wait for response completion.
pub fn parse_stream_body(
    body: &[u8],
    schema: &GeminiWebSchema,
) -> Result<LlmEventStream, GeminiAdapterError> {
    let response = std::str::from_utf8(body).map_err(|error| {
        GeminiAdapterError::SchemaMismatch(format!("stream body is not UTF-8: {error}"))
    })?;
    let metadata = parse_non_stream_metadata(body).map(|metadata| ProviderMetadata {
        raw: metadata_to_json(&metadata),
    });
    let mut diff = PrefixDiff::new();
    let mut events: Vec<Result<LlmEvent, LlmError>> = Vec::new();
    let mut parsed_frame = false;

    for line in response.lines().map(str::trim) {
        if line.is_empty() || line == ")]}'" || line.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let mut latest = None;
        find_candidate_text(&value, schema, &mut latest);
        if let Some(snapshot) = latest {
            parsed_frame = true;
            if let Some(delta) = diff.update(&snapshot) {
                let text = match delta {
                    Delta::Suffix(text) | Delta::Reset(text) => text,
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
        metadata,
    })));
    Ok(Box::pin(stream::iter(events)))
}

fn metadata_to_json(metadata: &GeminiWebMetadata) -> Value {
    serde_json::json!({
        "conversation_id": metadata.conversation_id,
        "response_id": metadata.response_id,
        "candidate_id": metadata.candidate_id,
        "code_execution": metadata.code_execution,
        "citations": metadata.citations,
    })
}

/// Parse response chunks incrementally into provider-neutral LLM events.
///
/// Dropping the returned stream drops `input`, which in production owns the
/// reqwest response body and therefore cancels/releases the upstream read.
pub fn parse_stream(input: TransportByteStream, schema: GeminiWebSchema) -> LlmEventStream {
    let state = ParserState {
        input,
        schema,
        frame: Vec::new(),
        diff: PrefixDiff::new(),
        metadata: GeminiWebMetadata::default(),
        parsed_frame: false,
        pending: VecDeque::new(),
        finished: false,
    };

    Box::pin(stream::unfold(state, |mut state| async move {
        let event = state.next_event().await?;
        Some((event, state))
    }))
}

struct ParserState {
    input: TransportByteStream,
    schema: GeminiWebSchema,
    frame: Vec<u8>,
    diff: PrefixDiff,
    metadata: GeminiWebMetadata,
    parsed_frame: bool,
    pending: VecDeque<Result<LlmEvent, LlmError>>,
    finished: bool,
}

impl ParserState {
    async fn next_event(&mut self) -> Option<Result<LlmEvent, LlmError>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            if self.finished {
                return None;
            }

            match self.input.next().await {
                Some(Ok(chunk)) => {
                    if let Err(error) = self.consume_chunk(&chunk) {
                        self.fail(error);
                    }
                }
                Some(Err(error)) => self.fail(map_transport_error(error)),
                None => self.finish(),
            }
        }
    }

    fn consume_chunk(&mut self, chunk: &[u8]) -> Result<(), LlmError> {
        let mut remaining = chunk;
        while let Some(newline) = remaining.iter().position(|byte| *byte == b'\n') {
            let (part, tail) = remaining.split_at(newline + 1);
            self.push_frame_bytes(part)?;
            let frame = std::mem::take(&mut self.frame);
            self.parse_frame(&frame)?;
            remaining = tail;
        }
        self.push_frame_bytes(remaining)
    }

    fn push_frame_bytes(&mut self, bytes: &[u8]) -> Result<(), LlmError> {
        if self.frame.len().saturating_add(bytes.len()) > MAX_STREAM_FRAME_BYTES {
            return Err(protocol_error(format!(
                "upstream frame exceeds {MAX_STREAM_FRAME_BYTES} byte limit"
            )));
        }
        self.frame.extend_from_slice(bytes);
        Ok(())
    }

    fn parse_frame(&mut self, frame: &[u8]) -> Result<(), LlmError> {
        let line = std::str::from_utf8(frame)
            .map_err(|error| protocol_error(format!("stream frame is not UTF-8: {error}")))?
            .trim();
        if line.is_empty() || line == ")]}'" || line.bytes().all(|byte| byte.is_ascii_digit()) {
            return Ok(());
        }

        let value: Value = serde_json::from_str(line)
            .map_err(|error| protocol_error(format!("invalid stream JSON frame: {error}")))?;
        if let Some(metadata) = parse_non_stream_metadata(line.as_bytes()) {
            merge_metadata(&mut self.metadata, metadata);
        }

        let mut latest = None;
        find_candidate_text(&value, &self.schema, &mut latest);
        if let Some(snapshot) = latest {
            self.parsed_frame = true;
            if let Some(delta) = self.diff.update(&snapshot) {
                let text = match delta {
                    Delta::Suffix(text) | Delta::Reset(text) => text,
                };
                self.pending.push_back(Ok(LlmEvent::TextDelta(text)));
            }
        }
        Ok(())
    }

    fn finish(&mut self) {
        if !self.frame.is_empty() {
            let frame = std::mem::take(&mut self.frame);
            if let Err(error) = self.parse_frame(&frame) {
                self.fail(error);
                return;
            }
        }

        self.finished = true;
        if !self.parsed_frame {
            self.pending.push_back(Err(protocol_error(
                "stream body contained no parseable candidate frames",
            )));
            return;
        }

        let metadata = metadata_provider(&self.metadata);
        self.pending
            .push_back(Ok(LlmEvent::Completed(CompletionSummary {
                finish_reason: "stop".to_owned(),
                usage: None,
                metadata,
            })));
    }

    fn fail(&mut self, error: LlmError) {
        self.finished = true;
        self.frame.clear();
        self.pending.clear();
        self.pending.push_back(Err(error));
    }
}

fn protocol_error(message: impl Into<String>) -> LlmError {
    LlmError::Protocol(message.into())
}

fn map_transport_error(_error: TransportError) -> LlmError {
    LlmError::Unavailable
}

fn merge_metadata(target: &mut GeminiWebMetadata, incoming: GeminiWebMetadata) {
    if incoming.conversation_id.is_some() {
        target.conversation_id = incoming.conversation_id;
    }
    if incoming.response_id.is_some() {
        target.response_id = incoming.response_id;
    }
    if incoming.candidate_id.is_some() {
        target.candidate_id = incoming.candidate_id;
    }
    for block in incoming.code_execution {
        if !target.code_execution.contains(&block) {
            target.code_execution.push(block);
        }
    }
    for citation in incoming.citations {
        if !target.citations.contains(&citation) {
            target.citations.push(citation);
        }
    }
}

fn metadata_provider(metadata: &GeminiWebMetadata) -> Option<ProviderMetadata> {
    if metadata == &GeminiWebMetadata::default() {
        return None;
    }
    Some(ProviderMetadata {
        raw: serde_json::json!({
            "conversation_id": metadata.conversation_id,
            "response_id": metadata.response_id,
            "candidate_id": metadata.candidate_id,
            "code_execution": metadata.code_execution,
            "citations": metadata.citations,
        }),
    })
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures::{StreamExt, stream};
    use gemini_bridge_llm_service::LlmEvent;
    use serde_json::json;

    use super::*;

    fn frame(text: &str) -> String {
        format!(
            ")]}}'\n{}\n",
            json!([null, null, null, null, [["candidate", [text]]]])
        )
    }

    fn byte_stream(chunks: Vec<Result<Bytes, TransportError>>) -> TransportByteStream {
        Box::pin(stream::iter(chunks))
    }

    #[tokio::test]
    async fn partial_frames_across_transport_chunks_parse_incrementally() {
        let response = format!("{}{}", frame("Hello"), frame("Hello world"));
        let split_a = 7;
        let split_b = response.len() - 5;
        let input = byte_stream(vec![
            Ok(Bytes::copy_from_slice(&response.as_bytes()[..split_a])),
            Ok(Bytes::copy_from_slice(
                &response.as_bytes()[split_a..split_b],
            )),
            Ok(Bytes::copy_from_slice(&response.as_bytes()[split_b..])),
        ]);

        let events: Vec<_> = parse_stream(input, GeminiWebSchema::default())
            .collect()
            .await;
        assert!(matches!(&events[0], Ok(LlmEvent::TextDelta(text)) if text == "Hello"));
        assert!(matches!(&events[1], Ok(LlmEvent::TextDelta(text)) if text == " world"));
        assert!(matches!(&events[2], Ok(LlmEvent::Completed(_))));
    }

    #[tokio::test]
    async fn malformed_mid_stream_frame_maps_to_protocol_error_without_completion() {
        let input = byte_stream(vec![
            Ok(Bytes::from(frame("Hello"))),
            Ok(Bytes::from_static(b"{not-json}\n")),
        ]);

        let events: Vec<_> = parse_stream(input, GeminiWebSchema::default())
            .collect()
            .await;
        assert!(matches!(&events[0], Ok(LlmEvent::TextDelta(text)) if text == "Hello"));
        assert!(matches!(&events[1], Err(LlmError::Protocol(_))));
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn mid_stream_transport_failure_maps_to_unavailable_without_completion() {
        let input = byte_stream(vec![
            Ok(Bytes::from(frame("Hello"))),
            Err(TransportError::Network("connection reset".to_owned())),
        ]);

        let events: Vec<_> = parse_stream(input, GeminiWebSchema::default())
            .collect()
            .await;
        assert!(matches!(&events[0], Ok(LlmEvent::TextDelta(text)) if text == "Hello"));
        assert!(matches!(&events[1], Err(LlmError::Unavailable)));
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn incomplete_frame_buffer_is_bounded() {
        let input = byte_stream(vec![Ok(Bytes::from(vec![
            b'x';
            MAX_STREAM_FRAME_BYTES + 1
        ]))]);

        let events: Vec<_> = parse_stream(input, GeminiWebSchema::default())
            .collect()
            .await;
        assert!(
            matches!(&events[0], Err(LlmError::Protocol(message)) if message.contains("exceeds"))
        );
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn buffered_compatibility_parser_emits_delta_then_completed() {
        let body = frame("Hello!");
        let events: Vec<_> = parse_stream_body(body.as_bytes(), &GeminiWebSchema::default())
            .unwrap()
            .collect()
            .await;
        assert!(matches!(&events[0], Ok(LlmEvent::TextDelta(text)) if text == "Hello!"));
        assert!(matches!(&events[1], Ok(LlmEvent::Completed(_))));
    }
}
