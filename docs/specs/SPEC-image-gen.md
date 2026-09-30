# Module Specification: `image-gen`

**Module ID:** `image-gen`  
**Crate:** `gemini-bridge-image-gen` (`crates/image-gen`)  
**Phase:** Fase 1  
**Depends On:** `gemini-adapter`, `upload`, `media-store`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-2, §1.3 KPI 5, §3.1, §4.3, §4.9  
**Status:** Approved Draft — enriched for P.5  

---

## 1. Objective & Responsibility

The `image-gen` module implements the end-to-end image generation pipeline for `POST /v1/images/generations` and image retrieval via `GET /v1/images/{id}`. It coordinates all participating modules in the correct order: validates the incoming request, resolves reference images through `upload`, issues the generation request to Gemini Web via `gemini-adapter`, extracts generated image URLs from the upstream response tree, downloads and caches them in `media-store`, and returns bridge-proxied stable URLs or `b64_json` payloads. No expiring Google-issued URLs are ever exposed to API clients.

**In scope:**
- Parsing and validating `ImageGenerationRequest` (prompt, n, size, response_format, reference_images).
- Resolving `reference_images` inputs (URL, base64 data URI, or local file `id`) through `upload` to Gemini `fileRef`s.
- Constructing and dispatching the generation request to `gemini-adapter`.
- Extracting generated image URLs from the Gemini response tree via positional slot access (schema-driven) and regex fallback for `googleusercontent.com` URLs.
- Downloading image bytes and caching them in `media-store` with generation metadata (prompt, model, timestamp).
- Returning stable bridge-proxied URLs (`/v1/images/{id}`) or base64-encoded bytes.
- `GET /v1/images/{id}` retrieval from `media-store`.

**Out of scope:**
- HTTP routing, multipart body parsing, and authentication (→ `http-server`).
- SSRF validation, push upload protocol (→ `upload`).
- Upstream wire protocol, `f.req` framing (→ `gemini-adapter`).
- Content deduplication and TTL management (→ `media-store`).

---

## 2. Public API & Interfaces

### 2.1 Request and Response Structures

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct ImageGenerationRequest {
    /// User prompt describing the image to generate.
    pub prompt: String,
    /// Number of images to generate. Default 1. Gemini Web may not support >1.
    pub n: Option<u32>,
    /// Requested dimensions. Gemini Web ignores this; passed through for spec compliance.
    pub size: Option<String>,
    /// Output format: "url" (default) | "b64_json".
    pub response_format: Option<String>,
    /// Optional reference image inputs (URL strings, base64 data URIs, or local file IDs).
    pub reference_images: Option<Vec<ImageInput>>,
}

