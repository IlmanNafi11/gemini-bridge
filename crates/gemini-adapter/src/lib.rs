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
use gemini_bridge_transport::{Idempotency, ReqwestTransport, TransportRequest, TransportService};
use http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use serde_json::Value;
use thiserror::Error;
use url::Url;

use schema::GeminiWebSchema;

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

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GeminiWebMetadata {
    pub conversation_id: Option<String>,
    pub response_id: Option<String>,
    pub candidate_id: Option<String>,
    pub code_execution: Vec<CodeExecutionBlock>,
    pub citations: Vec<CitationRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeExecutionBlock {
    pub language: String,
    pub code: String,
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
        Self::with_base_url_and_schema(
            identity,
            config,
            GEMINI_BASE_URL,
            GeminiWebSchema::default(),
        )
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

    /// Execute the StreamGenerate upstream request and return the raw response
    /// body bytes. Both streaming and non-streaming paths share this method.
    async fn execute_wire_request(
        &self,
        request: &LlmRequest,
    ) -> Result<Bytes, GeminiAdapterError> {
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
        let form_body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("f.req", &envelope_json)
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

        let response = self
            .transport
            .execute(TransportRequest {
                method: Method::POST,
                url,
                headers,
                body: Some(Bytes::from(form_body)),
                idempotency: Idempotency::NeverRetry,
            })
            .await
            .map_err(|error| GeminiAdapterError::Transport(error.to_string()))?;

        match response.status {
            StatusCode::METHOD_NOT_ALLOWED => return Err(GeminiAdapterError::StaleBuildLabel),
            StatusCode::TOO_MANY_REQUESTS => return Err(GeminiAdapterError::RateLimited),
            StatusCode::UNAUTHORIZED => return Err(GeminiAdapterError::NeedsAuth),
            StatusCode::FOUND | StatusCode::MOVED_PERMANENTLY => {
                if response
                    .headers
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|location| location.contains("sorry"))
                {
                    return Err(GeminiAdapterError::IpFlagged);
                }
            }
            status if !status.is_success() => {
                return Err(GeminiAdapterError::Transport(format!(
                    "upstream returned HTTP {status}"
                )));
            }
            _ => {}
        }

        Ok(response.body)
    }

    /// Execute the wire request with one 405 auto-recovery attempt.
    ///
    /// Implements the bounded retry contract from SPEC-health-admin §3.3:
    ///
    /// 1. Initial attempt.
    /// 2. On `StaleBuildLabel` (405): refresh bootstrap once via `identity`, retry exactly once.
    /// 3. If the retry also fails with 405: return a `SchemaMismatch` (maps to 502 Bad Gateway).
    /// 4. Any other error propagates immediately — 429/IpFlagged never enter this path.
    async fn execute_wire_with_recovery(
        &self,
        request: &LlmRequest,
    ) -> Result<Bytes, GeminiAdapterError> {
        match self.execute_wire_request(request).await {
            Ok(body) => Ok(body),
            Err(GeminiAdapterError::StaleBuildLabel) => {
                // `execute_wire_request` bootstraps before every upstream call,
                // so invoking it once more performs exactly one refresh and one retry.
                match self.execute_wire_request(request).await {
                    Err(GeminiAdapterError::StaleBuildLabel) => {
                        Err(GeminiAdapterError::SchemaMismatch(
                            "upstream returned 405 after build-label refresh".to_owned(),
                        ))
                    }
                    other => other,
                }
            }
            other => other,
        }
    }

    async fn execute_non_stream(
        &self,
        request: &LlmRequest,
    ) -> Result<Completion, GeminiAdapterError> {
        let body = self.execute_wire_with_recovery(request).await?;
        let text = parse_non_stream_response(&body, &self.schema)?;
        Ok(Completion {
            text,
            finish_reason: "stop".to_owned(),
            usage: None,
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
        // Reuse the same upstream wire call; the full buffered body is parsed
        // frame-by-frame through the prefix-diff engine.
        let response_bytes = self.execute_wire_with_recovery(&req).await?;
        // ChunkStream wraps a StreamChunk type distinct from LlmEvent.
        // We delegate to the shared parse_stream_body and translate events.
        use crate::stream::parse_stream_body;
        use futures::StreamExt;
        use gemini_bridge_llm_service::LlmEvent;
        let llm_stream = parse_stream_body(&response_bytes, &self.schema)?;
        let chunk_stream: ChunkStream = Box::pin(llm_stream.filter_map(|ev| async move {
            match ev {
                Ok(LlmEvent::TextDelta(text)) => Some(Ok(StreamChunk {
                    delta_text: Some(text),
                    is_finished: false,
                    finish_reason: None,
                    metadata: None,
                })),
                Ok(LlmEvent::Completed(summary)) => Some(Ok(StreamChunk {
                    delta_text: None,
                    is_finished: true,
                    finish_reason: Some(summary.finish_reason),
                    metadata: None,
                })),
                Ok(_) => None,
                Err(e) => Some(Err(GeminiAdapterError::Transport(e.to_string()))),
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
        use crate::stream::parse_stream_body;
        let response_bytes = self
            .execute_wire_with_recovery(&request)
            .await
            .map_err(map_llm_error)?;
        parse_stream_body(&response_bytes, &self.schema).map_err(map_llm_error)
    }
}

fn normalized_prompt(request: &LlmRequest) -> Result<String, GeminiAdapterError> {
    let mut lines = Vec::new();
    for message in request.messages.iter() {
        for part in message.parts.iter() {
            if let ContentPart::Text(text) = part
                && message.role == Role::User
            {
                lines.push(text.as_str());
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
