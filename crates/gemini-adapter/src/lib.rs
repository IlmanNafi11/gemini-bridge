//! Gemini Web provider adapter.
//!
//! Task 0.5 implements authenticated non-streaming generation. Task 0.6 adds
//! externalized schema loading and a startup self-check. Task 1.1 adds the
//! streaming SSE path with a prefix-diff engine.

pub mod prefix_diff;
pub mod schema;
pub mod self_check;
pub mod stream;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use gemini_bridge_config::BridgeConfig;
use gemini_bridge_identity::{DefaultIdentityService, IdentityError, IdentityService};
use gemini_bridge_llm_service::{
    Completion, ContentPart, LlmAdapter, LlmError, LlmEventStream, LlmRequest, Role,
};
use gemini_bridge_transport::{
    Idempotency, ReqwestTransport, TransportRequest, TransportService, TransportStreamResponse,
};
use http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use schema::GeminiWebSchema;
use self_check::run_self_check;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use url::Url;

const SAMPLE_FIXTURE: &str = include_str!("../fixtures/sample_response.txt");
const GEMINI_BASE_URL: &str = "https://gemini.google.com";
const STREAM_GENERATE_PATH: &str =
    "/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate";
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(100_000);

#[derive(Debug, Error)]
pub enum GeminiAdapterError {
    #[error("Upstream returned 405 Method Not Allowed / Stale BL")]
    StaleBuildLabel,
    #[error("Upstream rate limited (429)")]
    RateLimited,
    #[error("Session authentication required (401/expired)")]
    NeedsAuth,
    #[error("Upstream schema parsing failed: {0}")]
    SchemaMismatch(String),
    #[error("Gemini Web flagged IP / bot protection")]
    IpFlagged,
    #[error("Network/transport failure: {0}")]
    Transport(String),
}

pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<StreamChunk, GeminiAdapterError>> + Send>>;
pub type NormalizedLlmRequest = Arc<LlmRequest>;
pub type NormalizedLlmResponse = Completion;

