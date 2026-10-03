use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("Request timed out after {0:?}")]
    Timeout(Duration),
    #[error("TLS handshake failed")]
    TlsFailure,
    #[error("Proxy connection failed")]
    ProxyFailure,
    #[error("Unsupported TLS profile: {0}")]
    UnsupportedTlsProfile(String),
    #[error("Network error: {0}")]
    Network(String),
    #[error("Response body exceeds the {limit}-byte buffered response limit")]
    ResponseTooLarge { limit: usize },
}
