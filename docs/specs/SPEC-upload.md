# Module Specification: `upload`

**Module ID:** `upload`  
**Crate:** `gemini-bridge-upload` (`crates/upload`)  
**Phase:** Fase 1  
**Depends On:** `identity`, `transport`, `config` (uses `media-store` for local content cache)  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-2, US-3, §3.1, §4.7  
**Status:** Approved Draft — enriched for P.5  

---

## 1. Objective & Responsibility

The `upload` module handles all file ingestion for the bridge: accepting multipart uploads (`POST /v1/files`), optionally fetching image bytes from URL references, validating type and size, caching bytes locally via `media-store`, performing the Google resumable push upload protocol to acquire a Gemini `fileRef`, and deduplicating push uploads by SHA-256 content hash.

**In scope:**
- `POST /v1/files` — multipart file ingestion (handler body logic, not routing).
- Magic-byte MIME type detection (PNG, JPEG, WebP, GIF), rejecting unrecognised or non-`image/*` types.
- Streaming size limit enforcement (default 20 MiB) during body reception, before full buffering.
- SSRF-safe URL reference fetching: HTTP/HTTPS only; DNS resolution and private-address rejection; IP pinning per request; redirect re-validation.
- Two-step Google resumable push upload to `https://content-push.googleapis.com/upload/`:
  1. Initiate upload session (headers: `X-Goog-Upload-Command: start`, `X-Goog-Upload-Protocol: resumable`, `Content-Length`). Parse `X-Goog-Upload-URL` from response.
  2. Stream bytes with `X-Goog-Upload-Command: upload, finalize`. Extract `fileRef` from response payload.
- Local content-hash deduplication: identical content reuses previously cached `fileRef` without re-uploading.
- `GET /v1/files/{id}` — retrieval of locally stored bytes by ID (handler body logic).

**Out of scope:**
- HTTP routing and request body parsing (→ `http-server`).
- Image URL extraction from Gemini response trees (→ `image-gen`).
- Image generation orchestration (→ `image-gen`).
- Media TTL/gallery management (→ `media-store`, `gallery`).

---

## 2. Public API & Interfaces

### 2.1 Configuration

```rust
pub struct UploadLimits {
    /// Maximum bytes accepted before rejection. Default: 20 MiB (20 * 1024 * 1024).
    pub max_bytes: u64,
    /// Accepted MIME type prefix. Only "image/" is supported for MVP.
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
```

### 2.2 Domain Types

```rust
/// Represents a fully validated and stored upload.
pub struct UploadedFile {
    /// Local media-store ID for retrieval via `GET /v1/files/{id}`.
    pub id: String,
    /// Hex-encoded SHA-256 of content bytes.
    pub sha256: String,
    /// Detected MIME type (e.g. "image/png").
    pub mime_type: String,
    /// Content size in bytes.
    pub size_bytes: u64,
    /// Gemini fileRef acquired from push upload (cached for deduplication).
    pub file_ref: String,
    /// Unix creation timestamp.
    pub created_at: i64,
}
```

### 2.3 Errors

```rust
use thiserror::Error;
use url::Url;

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
```

### 2.4 Service Trait

```rust
use bytes::Bytes;
use url::Url;

#[async_trait::async_trait]
pub trait UploadService: Send + Sync {
    /// Accept raw bytes, detect MIME type, enforce limits, local-cache,
    /// and acquire a Gemini fileRef (deduplicated by SHA-256).
    /// `claimed_mime` is advisory only; magic-byte detection overrides it.
    async fn upload_bytes(
        &self,
        bytes: Bytes,
        claimed_mime: Option<&str>,
    ) -> Result<UploadedFile, UploadError>;

    /// Fetch bytes from a URL reference with SSRF validation, then call
    /// `upload_bytes` on the result.
    async fn upload_from_url(&self, url: Url) -> Result<UploadedFile, UploadError>;

    /// Retrieve locally stored bytes by media-store `id`.
    async fn get(&self, id: &str) -> Result<Bytes, UploadError>;

    /// Return existing `UploadedFile` for a given SHA-256 hash, or `None` if
    /// no such upload exists locally.
    async fn get_by_hash(&self, sha256: &str) -> Option<UploadedFile>;
}
```

### 2.5 Push Upload Client