#[derive(Debug, Clone, PartialEq)]
pub struct StreamChunk {
    pub delta_text: Option<String>,
    pub is_finished: bool,
    pub finish_reason: Option<String>,
    pub metadata: Option<GeminiWebMetadata>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GeminiWebMetadata {
    pub conversation_id: Option<String>,
    pub response_id: Option<String>,
    pub candidate_id: Option<String>,
    pub code_execution: Vec<CodeExecutionBlock>,
    pub citations: Vec<CitationRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeExecutionBlock {
    pub language: String,
    pub code: String,
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationRef {
    pub start_index: usize,
    pub end_index: usize,
    pub uri: String,
    pub title: Option<String>,
}

#[async_trait]
pub trait GeminiAdapter: Send + Sync {
    async fn generate_non_stream(
        &self,
        req: NormalizedLlmRequest,
    ) -> Result<NormalizedLlmResponse, GeminiAdapterError>;

    async fn generate_stream(
        &self,
        req: NormalizedLlmRequest,
    ) -> Result<ChunkStream, GeminiAdapterError>;
}

/// Gemini Web implementation backed by the shared identity and transport layers.
pub struct DefaultGeminiAdapter {
    identity: Arc<DefaultIdentityService>,
    transport: ReqwestTransport,
    base_url: String,
    schema: GeminiWebSchema,
}

impl DefaultGeminiAdapter {
    pub fn new(
        identity: Arc<DefaultIdentityService>,
        config: Arc<BridgeConfig>,
    ) -> Result<Self, GeminiAdapterError> {
        let schema = Self::bundled_schema()?;
        Self::with_base_url_and_schema(identity, config, GEMINI_BASE_URL, schema)
    }

    /// Load and validate the checked-in positional schema, failing fast on
    /// startup when the bundled contract does not match the sample fixture.
    fn bundled_schema() -> Result<GeminiWebSchema, GeminiAdapterError> {
        let schema = GeminiWebSchema::bundled().map_err(|error| {
            GeminiAdapterError::SchemaMismatch(format!("cannot load bundled schema: {error}"))
        })?;
        run_self_check(&schema, SAMPLE_FIXTURE).map_err(|error| {
            GeminiAdapterError::SchemaMismatch(format!("startup self-check failed: {error}"))
        })?;
        Ok(schema)
    }

    /// Construct against a custom upstream URL. This keeps localhost wire tests
    /// real while the production constructor remains fixed to Gemini Web.
    pub fn with_base_url(
        identity: Arc<DefaultIdentityService>,
        config: Arc<BridgeConfig>,
        base_url: impl Into<String>,
    ) -> Result<Self, GeminiAdapterError> {
        Self::with_base_url_and_schema(identity, config, base_url, GeminiWebSchema::default())
    }

    /// Construct with an injectable positional contract for drift/parser tests.
    pub fn with_base_url_and_schema(
        identity: Arc<DefaultIdentityService>,
        config: Arc<BridgeConfig>,
        base_url: impl Into<String>,
        schema: GeminiWebSchema,
    ) -> Result<Self, GeminiAdapterError> {
        let transport = ReqwestTransport::new(&config.transport)
            .map_err(|error| GeminiAdapterError::Transport(error.to_string()))?;
        Ok(Self {
            identity,
            transport,
            base_url: base_url.into(),
            schema,
        })
    }

    /// Build the `StreamGenerate` transport request: bootstrap, prompt envelope,
    /// auth headers, and the `_reqid`/`rt`/`hl`/`bl` query contract.
    async fn build_wire_transport_request(
        &self,
        request: &LlmRequest,
    ) -> Result<TransportRequest, GeminiAdapterError> {
        let bootstrap = self
            .identity
            .bootstrap()
            .await
            .map_err(map_identity_error)?;
        let message = normalized_prompt(request)?;
        let conversation_id = metadata_string(request, "conversation_id")?;
        let response_id = metadata_string(request, "response_id")?;
        let envelope = self.schema.build_request_envelope(
            message,
            conversation_id.map(str::to_owned),
            response_id.map(str::to_owned),
        )?;
        let envelope_json = serde_json::to_string(&envelope).map_err(|error| {
            GeminiAdapterError::SchemaMismatch(format!("cannot encode f.req: {error}"))
        })?;
        // Gemini Web requires the anti-XSRF `at` token (the `SNlM0e` value from
        // the bootstrap page) alongside `f.req`; without it upstream rejects the
        // request with HTTP 400 (missing anti-XSRF token).
        let form_body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("f.req", &envelope_json)
            .append_pair("at", &bootstrap.snlm0e)
            .finish();

        let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let mut url = Url::parse(&format!(
            "{}{STREAM_GENERATE_PATH}",
            self.base_url.trim_end_matches('/')
        ))
        .map_err(|error| GeminiAdapterError::Transport(error.to_string()))?;
        url.query_pairs_mut()
            .append_pair("_reqid", &request_id.to_string())
            .append_pair("rt", "c")
            .append_pair("hl", "en")
            .append_pair("bl", &bootstrap.bl);

        let mut headers = HeaderMap::new();
        self.identity
            .apply_auth_headers(&mut headers)
            .map_err(map_identity_error)?;
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded;charset=UTF-8"),
        );

        Ok(TransportRequest {
            method: Method::POST,
            url,
            headers,
            body: Some(Bytes::from(form_body)),
            idempotency: Idempotency::NeverRetry,
        })
    }

    /// Map an upstream `StreamGenerate` status to the adapter error taxonomy.
    ///
    /// Shared by the buffered and incremental paths so both surfaces report
    /// identical errors for the same upstream status.
    fn check_stream_status(
        status: StatusCode,
        headers: &HeaderMap,
    ) -> Result<(), GeminiAdapterError> {
        match status {
            StatusCode::METHOD_NOT_ALLOWED => Err(GeminiAdapterError::StaleBuildLabel),
            StatusCode::TOO_MANY_REQUESTS => Err(GeminiAdapterError::RateLimited),
            StatusCode::UNAUTHORIZED => Err(GeminiAdapterError::NeedsAuth),
            StatusCode::FOUND | StatusCode::MOVED_PERMANENTLY => {
                if headers
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|location| location.contains("sorry"))
                {
                    Err(GeminiAdapterError::IpFlagged)
                } else {
                    Ok(())
                }
            }
            status if !status.is_success() => Err(GeminiAdapterError::Transport(format!(
                "upstream returned HTTP {status}"
            ))),
            _ => Ok(()),
        }
    }

    /// Execute the `StreamGenerate` upstream request and return the raw response
    /// body bytes. The non-streaming path uses this method.
    async fn execute_wire_request(
        &self,
        request: &LlmRequest,
    ) -> Result<Bytes, GeminiAdapterError> {
        let transport_request = self.build_wire_transport_request(request).await?;
        let response = match self.transport.execute(transport_request).await {
            Ok(res) => res,
            Err(error) => {
                tracing::warn!(%error, "upstream wire transport execute failed");
                return Err(GeminiAdapterError::Transport(error.to_string()));
            }
        };
        if let Err(error) = Self::check_stream_status(response.status, &response.headers) {
            tracing::warn!(status = %response.status, %error, "upstream wire returned non-success status");
            return Err(error);
        }
        Ok(response.body)
    }

    /// Execute the `StreamGenerate` upstream request and return the response
    /// with its body still open as an incremental byte stream. The streaming
    /// path uses this method; dropping the body stream cancels the upstream read.
    async fn execute_wire_stream_request(
        &self,
        request: &LlmRequest,
    ) -> Result<TransportStreamResponse, GeminiAdapterError> {
        let transport_request = self.build_wire_transport_request(request).await?;
        let response = self
            .transport
            .execute_stream(transport_request)
            .await
            .map_err(|error| GeminiAdapterError::Transport(error.to_string()))?;
        Self::check_stream_status(response.status, &response.headers)?;
        Ok(response)
    }

    async fn execute_wire_stream_with_recovery(
        &self,
        request: &LlmRequest,
    ) -> Result<TransportStreamResponse, GeminiAdapterError> {
        match self.execute_wire_stream_request(request).await {
            Ok(response) => Ok(response),
            Err(GeminiAdapterError::StaleBuildLabel) => {
                self.identity
                    .refresh_1psidts()
                    .await
                    .map_err(map_identity_error)?;
                self.execute_wire_stream_request(request).await
            }
            other => other,
        }
    }

    async fn stream_events(
        &self,
        request: &LlmRequest,
    ) -> Result<LlmEventStream, GeminiAdapterError> {
        let response = self.execute_wire_stream_with_recovery(request).await?;
        Ok(crate::stream::parse_stream(
            response.body,
            self.schema.clone(),
        ))
    }
    /// On `StaleBuildLabel` (405): refresh `__Secure-1PSIDTS` via `identity`,
    /// re-execute the wire request (one fresh bootstrap and one retry), and
    /// preserve the retry's error taxonomy. 429/IpFlagged never enter this path.
    async fn execute_wire_with_recovery(
        &self,
        request: &LlmRequest,
    ) -> Result<Bytes, GeminiAdapterError> {
        match self.execute_wire_request(request).await {
            Ok(body) => Ok(body),
            Err(GeminiAdapterError::StaleBuildLabel) => {
                self.identity
                    .refresh_1psidts()
                    .await
                    .map_err(map_identity_error)?;
                self.execute_wire_request(request).await
            }
            other => other,
        }
    }

    async fn execute_non_stream(
        &self,
        request: &LlmRequest,
    ) -> Result<Completion, GeminiAdapterError> {
        let body = self.execute_wire_with_recovery(request).await?;
        let text = match parse_non_stream_response(&body, &self.schema) {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(%error, body_len = body.len(), "upstream response parsing failed");
                return Err(error);
            }
        };
        let metadata = parse_non_stream_metadata(&body);
        Ok(Completion {
            text,
            finish_reason: "stop".to_owned(),
            usage: None,
            metadata: metadata.map(|m| gemini_bridge_llm_service::ProviderMetadata {
                raw: serde_json::json!({
                    "conversation_id": m.conversation_id,
                    "response_id": m.response_id,
                    "candidate_id": m.candidate_id,
                    "code_execution": m.code_execution,
                    "citations": m.citations,
                }),
            }),
        })
    }
}

