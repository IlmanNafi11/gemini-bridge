use thiserror::Error;

#[derive(Debug, Error)]
pub enum UploadError {
    #[error("File exceeds configured limit of {limit} bytes (received {received} bytes)")]
    TooLarge { limit: u64, received: u64 },

    #[error("Unsupported MIME type detected: {0}")]
    UnsupportedType(String),

    #[error("Reference URL violates network policy: {0}")]
    SsrfDenied(String),

    #[error("DNS resolution failed for {0}")]
    DnsResolutionFailed(String),

    #[error("URL is invalid or unparseable: {0}")]
    InvalidUrl(String),

    #[error("Push upload initiation failed: {0}")]
    PushInitFailed(String),

    #[error("Push upload completion failed: {0}")]
    PushUploadFailed(String),

    #[error("Media store error: {0}")]
    StoreError(String),

    #[error("File not found: {0}")]
    NotFound(String),
}

impl From<gemini_bridge_media_store::StoreError> for UploadError {
    fn from(e: gemini_bridge_media_store::StoreError) -> Self {
        UploadError::StoreError(e.to_string())
    }
}
