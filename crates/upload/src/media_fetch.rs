//! Hardened outbound media fetcher shared by image and video retrieval.
//!
//! Fetch policy (R.1):
//! - HTTPS only; exact host allowlist (suffix-safe, dot-delimited).
//! - No credentials in the URL.
//! - All DNS answers resolved and checked against blocked/reserved ranges;
//!   the connection is pinned to a validated address (DNS-rebinding defense).
//! - Redirects are followed manually; every hop is re-validated and re-pinned.
//! - Connect/total timeouts, a redirect cap, a `Content-Length` pre-check, and
//!   a streamed-body byte cap defend against slowloris and decompression bombs.
//! - The body's magic bytes must match the expected media kind
//!   ([`MediaKind::Image`] / [`MediaKind::Video`]).

use std::{net::IpAddr, sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use url::Url;

use crate::error::UploadError;
use crate::ssrf::resolve_and_check;

/// DNS resolver used by the hardened fetch path. Production uses the system
/// resolver. This hidden public seam exists only so integration tests can
/// supply deterministic answers without weakening production policy.
#[doc(hidden)]
#[async_trait]
pub trait MediaHostResolver: Send + Sync {
    async fn resolve_and_check(&self, host: &str) -> Result<IpAddr, UploadError>;
}

#[derive(Debug, Default)]
struct SystemMediaHostResolver;

#[async_trait]
impl MediaHostResolver for SystemMediaHostResolver {
    async fn resolve_and_check(&self, host: &str) -> Result<IpAddr, UploadError> {
        resolve_and_check(host).await
    }
}

/// Hard cap on redirect hops per fetch.
const MAX_REDIRECTS: usize = 5;

/// Kind of media a fetch must produce; drives the magic-byte check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// PNG, JPEG, GIF, or WebP.
    Image,
    /// MP4 (ftyp/isom box) or WebM (EBML).
    Video,
}

impl MediaKind {
    /// Human-readable label for error messages.
    fn label(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
        }
    }

    /// Recognized file signatures for this media kind.
    fn signature(self, body: &Bytes) -> bool {
        match self {
            MediaKind::Image => crate::detect_mime(body).is_some(),
            MediaKind::Video => is_video_signature(body),
        }
    }

    /// Detect the MIME type from magic bytes, if recognized.
    pub fn detect_mime(self, body: &Bytes) -> Option<&'static str> {
        match self {
            MediaKind::Image => crate::detect_mime(body),
            MediaKind::Video if body.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) => Some("video/webm"),
            MediaKind::Video if is_video_signature(body) => Some("video/mp4"),
            MediaKind::Video => None,
        }
    }
}
/// Recognize a video container signature.
///
/// - MP4: `ftyp` box at the start (size + `ftyp` + brand).
/// - WebM/MKV: EBML magic `0x1A45DFA3`.
fn is_video_signature(body: &[u8]) -> bool {
    if body.len() >= 12 && &body[4..8] == b"ftyp" {
        return true;
    }
    body.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
}

/// Network policy for a media fetch.
///
/// The production constructor [`MediaFetchPolicy::provider_cdn`] enforces HTTPS
/// plus an exact allowlisted host boundary. A separate test-only constructor
/// relaxes scheme/address checks for deterministic local fixtures; it must
/// never be used in production wiring.
#[derive(Debug, Clone)]
pub struct MediaFetchPolicy {
    /// Allowed hosts (exact, or dot-delimited subdomains of these suffixes).
    pub allowed_hosts: Vec<String>,
    /// Resolve all DNS answers, reject blocked addresses, and pin one.
    pub strict_network: bool,
    /// Require HTTPS at every hop.
    pub require_https: bool,
    /// Upper bound on the response body.
    pub max_bytes: u64,
    /// Total deadline for the whole fetch including redirects and body.
    pub total_timeout: Duration,
    /// TCP connect deadline per hop.
    pub connect_timeout: Duration,
}

impl MediaFetchPolicy {
    /// Production policy for provider-returned media URLs.
    pub fn provider_cdn() -> Self {
        Self {
            allowed_hosts: vec!["googleusercontent.com".to_owned()],
            strict_network: true,
            require_https: true,
            max_bytes: 20 * 1024 * 1024,
            total_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
        }
    }

    /// Production policy for provider-returned video URLs: HTTPS only and
    /// globally routable DNS, without a provider-specific hostname allowlist.
    pub fn provider_https() -> Self {
        Self {
            allowed_hosts: Vec::new(),
            strict_network: true,
            require_https: true,
            max_bytes: 100 * 1024 * 1024,
            total_timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(10),
        }
    }