#[async_trait]
impl GeminiAdapter for DefaultGeminiAdapter {
    async fn generate_non_stream(
        &self,
        req: NormalizedLlmRequest,
    ) -> Result<NormalizedLlmResponse, GeminiAdapterError> {
        self.execute_non_stream(&req).await
    }

    async fn generate_stream(
        &self,
        req: NormalizedLlmRequest,
    ) -> Result<ChunkStream, GeminiAdapterError> {
        use futures::StreamExt;
        use gemini_bridge_llm_service::LlmEvent;

        let llm_stream = self.stream_events(&req).await?;
        let chunk_stream: ChunkStream = Box::pin(llm_stream.filter_map(|event| async move {
            match event {
                Ok(LlmEvent::TextDelta(text)) => Some(Ok(StreamChunk {
                    delta_text: Some(text),
                    is_finished: false,
                    finish_reason: None,
                    metadata: None,
                })),
                Ok(LlmEvent::Completed(summary)) => {
                    let metadata = summary
                        .metadata
                        .and_then(|metadata| serde_json::from_value(metadata.raw).ok());
                    Some(Ok(StreamChunk {
                        delta_text: None,
                        is_finished: true,
                        finish_reason: Some(summary.finish_reason),
                        metadata,
                    }))
                }
                Ok(_) => None,
                Err(error) => Some(Err(match error {
                    LlmError::Protocol(message) => GeminiAdapterError::SchemaMismatch(message),
                    other => GeminiAdapterError::Transport(other.to_string()),
                })),
            }
        }));
        Ok(chunk_stream)
    }
}

