# Module Specification: `upload`

**Module ID:** `upload`  
**Crate:** `gemini-bridge-upload` (`crates/upload`)  
**Phase:** Fase 1  
**Depends On:** `identity`, `transport`, `config` (uses `media-store` for local content cache)  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-3, §3.1, §4.7  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Accept image inputs from clients, validate and cache them locally, safely retrieve optional URL references, perform Gemini resumable push upload to `content-push.googleapis.com`, and return/cache the resulting Gemini `fileRef`. This module serves both `POST /v1/files` and image-generation reference input.

---

## 2. Public API & Interfaces

```rust
use bytes::Bytes;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum UploadError {
    #[error("File exceeds configured limit")]
    TooLarge,
    #[error("Only supported image/* files are accepted")]
    UnsupportedType,
    #[error("Reference URL violates network policy")]
    SsrfDenied,
    #[error("Upstream file push failed")]
    UpstreamFailure,
    #[error("File not found")]
    NotFound,
}

pub struct UploadLimits {
    pub max_bytes: u64, // default 20 MiB per PRD
    pub allowed_mime_prefix: &'static str, // "image/"
}

pub struct UploadedFile {
    pub id: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub file_ref: String,
}

#[async_trait::async_trait]
pub trait UploadService: Send + Sync {
    async fn upload_bytes(&self, bytes: Bytes, claimed_mime: Option<&str>) -> Result<UploadedFile, UploadError>;
    async fn upload_from_url(&self, url: Url) -> Result<UploadedFile, UploadError>;
    async fn get(&self, id: &str) -> Result<Bytes, UploadError>;
}
```

---

## 3. Behavior & Invariants

1. **Mime Sniffing:** Determine type from file signatures; never trust `Content-Type` or extension alone. Only validated `image/*` files accepted for MVP.
2. **Size Bound:** Default maximum is 20 MiB; configurable limit is checked during streaming body reception, before buffering the full request.
3. **SSRF Guard:** URL references must use HTTP(S), reject loopback/private/link-local/reserved addresses, resolve DNS and pin approved address to connection, and revalidate every redirect target. Do not follow unsafe redirects.
4. **Resumable Push Upload:** Use the two-step push upload protocol and return valid `fileRef` only after upstream completion.
5. **Deduplication:** Cache fileRef by SHA-256 content hash; identical uploads reuse previously validated ref where still valid.
6. **Credential Safety:** Never log request cookies, upload auth tokens, file contents, or full sensitive URLs.

---

## 4. Testing Strategy

- Magic-byte MIME sniffing tests including spoofed extension/header and malformed images.
- Streaming size-limit test ensuring oversized bodies are rejected before full buffering.
- SSRF tests for IPv4/IPv6 private, loopback, link-local, multicast/reserved ranges, DNS rebinding, and redirect escape.
- Mocked resumable upload initiation/completion test and content-hash cache reuse test.
- `GET /v1/files/{id}` retrieval test.

---

## 5. Boundaries

- **Always:** Use the configured size bound, MIME sniffing, and SSRF restrictions before outbound fetch or push.
- **Ask First:** Supporting non-image files, allowing additional schemes, or changing default upload size.
- **Never:** Fetch private/loopback addresses, trust claimed MIME without sniffing, or accept URL redirects outside policy.