```rust
/// Handles the two-step Google resumable push upload protocol.
#[async_trait::async_trait]
pub trait PushUploadClient: Send + Sync {
    /// Initiate an upload session. Returns the resumable upload URL.
    async fn initiate_session(
        &self,
        mime_type: &str,
        size_bytes: u64,
    ) -> Result<String, UploadError>;

    /// Upload bytes to the session URL and return the resulting Gemini fileRef.
    async fn upload_bytes(
        &self,
        session_url: &str,
        bytes: Bytes,
    ) -> Result<String, UploadError>;
}
```

### 2.6 SSRF Guard

```rust
use std::net::IpAddr;

/// Checks a resolved IP address against the blocked ranges.
///
/// Blocked IPv4 ranges: loopback (127.0.0.0/8), private (10.0.0.0/8,
/// 172.16.0.0/12, 192.168.0.0/16), link-local (169.254.0.0/16),
/// carrier-grade NAT (100.64.0.0/10), broadcast, and any address in the
/// all-zeros block (0.0.0.0/8).
///
/// Blocked IPv6 ranges: loopback (::1), unspecified (::), unique-local
/// (fc00::/7), link-local (fe80::/10), and IPv4-mapped private addresses
/// (::ffff:0:0/96 when the embedded IPv4 is in a blocked range).
pub fn is_address_allowed(ip: IpAddr) -> bool;

/// Resolve `host` to IP addresses, reject if any resolved IP is blocked.
/// Returns the first allowed IP address so it can be pinned to the connection.
pub async fn resolve_and_check(host: &str) -> Result<IpAddr, UploadError>;
```

---

## 3. Behavior & Invariants

1. **Magic-Byte MIME Sniffing:**
   MIME type is determined solely from file signatures — the caller's `Content-Type` header or file extension is advisory and never trusted alone.
   - PNG: `\x89PNG\r\n\x1a\n` (first 8 bytes).
   - JPEG: `\xff\xd8\xff` (first 3 bytes).
   - GIF87a / GIF89a: `GIF87a` / `GIF89a` (first 6 bytes).
   - WebP: `RIFF` at offset 0 and `WEBP` at offset 8.
   - Any file not matching the above signatures is rejected with `UploadError::UnsupportedType`.

2. **Streaming Size Limit:**
   Body bytes are counted during reception. The moment the cumulative count exceeds `max_bytes`, reception is aborted with `UploadError::TooLarge`. Full buffering before validation is prohibited.

3. **SSRF Protection:**
   - Only `http://` and `https://` schemes are permitted; others fail with `SsrfDenied`.
   - All hostnames are resolved synchronously before any HTTP connection is opened.
   - Each resolved IP address is checked against the blocked ranges (see §2.6).
   - The HTTP client is configured to connect only to the resolved/approved IP, preventing DNS rebinding.
   - Every redirect target URL is independently re-validated through the full SSRF guard; redirects to blocked targets abort the fetch with `SsrfDenied`.
   - `file://`, `gopher://`, `ftp://`, and data URIs are never accepted.

4. **Resumable Push Upload Protocol:**
   - Step 1 (Initiate): `POST https://content-push.googleapis.com/upload/` with headers `X-Goog-Upload-Command: start`, `X-Goog-Upload-Protocol: resumable`, `X-Goog-Upload-Header-Content-Length: {size}`, `X-Goog-Upload-Header-Content-Type: {mime}`, plus Gemini session auth headers from `identity`. Parse `X-Goog-Upload-URL` header from 200 response.
   - Step 2 (Upload): `POST {X-Goog-Upload-URL}` with headers `X-Goog-Upload-Command: upload, finalize`, `Content-Length: {size}`, body = raw bytes. Extract `fileRef` token from response JSON.
   - Retry policy: the initiation step is idempotent and may be retried once on transient transport failure. The upload step (`NeverRetry`) must not be automatically retried.

5. **Content-Hash Deduplication:**
   Before initiating any push upload, compute SHA-256 of the bytes and call `media-store.find_by_hash(sha256)`. If a matching `id` is found with a non-empty `file_ref` in its metadata, return the cached `UploadedFile` directly without re-uploading.

6. **Credential Safety:**
   Session cookies, auth hash tokens, and upload session URLs are never logged, traced, or embedded in error messages. File content bytes are not logged.

7. **Local Cache After Upload:**
   After a successful push upload, the bytes and metadata (including the acquired `file_ref`) are persisted in `media-store`. This ensures `GET /v1/files/{id}` works without re-fetching upstream.

---

## 4. Acceptance Criteria

### US-2 / US-3 Traceability