    /// Existing caller-reference policy: accept HTTP and HTTPS while retaining
    /// DNS checks, pinning, redirect validation, deadlines, and byte limits.
    pub fn caller_reference(max_bytes: u64) -> Self {
        Self {
            allowed_hosts: Vec::new(),
            strict_network: true,
            require_https: false,
            max_bytes,
            total_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
        }
    }

    /// Relaxed policy for deterministic local tests only.
    ///
    /// Pass only through explicit test injection points; production service
    /// constructors always install [`HardenedMediaDownloader`].
    #[doc(hidden)]
    pub fn test_local() -> Self {
        Self {
            allowed_hosts: Vec::new(),
            strict_network: false,
            require_https: false,
            max_bytes: 20 * 1024 * 1024,
            total_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
        }
    }

    /// True when `host` is an allowed host or a dot-delimited subdomain of one.
    ///
    /// Matching is exact per DNS label: `evil-googleusercontent.com` and
    /// `googleusercontent.com.evil.test` never match.
    pub fn host_allowed(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.allowed_hosts.iter().any(|suffix| {
            let suffix = suffix.trim_end_matches('.').to_ascii_lowercase();
            host == suffix || host.strip_suffix(&format!(".{suffix}")).is_some()
        })
    }
}

/// Injectable interface for media URL downloads.
///
/// Production wiring uses [`HardenedMediaDownloader`]; service constructors
/// accepting this trait exist so tests can exercise their result paths without
/// weakening the shared fetch policy.
#[async_trait]
pub trait MediaDownloader: Send + Sync {
    /// Fetch a URL and validate it as the requested media kind.
    async fn download(&self, url: &Url, kind: MediaKind) -> Result<Bytes, UploadError>;
}

/// The production downloader: strict provider CDN policy for images and
/// public-HTTPS-only policy for video.
#[derive(Debug, Default, Clone, Copy)]
pub struct HardenedMediaDownloader;

#[async_trait]
impl MediaDownloader for HardenedMediaDownloader {
    async fn download(&self, url: &Url, kind: MediaKind) -> Result<Bytes, UploadError> {
        let policy = match kind {
            MediaKind::Image => MediaFetchPolicy::provider_cdn(),
            MediaKind::Video => MediaFetchPolicy::provider_https(),
        };
        fetch_media(url, kind, &policy).await
    }
}

