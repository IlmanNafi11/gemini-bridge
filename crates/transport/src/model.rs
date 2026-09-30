use bytes::Bytes;
use http::{HeaderMap, HeaderName};
use reqwest::{Method, StatusCode};
use std::fmt;
use url::Url;

/// Indicates whether a request may be retried on transient failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idempotency {
    /// The request may be repeated automatically on transient network errors.
    SafeToRetry,
    /// The request must never be repeated automatically (e.g. non-idempotent mutations).
    NeverRetry,
}

/// A transport-layer request.
///
/// `Debug` output redacts sensitive headers (Cookie, Authorization, and any
/// upload/API token headers) to prevent accidental credential leakage in logs.
pub struct TransportRequest {
    pub method: Method,
    pub url: Url,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    pub idempotency: Idempotency,
}

/// Header names that carry secrets and must be redacted from debug output.
const SECRET_HEADERS: &[&str] = &[
    "cookie",
    "authorization",
    "x-goog-api-key",
    "x-goog-upload-header-content-disposition",
    "x-goog-upload-header-content-type",
    "x-goog-upload-protocol",
];

fn is_secret_header(name: &HeaderName) -> bool {
    let lower = name.as_str();
    SECRET_HEADERS.contains(&lower)
}

impl fmt::Debug for TransportRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(k, _v)| {
                if is_secret_header(k) {
                    (k.as_str(), "<redacted>")
                } else {
                    (k.as_str(), "<present>")
                }
            })
            .collect();

        f.debug_struct("TransportRequest")
            .field("method", &self.method)
            .field("url", &self.url.as_str())
            .field("headers", &redacted)
            .field("body_len", &self.body.as_ref().map(|b| b.len()))
            .field("idempotency", &self.idempotency)
            .finish()
    }
}

/// A transport-layer response.
pub struct TransportResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl fmt::Debug for TransportResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redact secret headers in responses too (e.g. Set-Cookie).
        let redacted: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(k, _v)| {
                let name = k.as_str();
                if name == "set-cookie" || is_secret_header(k) {
                    (name, "<redacted>")
                } else {
                    (name, "<present>")
                }
            })
            .collect();

        f.debug_struct("TransportResponse")
            .field("status", &self.status)
            .field("headers", &redacted)
            .field("body_len", &self.body.len())
            .finish()
    }
}