| PRD AC | Requirement | Covered by |
|---|---|---|
| US-3 AC 1 | `POST /v1/files` accepts multipart upload and returns `id` | `upload_bytes` → `UploadedFile.id` |
| US-3 AC 2 | `id` referenceable in `messages[].content[]` image_url and `reference_images` | `UploadedFile.id` / `get_by_hash` |
| US-3 AC 3 | Size and type validated with MIME sniffing; SSRF guard for URL references | Magic-byte check, `is_address_allowed`, `resolve_and_check` |
| US-3 AC 4 | Deduplication: same content reuses fileRef (hash-based) | `get_by_hash` / SHA-256 dedup |
| US-2 AC (ref) | Reference images resolved to fileRef for image generation | `upload_from_url` / `upload_bytes` → `file_ref` |

### Module-Level Acceptance Criteria

1. PNG, JPEG, GIF, and WebP bytes with correct signatures are accepted; corrupted or non-image bytes with a false `Content-Type: image/png` header are rejected with `UnsupportedType`.
2. Sending a request body exceeding `max_bytes` triggers `TooLarge` before the full payload is buffered server-side.
3. URL references using non-HTTP(S) schemes (`file://`, `data:`, etc.) are rejected with `SsrfDenied`.
4. URL references resolving to IPv4 private ranges (10.x.x.x, 172.16–31.x.x, 192.168.x.x), loopback (127.x.x.x), and link-local (169.254.x.x) are rejected with `SsrfDenied`.
5. URL references that redirect to a forbidden address also result in `SsrfDenied` (redirect chain is fully re-validated).
6. A mock resumable push upload completes in two steps, returns a valid `fileRef`, and the acquired `fileRef` is stored alongside the content in `media-store`.
7. Uploading identical content bytes twice returns the same `UploadedFile.id` and `file_ref` on the second call (no duplicate push upload issued).
8. `GET /v1/files/{id}` returns the exact bytes previously stored (verified in handler integration test).
9. No session tokens, auth hashes, or upload URLs appear in test logs or traces.

---

## 5. Testing Strategy

- **MIME Sniffing Tests (`crates/upload/tests/mime_test.rs`):**
  - Valid PNG/JPEG/GIF/WebP magic bytes accepted.
  - Truncated or random bytes rejected.
  - Claimed `image/png` Content-Type with JPEG body → rejected by sniffing, not claimed type.
  - Non-image format (PDF, ZIP) rejected with `UnsupportedType`.

- **Streaming Size Limit Tests (`crates/upload/tests/size_test.rs`):**
  - Body exactly at limit is accepted; one byte over limit is rejected.
  - Streaming abort confirmed before complete buffering.

- **SSRF Guard Tests (`crates/upload/tests/ssrf_test.rs`):**
  - Loopback, private IPv4 ranges, link-local, carrier-grade NAT blocked.
  - IPv6 loopback (::1), unique-local (fc00::/7), link-local (fe80::/10) blocked.
  - IPv4-mapped private addresses (::ffff:10.0.0.1) blocked.
  - Valid public IP addresses allowed.
  - Redirect to blocked address terminates with `SsrfDenied`.
  - `file://` and `data:` schemes rejected.

- **Resumable Upload Integration Tests (`crates/upload/tests/push_test.rs`) using `wiremock`:**
  - Step 1: mock returns `X-Goog-Upload-URL`; confirmed header presence.
  - Step 2: mock returns JSON with `fileRef`; confirmed extraction.
  - Transient Step 1 failure followed by success → one retry succeeds.
  - Step 2 failure is not retried.

- **Deduplication Tests:**
  - Two `upload_bytes` calls with identical content hit Step 2 only once (confirmed via `wiremock` request count).
  - `get_by_hash` returns cached file on second call.

---

## 6. Boundaries

- **Always:** Detect MIME type by magic bytes; enforce size limit before full buffering; apply full SSRF guard to all URL inputs; validate redirect targets; deduplicate by SHA-256; store successfully pushed files in `media-store`.
- **Ask First:** Supporting non-image MIME types for MVP; extending URL schemes; increasing default upload size above 20 MiB; adding retry on upload-phase failures.
- **Never:** Trust claimed `Content-Type` or file extension for type validation; fetch `file://`, `gopher://`, or `ftp://` URLs; connect to loopback or private addresses; log cookies, auth tokens, or upload session URLs; retry a `NeverRetry` upload step automatically.
