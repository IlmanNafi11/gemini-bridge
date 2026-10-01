//! Code execution and web search grounding metadata extraction.
//!
//! Extracts structured code execution and citation data from Gemini Web adapter
//! responses and attaches them to OpenAI-compatible responses as optional
//! `gemini_metadata` extension fields.  Standard OpenAI fields are never
//! modified.

pub mod extractor;

pub use extractor::{extract_from_value, extract_metadata, map_code_execution, sanitize_uri};
use serde::{Deserialize, Serialize};

// ── Extension structures ──────────────────────────────────────────────────────

/// Top-level metadata extension field appended to OpenAI chat responses.
///
/// Serialized as `gemini_metadata` at the root of a `ChatCompletionResponse`
/// (non-streaming) or the final terminal `ChatCompletionChunk` (streaming).
/// All collections default to empty; the field is omitted entirely when no
/// metadata is extracted (`#[serde(skip_serializing_if = "Option::is_none")]`
/// is the caller's responsibility).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GeminiMetadataExtension {
    /// Code execution blocks from the response, one per model execution turn.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub code_execution: Vec<ExtractedCodeExecution>,
    /// Grounding/web search citations referenced in the response text.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub citations: Vec<ExtractedCitation>,
    /// Upstream conversation ID for this response, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}

/// A single extracted code execution block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedCodeExecution {
    /// Programming language identifier (e.g. `"python"`, `"javascript"`).
    pub language: String,
    /// Source code submitted for execution.
    pub code: String,
    /// Combined stdout output from code execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    /// Combined stderr output. Populated if upstream isolates error output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

