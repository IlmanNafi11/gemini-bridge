use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use gemini_bridge_config::TransportConfig;
use http::HeaderMap;
use reqwest::Client;
use reqwest::header::HeaderValue;

use crate::error::TransportError;
use crate::model::{
    Idempotency, TransportByteStream, TransportRequest, TransportResponse, TransportStreamResponse,
};
use crate::tls::TlsProfile;

/// Maximum number of attempts (initial + retries) for `SafeToRetry` requests.
const MAX_ATTEMPTS: u32 = 3;

/// Maximum size of a response collected by `execute` (64 MiB).
pub const MAX_BUFFERED_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Concrete HTTP transport backed by `reqwest`.
///
/// Constructed via [`ReqwestTransport::new`] from a [`TransportConfig`].
pub struct ReqwestTransport {
    client: Client,
    tls_profile: TlsProfile,
    timeout: Duration,
}

impl ReqwestTransport {
    /// Build a new transport from configuration.
    ///
    /// Returns [`TransportError::ProxyFailure`] if the configured proxy URL is invalid,
    /// [`TransportError::UnsupportedTlsProfile`] for an unknown profile, and
    /// [`TransportError::Network`] for any other client-build failure.
    pub fn new(config: &TransportConfig) -> Result<Self, TransportError> {
        let timeout = Duration::from_secs(config.timeout_secs);
        let profile_name = config.tls_profile.trim().to_ascii_lowercase();
        if !matches!(profile_name.as_str(), "chrome" | "firefox" | "safari") {
            return Err(TransportError::UnsupportedTlsProfile(
                config.tls_profile.clone(),
            ));
        }
        let tls_profile = profile_name.parse::<TlsProfile>().unwrap_or_default();

        let mut builder = Client::builder().timeout(timeout);

        if let Some(proxy_url) = &config.proxy_url {
            let proxy = reqwest::Proxy::all(proxy_url.as_str())
                .map_err(|_| TransportError::ProxyFailure)?;
            builder = builder.proxy(proxy);
        }

        let client = builder
            .build()
            .map_err(|e| TransportError::Network(e.to_string()))?;

        Ok(Self {
            client,
            tls_profile,
            timeout,
        })
    }
}

#[async_trait]
impl TransportService for ReqwestTransport {
    async fn execute(
        &self,
        request: TransportRequest,
    ) -> Result<TransportResponse, TransportError> {
        let response = self.execute_response(request).await?;
        let status = response.status();
        let headers = convert_headers(response.headers());
        if response
            .content_length()
            .is_some_and(|length| length > MAX_BUFFERED_RESPONSE_BYTES as u64)
        {
            return Err(TransportError::ResponseTooLarge {
                limit: MAX_BUFFERED_RESPONSE_BYTES,
            });
        }

        let mut body = bytes::BytesMut::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|error| classify_error(error, self.timeout))?;
            if chunk.len() > MAX_BUFFERED_RESPONSE_BYTES - body.len() {
                return Err(TransportError::ResponseTooLarge {
                    limit: MAX_BUFFERED_RESPONSE_BYTES,
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(TransportResponse {
            status,
            headers,
            body: body.freeze(),
        })
    }

    async fn execute_stream(
        &self,
        request: TransportRequest,
    ) -> Result<TransportStreamResponse, TransportError> {
        let response = self.execute_response(request).await?;
        let status = response.status();
        let headers = convert_headers(response.headers());
        let timeout = self.timeout;
        let body: TransportByteStream = Box::pin(
            response
                .bytes_stream()
                .map(move |chunk| chunk.map_err(|error| classify_error(error, timeout))),
        );
        Ok(TransportStreamResponse {
            status,
            headers,
            body,
        })
    }
}

impl ReqwestTransport {
    async fn execute_response(
        &self,
        request: TransportRequest,
    ) -> Result<reqwest::Response, TransportError> {
        let max_attempts = match request.idempotency {
            Idempotency::SafeToRetry => MAX_ATTEMPTS,
            Idempotency::NeverRetry => 1,
        };
        let mut last_err = None;

        for attempt in 0..max_attempts {
            let req = build_reqwest_request(&self.client, &request, &self.tls_profile)?;
            match self.client.execute(req).await {
                Ok(response) => return Ok(response),
                Err(error) => {
                    let classified = classify_error(error, self.timeout);
                    let is_transient = matches!(
                        &classified,
                        TransportError::Timeout(_) | TransportError::Network(_)
                    );
                    if request.idempotency == Idempotency::SafeToRetry
                        && is_transient
                        && attempt + 1 < max_attempts
                    {
                        last_err = Some(classified);
                    } else {
                        return Err(classified);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| TransportError::Network("no attempts made".to_owned())))
    }
}

/// The transport service trait.
///
/// Separated here so that [`ReqwestTransport`] and any test doubles implement it.
#[async_trait]
pub trait TransportService: Send + Sync {
    async fn execute(&self, request: TransportRequest)
    -> Result<TransportResponse, TransportError>;

    /// Execute a request and return the response with its body still open as an
    /// incremental byte stream. Dropping the body stream cancels the upstream read.
    async fn execute_stream(
        &self,
        request: TransportRequest,
    ) -> Result<TransportStreamResponse, TransportError>;
}
// ── helpers ──────────────────────────────────────────────────────────────────

/// Build a `reqwest::Request` from a `TransportRequest`, applying HTTP header presets.
fn build_reqwest_request(
    client: &Client,
    req: &TransportRequest,
    profile: &TlsProfile,
) -> Result<reqwest::Request, TransportError> {
    let method = req.method.clone();
    let url = req.url.as_str().to_string();

    let mut builder = client.request(method, &url);

    // Start with caller headers, then inject profile defaults for missing keys.
    let mut combined = req.headers.clone();
    profile.apply_headers(&mut combined);

    // reqwest accepts &HeaderMap.
    builder = builder.headers(combined);

    if let Some(body) = &req.body {
        builder = builder.body(body.clone());
    }

    builder
        .build()
        .map_err(|e| TransportError::Network(e.to_string()))
}

/// Convert `reqwest::Response` headers to `http::HeaderMap`.
///
/// `reqwest` uses the same underlying `http` crate types, so this is a simple
/// clone through the `HeaderMap` API.
fn convert_headers(resp_headers: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(resp_headers.len());
    for (k, v) in resp_headers.iter() {
        // `reqwest::header::HeaderName` and `http::header::HeaderName` are the
        // same type (re-exported from the `http` crate), so transmuting via the
        // str representation is safe and avoids unsafe code.
        if let Ok(name) = http::HeaderName::from_bytes(k.as_ref()) {
            out.insert(
                name,
                HeaderValue::from_bytes(v.as_bytes())
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
        }
    }
    out
}

/// Map a `reqwest::Error` to our `TransportError`.
fn classify_error(e: reqwest::Error, timeout: Duration) -> TransportError {
    if e.is_timeout() {
        TransportError::Timeout(timeout)
    } else if e.is_connect() {
        // Proxy-related connect errors carry "proxy" in the message; otherwise TLS/TCP.
        let msg = e.to_string().to_lowercase();
        if msg.contains("proxy") {
            TransportError::ProxyFailure
        } else if msg.contains("tls") || msg.contains("ssl") || msg.contains("certificate") {
            TransportError::TlsFailure
        } else {
            TransportError::Network(e.to_string())
        }
    } else {
        TransportError::Network(e.to_string())
    }
}
