use async_trait::async_trait;
use bytes::Bytes;
use gemini_bridge_identity::IdentityService;
use gemini_bridge_transport::{Idempotency, TransportRequest, TransportService};
use http::{HeaderMap, HeaderValue, Method};
use serde_json::Value;
use std::sync::Arc;
use url::Url;

use crate::UploadError;

/// Two-step Google resumable push upload interface.
#[async_trait]
pub trait PushUploadClient: Send + Sync {
    async fn initiate_session(
        &self,
        mime_type: &str,
        size_bytes: u64,
    ) -> Result<String, UploadError>;

    /// Upload is non-idempotent and is never retried automatically.
    async fn upload_bytes(&self, session_url: &str, bytes: Bytes) -> Result<String, UploadError>;
}

/// Google resumable upload implementation using the shared outbound transport.
pub struct HttpPushUploadClient {
    transport: Arc<dyn TransportService>,
    identity: Arc<dyn IdentityService>,
    endpoint: Url,
}

impl HttpPushUploadClient {
    pub fn new(
        identity: Arc<dyn IdentityService>,
        transport: Arc<dyn TransportService>,
    ) -> Result<Self, UploadError> {
        Self::with_endpoint(
            identity,
            transport,
            "https://content-push.googleapis.com/upload/",
        )
    }

    /// Construct with a custom initiation endpoint for deterministic tests.
    pub fn with_endpoint(
        identity: Arc<dyn IdentityService>,
        transport: Arc<dyn TransportService>,
        endpoint: impl AsRef<str>,
    ) -> Result<Self, UploadError> {
        let endpoint = Url::parse(endpoint.as_ref())
            .map_err(|_| UploadError::InvalidUrl("invalid push upload endpoint".into()))?;
        Ok(Self {
            transport,
            identity,
            endpoint,
        })
    }

    fn auth_headers(&self, phase: UploadPhase) -> Result<HeaderMap, UploadError> {
        let mut headers = HeaderMap::new();
        self.identity
            .apply_auth_headers(&mut headers)
            .map_err(|_| phase.auth_error())?;
        Ok(headers)
    }
}

#[derive(Clone, Copy)]
enum UploadPhase {
    Init,
    Upload,
}

impl UploadPhase {
    fn auth_error(self) -> UploadError {
        match self {
            Self::Init => {
                UploadError::PushInitFailed("session authentication is unavailable".into())
            }
            Self::Upload => {
                UploadError::PushUploadFailed("session authentication is unavailable".into())
            }
        }
    }
}

#[async_trait]
impl PushUploadClient for HttpPushUploadClient {
    async fn initiate_session(
        &self,
        mime_type: &str,
        size_bytes: u64,
    ) -> Result<String, UploadError> {
        let mut headers = self.auth_headers(UploadPhase::Init)?;
        headers.insert("X-Goog-Upload-Command", HeaderValue::from_static("start"));
        headers.insert(
            "X-Goog-Upload-Protocol",
            HeaderValue::from_static("resumable"),
        );
        headers.insert(
            "X-Goog-Upload-Header-Content-Length",
            HeaderValue::from_str(&size_bytes.to_string())
                .map_err(|_| UploadError::PushInitFailed("invalid content length".into()))?,
        );
        headers.insert(
            "X-Goog-Upload-Header-Content-Type",
            HeaderValue::from_str(mime_type)
                .map_err(|_| UploadError::PushInitFailed("invalid content type".into()))?,
        );
        let response = self
            .transport
            .execute(TransportRequest {
                method: Method::POST,
                url: self.endpoint.clone(),
                headers,
                body: None,
                idempotency: Idempotency::SafeToRetry,
            })
            .await
            .map_err(|_| UploadError::PushInitFailed("transport request failed".into()))?;
        if !response.status.is_success() {
            return Err(UploadError::PushInitFailed(format!(
                "upstream returned {}",
                response.status
            )));
        }
        response
            .headers
            .get("X-Goog-Upload-URL")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                UploadError::PushInitFailed("upstream omitted upload session URL".into())
            })
    }

    async fn upload_bytes(&self, session_url: &str, bytes: Bytes) -> Result<String, UploadError> {
        let url = Url::parse(session_url)
            .map_err(|_| UploadError::PushUploadFailed("invalid upload session URL".into()))?;
        let mut headers = self.auth_headers(UploadPhase::Upload)?;
        headers.insert(
            "X-Goog-Upload-Command",
            HeaderValue::from_static("upload, finalize"),
        );
        headers.insert(
            http::header::CONTENT_LENGTH,
            HeaderValue::from_str(&bytes.len().to_string())
                .map_err(|_| UploadError::PushUploadFailed("invalid content length".into()))?,
        );
        let response = self
            .transport
            .execute(TransportRequest {
                method: Method::POST,
                url,
                headers,
                body: Some(bytes),
                idempotency: Idempotency::NeverRetry,
            })
            .await
            .map_err(|_| UploadError::PushUploadFailed("upload request failed".into()))?;
        if !response.status.is_success() {
            return Err(UploadError::PushUploadFailed(format!(
                "upstream returned {}",
                response.status
            )));
        }
        let payload: Value = serde_json::from_slice(&response.body).map_err(|_| {
            UploadError::PushUploadFailed("upstream returned invalid response".into())
        })?;
        find_file_ref(&payload).ok_or_else(|| {
            UploadError::PushUploadFailed("upstream response omitted fileRef".into())
        })
    }
}

fn find_file_ref(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(file_ref)) = map.get("fileRef")
                && !file_ref.is_empty()
            {
                return Some(file_ref.clone());
            }
            map.values().find_map(find_file_ref)
        }
        Value::Array(items) => items.iter().find_map(find_file_ref),
        _ => None,
    }
}