#[async_trait]
impl LlmAdapter for DefaultGeminiAdapter {
    fn provider_id(&self) -> &'static str {
        "gemini-web"
    }

    async fn complete(&self, request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        self.generate_non_stream(request)
            .await
            .map_err(map_llm_error)
    }

    async fn complete_raw(&self, request: Arc<LlmRequest>) -> Result<Value, LlmError> {
        let body = self
            .execute_wire_with_recovery(&request)
            .await
            .map_err(map_llm_error)?;
        parse_raw_response(&body).map_err(map_llm_error)
    }

    async fn stream(&self, request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        self.stream_events(&request).await.map_err(map_llm_error)
    }
}

fn normalized_prompt(request: &LlmRequest) -> Result<String, GeminiAdapterError> {
    let mut lines = Vec::new();
    for message in request.messages.iter() {
        for part in message.parts.iter() {
            match part {
                ContentPart::Text(text)
                    if message.role == Role::User || message.role == Role::Tool =>
                {
                    lines.push(text.as_str());
                }
                ContentPart::ToolResult(result)
                    if message
                        .parts
                        .iter()
                        .all(|p| !matches!(p, ContentPart::Text(_))) =>
                {
                    lines.push(result.content.as_str());
                }
                _ => {}
            }
        }
    }
    if lines.is_empty() {
        return Err(GeminiAdapterError::SchemaMismatch(
            "request contains no user text".to_owned(),
        ));
    }
    Ok(lines.join("\n"))
}

fn metadata_string<'a>(
    request: &'a LlmRequest,
    key: &str,
) -> Result<Option<&'a str>, GeminiAdapterError> {
    match request.metadata.get(key) {
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(GeminiAdapterError::SchemaMismatch(format!(
            "metadata field {key} must be a string"
        ))),
        None => Ok(None),
    }
}

/// Parse Gemini newline-framed output into a JSON array while preserving the
/// complete provider response tree. Numeric framing lines and the XSSI prefix
/// are excluded.
pub fn parse_raw_response(body: &[u8]) -> Result<Value, GeminiAdapterError> {
    let response = std::str::from_utf8(body).map_err(|error| {
        GeminiAdapterError::SchemaMismatch(format!("response is not UTF-8: {error}"))
    })?;
    let mut frames = Vec::new();
    for line in response.lines().map(str::trim) {
        if line.is_empty() || line == ")]}'" || line.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            frames.push(value);
        }
    }
    if frames.is_empty() {
        return Err(GeminiAdapterError::SchemaMismatch(
            "response contained no JSON frames".to_owned(),
        ));
    }
    Ok(Value::Array(frames))
}

/// Parse newline-framed Gemini Web output and return the latest cumulative text.
pub fn parse_non_stream_response(
    body: &[u8],
    schema: &GeminiWebSchema,
) -> Result<String, GeminiAdapterError> {
    let response = std::str::from_utf8(body).map_err(|error| {
        GeminiAdapterError::SchemaMismatch(format!("response is not UTF-8: {error}"))
    })?;
    let mut latest = None;
    let mut parsed_frame = false;

    for line in response.lines().map(str::trim) {
        if line.is_empty() || line == ")]}'" || line.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        parsed_frame = true;
        find_candidate_text(&value, schema, &mut latest);
    }

    latest.ok_or_else(|| {
        let detail = if parsed_frame {
            "no candidate text matched the configured response path"
        } else {
            "response contained no JSON frames"
        };
        GeminiAdapterError::SchemaMismatch(detail.to_owned())
    })
}

