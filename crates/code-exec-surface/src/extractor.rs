//! Extractor functions for Gemini Web metadata.

use gemini_bridge_adapter_gemini::{CitationRef, CodeExecutionBlock, GeminiWebMetadata};

use crate::{ExtractedCitation, ExtractedCodeExecution, GeminiMetadataExtension};

/// Extract structured metadata from an adapter-provided `GeminiWebMetadata`.
///
/// Returns `None` when the metadata object contains no code execution blocks
/// or citations *and* no conversation ID; callers should omit `gemini_metadata`
/// from the response in that case.
///
/// Silently skips blocks or citations that fail validation or sanitization.
pub fn extract_metadata(meta: &GeminiWebMetadata) -> Option<GeminiMetadataExtension> {
    let code_execution: Vec<ExtractedCodeExecution> = meta
        .code_execution
        .iter()
        .filter_map(map_code_execution)
        .collect();

    let citations: Vec<ExtractedCitation> =
        meta.citations.iter().filter_map(map_citation).collect();

    let conversation_id = meta.conversation_id.clone();

    if code_execution.is_empty() && citations.is_empty() && conversation_id.is_none() {
        return None;
    }

    Some(GeminiMetadataExtension {
        code_execution,
        citations,
        conversation_id,
    })
}

/// Extract structured metadata from an opaque JSON value (e.g. from `ProviderMetadata`).
///
/// Deserializes known fields (`code_execution`, `citations`, `conversation_id`)
/// without failing on extra or missing fields.
pub fn extract_from_value(value: &serde_json::Value) -> Option<GeminiMetadataExtension> {
    let mut code_execution: Vec<ExtractedCodeExecution> = Vec::new();
    let mut citations: Vec<ExtractedCitation> = Vec::new();

    if let Some(arr) = value.get("code_execution").and_then(|v| v.as_array()) {
        for item in arr {
            let lang = item.get("language").and_then(|v| v.as_str()).unwrap_or("");
            let code = item.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let stdout = item
                .get("output")
                .or_else(|| item.get("stdout"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let stderr = item
                .get("stderr")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);

            if !lang.is_empty() && !code.is_empty() {
                code_execution.push(ExtractedCodeExecution {
                    language: lang.to_string(),
                    code: code.to_string(),
                    stdout,
                    stderr,
                });
            }
        }
    }

    if let Some(arr) = value.get("citations").and_then(|v| v.as_array()) {
        for item in arr {
            let start = item
                .get("start_index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let end = item.get("end_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let uri_raw = item.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            let title = item
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            if let Some(uri) = sanitize_uri(uri_raw) {
                citations.push(ExtractedCitation {
                    start_index: start,
                    end_index: end,
                    uri,
                    title,
                });
            }
        }
    }

    let conversation_id = value
        .get("conversation_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    if code_execution.is_empty() && citations.is_empty() && conversation_id.is_none() {
        return None;
    }

    Some(GeminiMetadataExtension {
        code_execution,
        citations,
        conversation_id,
    })
}

/// Map an adapter `CodeExecutionBlock` into an `ExtractedCodeExecution`.
///
/// The adapter's `output` field maps to `stdout`; `stderr` remains `None`
/// unless Gemini Web later surfaces separate error channels. Returns `None`
/// when `language` or `code` is empty (considered malformed).
pub fn map_code_execution(block: &CodeExecutionBlock) -> Option<ExtractedCodeExecution> {
    if block.language.is_empty() || block.code.is_empty() {
        return None;
    }
    Some(ExtractedCodeExecution {
        language: block.language.clone(),
        code: block.code.clone(),
        stdout: block.output.clone().filter(|s| !s.is_empty()),
        stderr: None,
    })
}

/// Sanitize a citation URI.
///
/// Accepts only absolute `https://` URLs and, for test environments,
/// `http://localhost[:<port>]` and `http://127.0.0.1[:<port>]` URLs.
/// All other schemes (`http://` to non-localhost hosts, `file://`,
/// `javascript:`, `data:`, relative paths) are rejected and return `None`.
pub fn sanitize_uri(raw: &str) -> Option<String> {
    use url::Url;
    let parsed = Url::parse(raw).ok()?;
    match parsed.scheme() {
        "https" => Some(raw.to_string()),
        "http" => {
            let host = parsed.host_str()?;
            if host == "localhost" || host == "127.0.0.1" {
                Some(raw.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn map_citation(r: &CitationRef) -> Option<ExtractedCitation> {
    let uri = sanitize_uri(&r.uri)?;
    Some(ExtractedCitation {
        start_index: r.start_index,
        end_index: r.end_index,
        uri,
        title: r.title.clone(),
    })
}