/// A single reference image input in a variety of accepted forms.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ImageInput {
    /// HTTP(S) URL to a reference image. Fetched and validated through `upload`.
    Url(String),
    /// Inline base64-encoded image with optional MIME type hint.
    B64Json { b64_json: String, mime_type: Option<String> },
    /// ID of a file previously uploaded via `POST /v1/files`.
    FileId(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageGenerationResponse {
    /// Unix timestamp of image creation.
    pub created: i64,
    /// One or more generated image results.
    pub data: Vec<ImageResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageResult {
    /// Bridge-proxied URL (`/v1/images/{id}`). Present when `response_format = "url"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Standard base64-encoded image bytes. Present when `response_format = "b64_json"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
}
```

### 2.2 Errors

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImageGenError {
    #[error("Reference upload failed: {0}")]
    UploadFailed(String),
    #[error("No generated image found in upstream response")]
    NoImageExtracted,
    #[error("Upstream image download failed: {0}")]
    ImageDownloadFailed(String),
    #[error("Media store error: {0}")]
    StoreError(String),
    #[error("Request validation failed: {0}")]
    Validation(String),
    #[error("Upstream adapter error: {0}")]
    AdapterError(String),
}
```

### 2.3 Service Trait

```rust
use bytes::Bytes;

#[async_trait::async_trait]
pub trait ImageGenService: Send + Sync {
    /// Resolve references, generate image(s) via `gemini-adapter`, cache results,
    /// and return a stable response.
    async fn generate(
        &self,
        req: ImageGenerationRequest,
    ) -> Result<ImageGenerationResponse, ImageGenError>;

    /// Retrieve a previously generated and cached image by its local ID.
    /// Returns `(content_bytes, mime_type)`.
    async fn get_image(&self, id: &str) -> Result<(Bytes, String), ImageGenError>;
}
```

### 2.4 Image URL Extractor

```rust
/// Extract generated image URLs from a Gemini response payload.
///
/// Strategy (in order):
/// 1. Positional slot extraction using indices from `schema/gemini-web.toml`.
/// 2. Regex fallback: match `https://[^\s"']*googleusercontent\.com/[^\s"']+`
///    anywhere in the serialized payload.
/// If neither yields a URL, returns `Err(ImageGenError::NoImageExtracted)`.
pub struct ImageExtractor;

impl ImageExtractor {
    pub fn extract_image_urls(payload: &serde_json::Value) -> Vec<String>;
    pub fn regex_fallback(raw_text: &str) -> Vec<String>;
}
```

---

## 3. Behavior & Invariants

1. **Reference Resolution:**
   All `reference_images` inputs are resolved through `upload` before sending the generation request. For each input:
   - `Url(u)`: call `upload.upload_from_url(u)` → `file_ref`.
   - `B64Json { b64_json, mime_type }`: decode base64, call `upload.upload_bytes(bytes, mime_type)` → `file_ref`.
   - `FileId(id)`: call `upload.get_by_hash` or `media-store.get(id)` to retrieve the previously cached `file_ref`.

2. **Adapter Dispatch:**
   The image generation request (prompt + reference `fileRef`s) is forwarded to `gemini-adapter` as a provider-neutral `LlmRequest` with image-generation intent. The adapter maps this to Gemini Web `StreamGenerate`. The module does not access Gemini wire protocol directly.

3. **URL Extraction & Download:**
   - After the adapter returns the response payload, `ImageExtractor::extract_image_urls` is called.
   - For each extracted URL, bytes are fetched through `transport` (or a plain HTTPS client that does not apply SSRF validation because the source is our own adapter, not untrusted user input).
   - Downloaded bytes are stored in `media-store` with prompt, model, and creation timestamp.

4. **Expiry Protection:**
   Original `googleusercontent.com` URLs (which expire and require authentication) are **never returned** to API clients. Only bridge-proxied stable URLs (`/v1/images/{id}`) or base64-encoded bytes are returned.

5. **Idempotency:**
   Downloaded image bytes are stored in `media-store` keyed by SHA-256. If the same response URL is fetched again (e.g., retry scenario), the deduplication in `media-store` prevents redundant writes.

6. **Error Cascading:**
   - `upload` error → `UploadFailed`; operation aborts before adapter call.
   - Zero image URLs extracted → `NoImageExtracted`.
   - Download failure → `ImageDownloadFailed`.
   - `media-store` write failure → `StoreError`.
   - All errors include context to aid diagnosis but never include credentials, cookies, or auth tokens.

7. **`n > 1` Handling:**
   If the upstream response delivers multiple image URLs, all are downloaded, cached, and returned in the `data` array. If Gemini Web returns only one image despite `n > 1`, the single result is returned without error.

---

## 4. Acceptance Criteria

### US-2 Traceability

| PRD US-2 AC | Requirement | Covered by |
|---|---|---|
| US-2 AC 1 | `POST /v1/images/generations` accepts `prompt`, `n`, `size`, `response_format`, `reference_images[]` | `ImageGenerationRequest` deserialization |
| US-2 AC 2 | Reference images uploaded via resumable push, prompt sent to StreamGenerate, image URL extracted | `upload` reference resolution → `gemini-adapter` dispatch → `ImageExtractor` |
| US-2 AC 3 | Result as bridge-proxied URL or `b64_json` | `ImageResult.url` (proxy) or `b64_json` encoding |
| US-2 AC 4 | Images cached locally with metadata (prompt, model, time) | `media-store.put(bytes, metadata)` |
| US-2 AC 5 | `GET /v1/images/{id}` retrieval endpoint | `get_image(id)` |
| US-2 AC 6 (Acceptance test) | ≥5 distinct prompts + ≥2 multimodal produce valid images (opt-in live run) | Live golden test (`tests/e2e_image_test.rs`, opt-in with credentials) |

### Module-Level Acceptance Criteria

1. A request with a URL reference image resolves via `upload`, produces a `file_ref`, and correctly passes it to the adapter request.
2. A request with a base64-encoded reference image decodes, sniff-validates the MIME type, and produces a `file_ref`.
3. A request with a local file `id` retrieves the associated `file_ref` from the cache without re-uploading.
4. `ImageExtractor::extract_image_urls` returns at least one URL from a recorded Gemini response fixture; `NoImageExtracted` is returned for an empty payload.
5. `ImageExtractor::regex_fallback` extracts all `googleusercontent.com` URLs from a raw text payload without false positives on unrelated HTTPS URLs.
6. When `response_format = "b64_json"`, the response includes valid base64-encoded image bytes decodable by standard libraries.
7. When `response_format = "url"`, the response URL is in the form `/v1/images/{id}` and `get_image(id)` returns the original bytes.
8. No `googleusercontent.com` URL appears in any field of the API response.
9. Providing `reference_images: null` (no references) generates the image without upload steps.
10. `get_image(id)` returns `StoreError` for unknown IDs.

---

## 5. Testing Strategy

- **Extraction Unit Tests (`crates/image-gen/src/extractor.rs` test module):**
  - Parse a representative sanitized Gemini response fixture: assert at least one `googleusercontent.com` URL extracted.
  - Empty or structurally unexpected response payload: assert `NoImageExtracted`.
  - Regex fallback on a raw string with embedded URLs: verify exact match count and no false positives on unrelated HTTPS URLs.

- **Reference Input Routing Tests (`crates/image-gen/tests/input_test.rs`) with mock `UploadService`:**
  - URL input routes to `upload_from_url` exactly once, returning the mock `file_ref`.
  - Base64 input decodes and routes to `upload_bytes` with correct bytes.
  - FileId input routes to `get_by_hash` or `get`, returning the cached `file_ref`.
  - Empty `reference_images` field: `upload` is never called.

- **Integration Tests with Mock Adapter and Store (`crates/image-gen/tests/gen_test.rs`):**
  - Mock adapter returns a fixture payload; verify `generate` resolves image URL, downloads bytes, stores in mock `media-store`, and returns a proxy URL.
  - `response_format = "b64_json"`: verify base64 in response decodes to same bytes as stored.
  - Adapter returns empty payload → `NoImageExtracted` propagated.
  - `media-store.put` failure → `StoreError` propagated.

- **`GET /v1/images/{id}` Retrieval Test (`tests/e2e_image_test.rs`):**
  - Store a fixture image in a mock store; call `get_image(id)` and verify byte equality.
  - Unknown `id` returns `StoreError`.

- **Live Golden Acceptance Test (opt-in, `tests/e2e_image_test.rs` with `BRIDGE_LIVE_TEST=1`):**
  - 5 distinct prompts produce non-empty valid images (verified via magic bytes and non-zero file size).
  - 2 prompts with reference images produce valid blended images.
  - Requires operator-provided local credentials; never committed to repository; excluded from default CI.

---

## 6. Boundaries

- **Always:** Resolve all reference inputs through `upload` before dispatch; cache all downloaded images in `media-store`; return only bridge-proxied URLs or base64; include prompt/model metadata in `media-store` records.
- **Ask First:** Changing the extraction strategy precedence (positional-first vs. regex-first); adding support for non-image response formats; implementing video generation in this crate.
- **Never:** Return raw `googleusercontent.com` URLs to clients; skip SSRF validation on URL-format reference inputs; embed Gemini wire protocol details (use adapter contract only); log auth tokens, cookies, or session identifiers.
