mod audit;
mod rate_limit;
mod redact;

pub use audit::{AuditLogEntry, AuditSink};
pub use rate_limit::{AdmissionDecision, TokenBucketConfig, TokenBucketLimiter};
pub use redact::RedactionFilter;

use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Immutable metadata derived from an incoming HTTP request.
///
/// This projection intentionally contains no headers or request/response body.
#[derive(Debug, Clone)]
pub struct RequestMetadata {
    pub request_id: Arc<str>,
    pub method: Arc<str>,
    pub path: Arc<str>,
    pub client_id: Arc<str>,
    pub started_at: SystemTime,
    pub model: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct BeforeRequest {
    pub metadata: RequestMetadata,
}

#[derive(Debug, Clone)]
pub struct AfterResponse {
    pub metadata: RequestMetadata,
    pub status: u16,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct BeforeLog {
    pub request_id: Option<Arc<str>>,
    pub level: tracing::Level,
    pub fields: serde_json::Value,
}
