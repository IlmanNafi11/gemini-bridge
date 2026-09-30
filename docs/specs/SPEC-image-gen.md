# Module Specification: `image-gen`

**Module ID:** `image-gen`  
**Crate:** `gemini-bridge-image-gen` (`crates/image-gen`)  
**Phase:** Fase 1  
**Depends On:** `gemini-adapter`, `upload`, `media-store`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-2  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Handle `POST /v1/images/generations` end-to-end: accept client prompt and optional reference images, resolve references through `upload`, build and forward the generation request via `gemini-adapter`, extract generated image URLs from the Gemini response tree, download and cache those images via `media-store`, and return stable bridge-proxied URLs or `b64_json` payloads.

---

## 2. Public API & Interfaces

```rust
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImageGenError {
    #[error("Reference upload failed")]
    UploadFailed,
    #[error("No image found in upstream response")]
    NoImageExtracted,
    #[error("Upstream image fetch failed")]
    FetchFailed,
    #[error("Request validation failed: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageGenerationRequest {
    pub prompt: String,
    pub n: Option<u32>,
    pub size: Option<String>,
    pub response_format: Option<String>,
    pub reference_images: Option<Vec<ImageInput>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ImageInput {
    Url(String),
    B64Json { b64_json: String, mime_type: Option<String> },
    FileId(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageGenerationResponse {
    pub created: i64,
    pub data: Vec<ImageResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
}
```

---

## 3. Behavior & Invariants

1. **URL Extraction:** Extract generated image URLs from the Gemini response tree using both position-based and `googleusercontent.com` URL pattern regex. Fallback to regex when positional extraction fails, returning an explicit error rather than a blank result if both fail.
2. **Local Caching:** Downloaded images stored in `media-store` by SHA-256 with generation metadata (prompt, model, generation time). Bridge-proxied URL is the primary return when `response_format=url`.
3. **Reference Image Input:** Each input is resolved through `upload` before inclusion in the generation request, regardless of whether it's a URL, base64, or existing file ID.
4. **Expiry Protection:** Never return original Gemini-issued `googleusercontent.com` URLs to clients; always proxy through the local bridge endpoint (`GET /v1/images/{id}`).
5. **Validation:** The E1 golden test requirement (5 prompts + 2 multimodal) is an acceptance run, not a CI unit test — live acceptance must confirm valid decodable images.

---

## 4. Testing Strategy

- Extraction unit tests against recorded Gemini response tree fixtures (sanitized), including absent-image and fallback-pattern cases.
- b64 encoding/decoding round-trip for `response_format=b64_json`.
- Reference input routing through mock upload service for each `ImageInput` variant.
- `GET /v1/images/{id}` retrieval from cache with media-store mock.
- Live golden test (opt-in): 5 prompts + 2 multimodal; verified via magic bytes and non-zero image dimensions.

---

## 5. Boundaries

- **Always:** Cache and proxy generated images; never return expiring upstream URLs directly to clients.
- **Ask First:** Changing the extraction strategy selection order or adding support for non-image response formats.
- **Never:** Skip SSRF validation on URL-format reference inputs.
