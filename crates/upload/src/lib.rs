use async_trait::async_trait;
use bytes::Bytes;
use gemini_bridge_media_store::{MediaMetadata, MediaStore};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use url::Url;

pub mod error;
pub mod media_fetch;
pub mod push_client;
pub mod ssrf;

pub use error::UploadError;
pub use media_fetch::{
    HardenedMediaDownloader, MediaDownloader, MediaFetchPolicy, MediaKind, fetch_media,
};
pub use push_client::{HttpPushUploadClient, PushUploadClient};
pub use ssrf::{is_address_allowed, resolve_and_check};

/// Upload constraints. `max_bytes` defaults to 20 MiB.
#[derive(Debug, Clone)]
pub struct UploadLimits {
    pub max_bytes: u64,
    pub allowed_mime_prefix: &'static str,
}

impl Default for UploadLimits {
    fn default() -> Self {
        Self {
            max_bytes: 20 * 1024 * 1024,
            allowed_mime_prefix: "image/",
        }
    }
}

/// A validated file cached locally and accepted by the upstream push API.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UploadedFile {
    pub id: String,
    pub sha256: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub file_ref: String,
    pub created_at: i64,
}

#[async_trait]
pub trait UploadService: Send + Sync {
    async fn upload_bytes(
        &self,
        bytes: Bytes,
        claimed_mime: Option<&str>,
    ) -> Result<UploadedFile, UploadError>;
    async fn upload_from_url(&self, url: Url) -> Result<UploadedFile, UploadError>;
    async fn get(&self, id: &str) -> Result<Bytes, UploadError>;
    async fn get_by_hash(&self, sha256: &str) -> Option<UploadedFile>;

    /// Maximum accepted content size in bytes.
    fn max_bytes(&self) -> u64;

    /// MIME type recorded for a stored file, if present.
    async fn get_by_id_mime(&self, id: &str) -> Option<String>;

    /// Retrieve a stored [`UploadedFile`] by local `id`.
    async fn get_by_id(&self, id: &str) -> Option<UploadedFile>;
}

pub struct UploadServiceImpl<S, P> {
    store: S,
    push: Arc<P>,
    limits: UploadLimits,
}

impl<S, P> UploadServiceImpl<S, P>
where
    S: MediaStore,
    P: PushUploadClient + 'static,
{
    pub fn new(store: S, push: Arc<P>, limits: UploadLimits) -> Self {
        Self {
            store,
            push,
            limits,
        }
    }

    async fn fetch_reference(&self, url: Url) -> Result<Bytes, UploadError> {
        // Caller-supplied references retain their documented public HTTP(S)
        // behavior. The shared mechanism still validates credentials, all DNS
        // answers, every redirect hop, signatures, deadlines, and body bounds.
        let policy = MediaFetchPolicy::caller_reference(self.limits.max_bytes);
        fetch_media(&url, MediaKind::Image, &policy).await
    }
}

/// Enforce HTTP(S) and deny literal blocked IPs before DNS resolution.
pub fn validate_reference_url(url: &Url) -> Result<Url, UploadError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UploadError::SsrfDenied(
            "only http and https URLs are permitted".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UploadError::SsrfDenied(
            "URL user information is not permitted".into(),
        ));
    }
    match url.host() {
        Some(url::Host::Ipv4(v4)) => {
            if !is_address_allowed(std::net::IpAddr::V4(v4)) {
                return Err(UploadError::SsrfDenied(format!(
                    "address {v4} is in a blocked range"
                )));
            }
        }
        Some(url::Host::Ipv6(v6)) => {
            if !is_address_allowed(std::net::IpAddr::V6(v6)) {
                return Err(UploadError::SsrfDenied(format!(
                    "address {v6} is in a blocked range"
                )));
            }
        }
        Some(url::Host::Domain(domain)) => {
            if let Ok(ip) = domain.parse::<std::net::IpAddr>()
                && !is_address_allowed(ip)
            {
                return Err(UploadError::SsrfDenied(format!(
                    "address {ip} is in a blocked range"
                )));
            }
        }
        None => return Err(UploadError::SsrfDenied("URL has no host".into())),
    }
    Ok(url.clone())
}