pub(crate) fn find_candidate_text(
    value: &Value,
    schema: &GeminiWebSchema,
    latest: &mut Option<String>,
) {
    if let Ok(text) = schema.extract_candidate_text(value) {
        *latest = Some(text.to_owned());
    }

    match value {
        Value::Array(values) => {
            for child in values {
                find_candidate_text(child, schema, latest);
            }
        }
        Value::Object(values) => {
            for child in values.values() {
                find_candidate_text(child, schema, latest);
            }
        }
        Value::String(encoded) => {
            if let Ok(child) = serde_json::from_str::<Value>(encoded) {
                find_candidate_text(&child, schema, latest);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
/// Parse Gemini response frames and extract conversation/response/candidate identifiers.
pub fn parse_non_stream_metadata(body: &[u8]) -> Option<GeminiWebMetadata> {
    let response = std::str::from_utf8(body).ok()?;
    let mut meta = GeminiWebMetadata::default();
    let mut found_any = false;

    for line in response.lines().map(str::trim) {
        if line.is_empty() || line == ")]}'" || line.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if extract_metadata_recursive(&value, &mut meta) {
            found_any = true;
        }
    }

    if found_any
        || meta.conversation_id.is_some()
        || meta.response_id.is_some()
        || meta.candidate_id.is_some()
        || !meta.code_execution.is_empty()
        || !meta.citations.is_empty()
    {
        Some(meta)
    } else {
        None
    }
}

fn extract_metadata_recursive(value: &Value, meta: &mut GeminiWebMetadata) -> bool {
    let mut found = false;

    if let Value::Object(map) = value {
        if let Some(Value::String(cid)) = map
            .get("conversation_id")
            .or_else(|| map.get("conversationId"))
        {
            meta.conversation_id = Some(cid.clone());
            found = true;
        }
        if let Some(Value::String(rid)) = map.get("response_id").or_else(|| map.get("responseId")) {
            meta.response_id = Some(rid.clone());
            found = true;
        }
        if let Some(Value::String(cand)) =
            map.get("candidate_id").or_else(|| map.get("candidateId"))
        {
            meta.candidate_id = Some(cand.clone());
            found = true;
        }
        if let Some(Value::Array(candidates)) = map.get("candidates")
            && let Some(first_cand) = candidates.first()
            && let Some(Value::String(cand)) = first_cand
                .get("candidateId")
                .or_else(|| first_cand.get("candidate_id"))
        {
            meta.candidate_id = Some(cand.clone());
            found = true;
        }

        if let (Some(Value::String(lang)), Some(Value::String(code))) =
            (map.get("language"), map.get("code"))
        {
            let output = map
                .get("output")
                .or_else(|| map.get("stdout"))
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let block = CodeExecutionBlock {
                language: lang.clone(),
                code: code.clone(),
                output,
            };
            if !meta.code_execution.contains(&block) {
                meta.code_execution.push(block);
                found = true;
            }
        }

        if let Some(Value::String(uri)) = map.get("uri").or_else(|| map.get("url")) {
            let start = map
                .get("start_index")
                .or_else(|| map.get("startIndex"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let end = map
                .get("end_index")
                .or_else(|| map.get("endIndex"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let title = map.get("title").and_then(|v| v.as_str()).map(str::to_owned);
            let citation = CitationRef {
                start_index: start,
                end_index: end,
                uri: uri.clone(),
                title,
            };
            if !meta.citations.contains(&citation) {
                meta.citations.push(citation);
                found = true;
            }
        }
    }
    match value {
        Value::Array(items) => {
            for item in items {
                if extract_metadata_recursive(item, meta) {
                    found = true;
                }
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                if extract_metadata_recursive(v, meta) {
                    found = true;
                }
            }
        }
        Value::String(encoded) => {
            if let Ok(child) = serde_json::from_str::<Value>(encoded)
                && extract_metadata_recursive(&child, meta)
            {
                found = true;
            }
        }
        _ => {}
    }

    found
}

fn map_identity_error(error: IdentityError) -> GeminiAdapterError {
    match error {
        IdentityError::MissingCredentials | IdentityError::NeedsReauth => {
            GeminiAdapterError::NeedsAuth
        }
        IdentityError::IpFlagged => GeminiAdapterError::IpFlagged,
        other => GeminiAdapterError::Transport(other.to_string()),
    }
}

fn map_llm_error(error: GeminiAdapterError) -> LlmError {
    match error {
        GeminiAdapterError::NeedsAuth => LlmError::Authentication,
        GeminiAdapterError::RateLimited => LlmError::RateLimited,
        GeminiAdapterError::StaleBuildLabel
        | GeminiAdapterError::IpFlagged
        | GeminiAdapterError::Transport(_) => LlmError::Unavailable,
        GeminiAdapterError::SchemaMismatch(message) => LlmError::Protocol(message),
    }
}
