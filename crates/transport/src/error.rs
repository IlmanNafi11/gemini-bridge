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
    #[error("Network error: {0}")]
    Network(String),
}