/// Parse and validate a redirect target relative to its current URL.
pub fn validate_redirect_target(base: &Url, location: &str) -> Result<Url, UploadError> {
    let target = base
        .join(location)
        .map_err(|_| UploadError::SsrfDenied("invalid redirect target".into()))?;
    validate_reference_url(&target)
}

/// Detect MIME from file signature; never uses the caller's claimed type.
pub fn detect_mime(bytes: &Bytes) -> Option<&'static str> {
    let b = bytes.as_ref();
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[async_trait]
impl<S, P> UploadService for UploadServiceImpl<S, P>
where
    S: MediaStore,
    P: PushUploadClient + 'static,
{
    async fn upload_bytes(
        &self,
        bytes: Bytes,
        _claimed_mime: Option<&str>,
    ) -> Result<UploadedFile, UploadError> {
        let size = bytes.len() as u64;
        if size > self.limits.max_bytes {
            return Err(UploadError::TooLarge {
                limit: self.limits.max_bytes,
                received: size,
            });
        }
        let mime =
            detect_mime(&bytes).ok_or_else(|| UploadError::UnsupportedType("unknown".into()))?;
        if !mime.starts_with(self.limits.allowed_mime_prefix) {
            return Err(UploadError::UnsupportedType(mime.into()));
        }
        let hash = format!("{:x}", Sha256::digest(&bytes));
        if let Some(id) = self.store.find_by_hash(&hash).await
            && let Ok((_, metadata)) = self.store.get(&id).await
            && !metadata.file_ref.is_empty()
        {
            return Ok(to_uploaded_file(metadata));
        }
        let session_url = self.push.initiate_session(mime, size).await?;
        let file_ref = self.push.upload_bytes(&session_url, bytes.clone()).await?;
        let metadata = MediaMetadata {
            id: String::new(),
            sha256: hash,
            mime_type: mime.to_string(),
            size_bytes: size,
            created_at: gemini_bridge_media_store::cleanup::now_unix(),
            expires_at: None,
            prompt: None,
            file_ref,
            model: None,
        };
        let id = self.store.put(bytes, metadata).await?;
        let (_, metadata) = self.store.get(&id).await?;
        Ok(to_uploaded_file(metadata))
    }

    async fn upload_from_url(&self, url: Url) -> Result<UploadedFile, UploadError> {
        let bytes = self.fetch_reference(url).await?;
        self.upload_bytes(bytes, None).await
    }

    async fn get(&self, id: &str) -> Result<Bytes, UploadError> {
        let (bytes, _) = self.store.get(id).await.map_err(|e| match e {
            gemini_bridge_media_store::StoreError::NotFound(_) => {
                UploadError::NotFound(id.to_string())
            }
            other => UploadError::from(other),
        })?;
        Ok(bytes)
    }

    async fn get_by_hash(&self, sha256: &str) -> Option<UploadedFile> {
        let id = self.store.find_by_hash(sha256).await?;
        let (_, metadata) = self.store.get(&id).await.ok()?;
        Some(to_uploaded_file(metadata))
    }

    fn max_bytes(&self) -> u64 {
        self.limits.max_bytes
    }

    async fn get_by_id_mime(&self, id: &str) -> Option<String> {
        let (_, metadata) = self.store.get(id).await.ok()?;
        Some(metadata.mime_type)
    }

    async fn get_by_id(&self, id: &str) -> Option<UploadedFile> {
        let (_, metadata) = self.store.get(id).await.ok()?;
        Some(to_uploaded_file(metadata))
    }
}

fn to_uploaded_file(metadata: MediaMetadata) -> UploadedFile {
    UploadedFile {
        id: metadata.id,
        sha256: metadata.sha256,
        mime_type: metadata.mime_type,
        size_bytes: metadata.size_bytes,
        file_ref: metadata.file_ref,
        created_at: metadata.created_at,
    }
}