/// Validate one URL hop against the fetch policy.
fn validate_hop(url: &Url, policy: &MediaFetchPolicy) -> Result<(), UploadError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UploadError::SsrfDenied(
            "URL user information is not permitted".into(),
        ));
    }
    if policy.require_https && url.scheme() != "https" {
        return Err(UploadError::SsrfDenied(
            "provider media URLs must use HTTPS".into(),
        ));
    }
    if policy.strict_network {
        crate::validate_reference_url(url)?;
    } else if !matches!(url.scheme(), "http" | "https") {
        return Err(UploadError::SsrfDenied(
            "only http and https URLs are permitted".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| UploadError::SsrfDenied("URL has no host".into()))?;
    if policy.strict_network && !policy.allowed_hosts.is_empty() && !policy.host_allowed(host) {
        return Err(UploadError::SsrfDenied(format!(
            "host {host} is not on the media fetch allowlist"
        )));
    }
    Ok(())
}

fn validate_redirect(
    base: &Url,
    location: &str,
    policy: &MediaFetchPolicy,
) -> Result<Url, UploadError> {
    let target = base
        .join(location)
        .map_err(|_| UploadError::SsrfDenied("invalid redirect target".into()))?;
    validate_hop(&target, policy)?;
    Ok(target)
}

/// Validate a single URL against a media fetch policy without network I/O.
pub fn validate_media_url(url: &Url, policy: &MediaFetchPolicy) -> Result<(), UploadError> {
    validate_hop(url, policy)
}

/// Resolve a redirect target relative to `base` and validate it against the
/// same media fetch policy without network I/O.
pub fn validate_media_redirect(
    base: &Url,
    location: &str,
    policy: &MediaFetchPolicy,
) -> Result<Url, UploadError> {
    validate_redirect(base, location, policy)
}

/// Fetch `url` under `policy` and return the validated media bytes.
pub async fn fetch_media(
    url: &Url,
    kind: MediaKind,
    policy: &MediaFetchPolicy,
) -> Result<Bytes, UploadError> {
    fetch_media_with_resolver(url, kind, policy, Arc::new(SystemMediaHostResolver)).await
}

/// Fetch with an injected resolver for deterministic DNS-pinning tests.
#[doc(hidden)]
pub async fn fetch_media_with_resolver(
    url: &Url,
    kind: MediaKind,
    policy: &MediaFetchPolicy,
    resolver: Arc<dyn MediaHostResolver>,
) -> Result<Bytes, UploadError> {
    match tokio::time::timeout(
        policy.total_timeout,
        fetch_media_inner(url, kind, policy, resolver.as_ref()),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(UploadError::SsrfDenied("media fetch timed out".into())),
    }
}

async fn fetch_media_inner(
    url: &Url,
    kind: MediaKind,
    policy: &MediaFetchPolicy,
    resolver: &dyn MediaHostResolver,
) -> Result<Bytes, UploadError> {
    let mut current = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        validate_hop(&current, policy)?;
        let host = current
            .host_str()
            .ok_or_else(|| UploadError::SsrfDenied("URL has no host".into()))?
            .to_owned();
        let port = current.port_or_known_default().unwrap_or(443);

        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(policy.connect_timeout)
            .timeout(policy.total_timeout)
            .no_gzip()
            .no_brotli()
            .no_deflate();
        if policy.strict_network {
            let pinned = resolver.resolve_and_check(&host).await?;
            builder = builder.resolve(&host, std::net::SocketAddr::new(pinned, port));
        }
        let client = builder
            .build()
            .map_err(|_| UploadError::SsrfDenied("media fetch client build failed".into()))?;

        let response = client
            .get(current.clone())
            .send()
            .await
            .map_err(|e| media_fetch_error(&e))?;

        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| UploadError::SsrfDenied("redirect had no valid Location".into()))?;
            current = validate_redirect(&current, location, policy)?;
            continue;
        }

        if !response.status().is_success() {
            return Err(UploadError::InvalidUrl(format!(
                "media fetch responded with status {}",
                response.status()
            )));
        }

        if let Some(length) = response.content_length()
            && length > policy.max_bytes
        {
            return Err(UploadError::TooLarge {
                limit: policy.max_bytes,
                received: length,
            });
        }

        let body = read_bounded(response, policy.max_bytes).await?;
        if !kind.signature(&body) {
            return Err(UploadError::UnsupportedType(format!(
                "media body is not a recognized {} signature",
                kind.label()
            )));
        }
        return Ok(body);
    }
    Err(UploadError::SsrfDenied("too many redirects".into()))
}

/// Stream the body under the byte cap, regardless of `Content-Length`.
async fn read_bounded(
    mut response: reqwest::Response,
    max_bytes: u64,
) -> Result<Bytes, UploadError> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| media_fetch_error(&e))? {
        let received = body.len().saturating_add(chunk.len()) as u64;
        if received > max_bytes {
            return Err(UploadError::TooLarge {
                limit: max_bytes,
                received,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

/// Map a transport-level failure to a policy-flavored error.
fn media_fetch_error(e: &reqwest::Error) -> UploadError {
    if e.is_timeout() {
        UploadError::SsrfDenied("media fetch timed out".into())
    } else if e.is_connect() {
        UploadError::SsrfDenied("media fetch connection failed".into())
    } else if e.is_decode() {
        UploadError::InvalidUrl("media response body failed".into())
    } else {
        UploadError::InvalidUrl(format!("media fetch failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowlist_is_exact_and_suffix_safe() {
        let policy = MediaFetchPolicy::provider_cdn();
        assert!(policy.host_allowed("googleusercontent.com"));
        assert!(policy.host_allowed("lh3.googleusercontent.com"));
        assert!(policy.host_allowed("a.b.googleusercontent.com"));
        assert!(!policy.host_allowed("evil-googleusercontent.com"));
        assert!(!policy.host_allowed("googleusercontent.com.evil.test"));
        assert!(!policy.host_allowed("example.com"));
    }

    #[test]
    fn video_signature_recognizes_mp4_and_webm_but_not_text() {
        assert!(is_video_signature(
            b"\x00\x00\x00\x18ftypisom\x00\x00\x00\x00isommp42"
        ));
        assert!(is_video_signature(&[0x1A, 0x45, 0xDF, 0xA3, 0x00, 0x01]));
        assert!(!is_video_signature(b"just text"));
        assert!(!is_video_signature(b"\x89PNG\r\n\x1a\n"));
    }
}