/// A single web search grounding citation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedCitation {
    /// Character offset of the cited text start within the response.
    pub start_index: usize,
    /// Character offset of the cited text end (exclusive) within the response.
    pub end_index: usize,
    /// Sanitized HTTPS URI of the cited source.
    pub uri: String,
    /// Optional source page title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_bridge_adapter_gemini::{CitationRef, CodeExecutionBlock, GeminiWebMetadata};

    fn code_block(lang: &str, code: &str, output: Option<&str>) -> CodeExecutionBlock {
        CodeExecutionBlock {
            language: lang.to_string(),
            code: code.to_string(),
            output: output.map(str::to_string),
        }
    }

    fn citation(start: usize, end: usize, uri: &str, title: Option<&str>) -> CitationRef {
        CitationRef {
            start_index: start,
            end_index: end,
            uri: uri.to_string(),
            title: title.map(str::to_string),
        }
    }

    // ── sanitize_uri ─────────────────────────────────────────────────────────

    #[test]
    fn sanitize_accepts_https() {
        assert_eq!(
            sanitize_uri("https://google.com"),
            Some("https://google.com".to_string())
        );
    }

    #[test]
    fn sanitize_accepts_https_with_path() {
        assert_eq!(
            sanitize_uri("https://example.com/path?q=1"),
            Some("https://example.com/path?q=1".to_string())
        );
    }

    #[test]
    fn sanitize_accepts_localhost_http() {
        assert!(sanitize_uri("http://localhost:8080/data").is_some());
        assert!(sanitize_uri("http://localhost/data").is_some());
        assert!(sanitize_uri("http://127.0.0.1/data").is_some());
    }

    #[test]
    fn sanitize_rejects_insecure_http_external() {
        assert!(sanitize_uri("http://google.com").is_none());
        assert!(sanitize_uri("http://example.com/path").is_none());
    }

    #[test]
    fn sanitize_rejects_file_scheme() {
        assert!(sanitize_uri("file:///etc/passwd").is_none());
    }

    #[test]
    fn sanitize_rejects_javascript_scheme() {
        assert!(sanitize_uri("javascript:alert(1)").is_none());
    }

    #[test]
    fn sanitize_rejects_data_uri() {
        assert!(sanitize_uri("data:text/plain;base64,SGVsbG8=").is_none());
    }

    #[test]
    fn sanitize_rejects_relative_path() {
        assert!(sanitize_uri("../../../etc/passwd").is_none());
        assert!(sanitize_uri("/etc/passwd").is_none());
    }

    #[test]
    fn sanitize_rejects_malformed() {
        assert!(sanitize_uri("not a url").is_none());
        assert!(sanitize_uri("").is_none());
    }

    // ── map_code_execution ────────────────────────────────────────────────────

    #[test]
    fn map_code_execution_maps_stdout() {
        let block = code_block("python", "print('hi')", Some("hi\n"));
        let result = map_code_execution(&block).unwrap();
        assert_eq!(result.language, "python");
        assert_eq!(result.code, "print('hi')");
        assert_eq!(result.stdout, Some("hi\n".to_string()));
        assert!(result.stderr.is_none());
    }

    #[test]
    fn map_code_execution_no_output_produces_none_stdout() {
        let block = code_block("python", "x = 1", None);
        let result = map_code_execution(&block).unwrap();
        assert!(result.stdout.is_none());
    }

    #[test]
    fn map_code_execution_empty_output_produces_none_stdout() {
        let block = code_block("python", "x = 1", Some(""));
        let result = map_code_execution(&block).unwrap();
        assert!(result.stdout.is_none());
    }

    #[test]
    fn map_code_execution_rejects_empty_language() {
        let block = code_block("", "print(1)", None);
        assert!(map_code_execution(&block).is_none());
    }

    #[test]
    fn map_code_execution_rejects_empty_code() {
        let block = code_block("python", "", None);
        assert!(map_code_execution(&block).is_none());
    }

    // ── extract_metadata ──────────────────────────────────────────────────────

    #[test]
    fn extract_metadata_returns_none_for_empty() {
        let meta = GeminiWebMetadata::default();
        assert!(extract_metadata(&meta).is_none());
    }

    #[test]
    fn extract_metadata_returns_none_when_only_response_id_present() {
        let meta = GeminiWebMetadata {
            response_id: Some("rid".to_string()),
            ..Default::default()
        };
        assert!(extract_metadata(&meta).is_none());
    }

    #[test]
    fn extract_metadata_single_code_block() {
        let meta = GeminiWebMetadata {
            code_execution: vec![code_block("python", "print(42)", Some("42\n"))],
            ..Default::default()
        };
        let ext = extract_metadata(&meta).unwrap();
        assert_eq!(ext.code_execution.len(), 1);
        assert_eq!(ext.code_execution[0].language, "python");
        assert_eq!(ext.code_execution[0].stdout, Some("42\n".to_string()));
        assert!(ext.citations.is_empty());
        assert!(ext.conversation_id.is_none());
    }

    #[test]
    fn extract_metadata_multiple_code_blocks_preserved_in_order() {
        let meta = GeminiWebMetadata {
            code_execution: vec![
                code_block("python", "print(1)", Some("1\n")),
                code_block("javascript", "console.log(2)", Some("2\n")),
            ],
            ..Default::default()
        };
        let ext = extract_metadata(&meta).unwrap();
        assert_eq!(ext.code_execution.len(), 2);
        assert_eq!(ext.code_execution[0].language, "python");
        assert_eq!(ext.code_execution[1].language, "javascript");
    }

    #[test]
    fn extract_metadata_citations_with_valid_https() {
        let meta = GeminiWebMetadata {
            citations: vec![citation(0, 10, "https://example.com", Some("Example"))],
            ..Default::default()
        };
        let ext = extract_metadata(&meta).unwrap();
        assert_eq!(ext.citations.len(), 1);
        assert_eq!(ext.citations[0].uri, "https://example.com");
        assert_eq!(ext.citations[0].title, Some("Example".to_string()));
    }

    #[test]
    fn extract_metadata_insecure_citation_omitted() {
        let meta = GeminiWebMetadata {
            citations: vec![citation(0, 10, "http://insecure.com", None)],
            ..Default::default()
        };
        assert!(extract_metadata(&meta).is_none());
    }

    #[test]
    fn extract_metadata_malformed_block_skipped_others_kept() {
        let meta = GeminiWebMetadata {
            code_execution: vec![
                code_block("", "bad block", None),
                code_block("python", "print(1)", None),
            ],
            ..Default::default()
        };
        let ext = extract_metadata(&meta).unwrap();
        assert_eq!(ext.code_execution.len(), 1);
        assert_eq!(ext.code_execution[0].language, "python");
    }

    #[test]
    fn extract_metadata_mixed_valid_invalid_citations() {
        let meta = GeminiWebMetadata {
            citations: vec![
                citation(0, 5, "https://valid.com", None),
                citation(5, 10, "http://insecure.com", None),
            ],
            ..Default::default()
        };
        let ext = extract_metadata(&meta).unwrap();
        assert_eq!(ext.citations.len(), 1);
        assert_eq!(ext.citations[0].uri, "https://valid.com");
    }

    #[test]
    fn extract_metadata_conversation_id_included() {
        let meta = GeminiWebMetadata {
            conversation_id: Some("conv-abc".to_string()),
            ..Default::default()
        };
        let ext = extract_metadata(&meta).unwrap();
        assert_eq!(ext.conversation_id, Some("conv-abc".to_string()));
        assert!(ext.code_execution.is_empty());
        assert!(ext.citations.is_empty());
    }

    #[test]
    fn extract_metadata_both_code_and_citations() {
        let meta = GeminiWebMetadata {
            code_execution: vec![code_block("python", "x=1", None)],
            citations: vec![citation(
                0,
                10,
                "https://example.org/paper",
                Some("A Paper"),
            )],
            conversation_id: Some("cid".to_string()),
            ..Default::default()
        };
        let ext = extract_metadata(&meta).unwrap();
        assert_eq!(ext.code_execution.len(), 1);
        assert_eq!(ext.citations.len(), 1);
        assert_eq!(ext.conversation_id, Some("cid".to_string()));
    }

    #[test]
    fn extract_from_value_deserializes_json_correctly() {
        let val = serde_json::json!({
            "code_execution": [{
                "language": "python",
                "code": "print('ok')",
                "output": "ok\n"
            }],
            "citations": [{
                "start_index": 0,
                "end_index": 5,
                "uri": "https://example.com/source",
                "title": "Source"
            }],
            "conversation_id": "conv-123"
        });
        let ext = extract_from_value(&val).unwrap();
        assert_eq!(ext.code_execution.len(), 1);
        assert_eq!(ext.code_execution[0].stdout, Some("ok\n".to_string()));
        assert_eq!(ext.citations.len(), 1);
        assert_eq!(ext.conversation_id, Some("conv-123".to_string()));
    }

    #[test]
    fn gemini_metadata_extension_serializes_without_empty_fields() {
        let ext = GeminiMetadataExtension {
            code_execution: vec![],
            citations: vec![],
            conversation_id: None,
        };
        let value = serde_json::to_value(&ext).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("code_execution"));
        assert!(!obj.contains_key("citations"));
        assert!(!obj.contains_key("conversation_id"));
    }

    #[test]
    fn gemini_metadata_extension_serializes_populated() {
        let ext = GeminiMetadataExtension {
            code_execution: vec![ExtractedCodeExecution {
                language: "python".to_string(),
                code: "print(1)".to_string(),
                stdout: Some("1\n".to_string()),
                stderr: None,
            }],
            citations: vec![ExtractedCitation {
                start_index: 0,
                end_index: 10,
                uri: "https://example.com".to_string(),
                title: None,
            }],
            conversation_id: Some("c1".to_string()),
        };
        let value = serde_json::to_value(&ext).unwrap();
        assert_eq!(value["code_execution"][0]["language"], "python");
        assert_eq!(value["citations"][0]["uri"], "https://example.com");
        assert!(
            value["code_execution"][0].get("stderr").is_none()
                || value["code_execution"][0]["stderr"].is_null()
        );
    }
}
