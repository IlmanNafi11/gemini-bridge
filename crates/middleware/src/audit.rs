use serde::Serialize;

/// An audit record safe to write to any sink.
///
/// Deliberately excludes: headers, query strings, request/response bodies,
/// cookie values, and IP addresses.
#[derive(Debug, Clone, Serialize)]
pub struct AuditLogEntry {
    pub request_id: String,
    pub timestamp: i64,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: u64,
    pub model: Option<String>,
}

/// An object-safe sink for audit log entries.
pub trait AuditSink: Send + Sync {
    fn record(&self, entry: AuditLogEntry);
}
