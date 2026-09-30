# Module Specification: `video-adapter`

**Module ID:** `video-adapter`  
**Crate:** `gemini-bridge-adapter-video` (`crates/video-adapter`)  
**Phase:** Fase 3 (Experimental)  
**Depends On:** `gemini-adapter` (generation and capability detection), `media-store` (content-addressed caching of supported results). Feature enablement is read from `config` at runtime composition.
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-9, §4.9
**Status:** Approved Draft — enriched for P.7

---

## 1. Objective & Responsibility

The `video-adapter` module provides an experimental, opt-in adapter for Gemini Web video generation, exposed through an OpenAI-shaped generation request/response contract. It is registered as a plugin but **disabled by default**. If the adapter is disabled or the active upstream/account does not support the requested video pipeline, the service returns a clear `501 Not Implemented` response with an actionable reason rather than an unhandled `500 Internal Server Error`. If supported, generated video content is retrieved and cached locally, then returned via a bridge-proxied URL or base64 using the exact same response shape as image generation.

**In scope:**
- `POST /v1/videos/generations` request and response models with `prompt`, optional duration/format parameters, and URL/base64 result entries.
- Experimental plugin registration controlled by an explicit configuration flag that defaults to `false`.
- Dispatch to Gemini Web through the `gemini-adapter` boundary.
- Explicit disabled/unavailable capability error mapping to HTTP `501 Not Implemented`.
- Extraction of supported upstream video result URL(s), secure retrieval, content-addressed caching via `media-store` (with MIME types such as `video/mp4`), and stable local URL or `b64_json` output.
- When supported, response object semantics fully compatible with image generation's `{created, data:[{url | b64_json}]}` pattern.

**Out of scope:**
- Audio generation, audio transcription, TTS, speech capabilities, or audio/video combined generation (explicitly excluded).
- Video editing, extension, frame extraction, or transformations.
- Background polling workers or async job-management APIs.
- Upstream wire protocol details, which remain in `gemini-adapter`.
- General media cache deduplication/TTL policy (→ `media-store`).
- HTTP server startup, general auth policy, or middleware (→ `http-server`, `middleware`).

---

## 2. Public API & Interfaces

### 2.1 Request and Response Models

```rust
use serde::{Deserialize, Serialize};

/// Request body for `POST /v1/videos/generations`.
#[derive(Debug, Clone, Deserialize)]
pub struct VideoGenerationRequest {
    /// Text description of the video to generate (required, non-empty after trimming).
    pub prompt: String,
    /// Optional requested duration in seconds (must be 1..=60 if provided).
    pub duration_seconds: Option<u32>,
    /// Output format: "url" (default) or "b64_json". Other values return HTTP 400.
    pub response_format: Option<String>,
}

/// OpenAI-shaped response returned after generation completes (matches image generation schema).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoGenerationResponse {
    /// Unix timestamp of result creation.
    pub created: i64,
    /// Generated video results.
    pub data: Vec<VideoResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoResult {
    /// Bridge-proxied local retrieval URL (`/v1/videos/{id}`), when response_format = "url".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Base64-encoded cached video bytes, when response_format = "b64_json".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
}
```

### 2.2 Error Types and HTTP Mapping

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VideoError {
    #[error("Video generation is disabled in bridge configuration")]
    Disabled,

    #[error("Video generation capability is not available upstream for this account")]
    NotImplemented,

    #[error("Invalid video generation request: {0}")]
    Validation(String),

    #[error("Video generation failed upstream: {0}")]
    UpstreamFailure(String),

    #[error("Video retrieval or caching failed: {0}")]
    MediaError(String),
}

/// `VideoError::Disabled` and `NotImplemented` map to HTTP 501.
/// Validation maps to 400; transient upstream failures map to the normal upstream
/// error mapping contract, never a panic or unhandled 500.
pub fn error_status(error: &VideoError) -> http::StatusCode;
```

### 2.3 Adapter Trait and Configuration

```rust
use async_trait::async_trait;

#[async_trait]
pub trait VideoAdapter: Send + Sync {
    /// Generate a video using the Gemini adapter, retrieve/cache bytes in media-store,
    /// and return a stable URL or b64_json according to the requested response format.
    async fn generate_video(
        &self,
        req: VideoGenerationRequest,
    ) -> Result<VideoGenerationResponse, VideoError>;
}

/// Configuration section for experimental video generation.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct VideoConfig {
    /// Experimental video generation is opt-in and disabled by default.
    #[serde(default)]
    pub enabled: bool,
}
```

---

## 3. Route Contract

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/v1/videos/generations` | Same API-key policy as `/v1/images/generations` | Generate video or return `501 Not Implemented` if disabled/unavailable |
| `GET` | `/v1/videos/{id}` | Same API-key policy as image retrieval | Retrieve cached video bytes with correct MIME type (e.g. `video/mp4`) |

When video generation is disabled, the generation endpoint remains registered and returns an OpenAI-style JSON error envelope with status 501 and a clear error code/message. The API does not become an unrecognized-path 404. Existing server auth policy applies equally to the route regardless of capability enablement.

---

## 4. Behavior & Invariants

1. **Off-by-Default Plugin Registration:**
   - `VideoConfig.enabled` defaults to `false` when omitted from `bridge.toml` or configuration profiles.
   - The plugin may be registered in the static registry but must not issue upstream requests while disabled.
   - The endpoint reports an explicit, stable 501 disabled response.

2. **Explicit Unavailable Capability Mapping:**
   - If the Gemini adapter reports no upstream video support, unsupported account tier, or unavailable generation pipeline, map to `VideoError::NotImplemented` and HTTP 501.
   - Missing capability is not a server fault; it must never become generic HTTP 500.
   - Error JSON is actionable and distinguishes `disabled` from `not_implemented`.

3. **Request Validation:**
   - Empty `prompt` (after whitespace trimming) → HTTP 400 / `VideoError::Validation`.
   - `duration_seconds` outside 1..=60 → HTTP 400 / `VideoError::Validation`.
   - `response_format` other than `"url"` or `"b64_json"` → HTTP 400 / `VideoError::Validation`.
   - Validation failures are deterministic and do not forward the request upstream.
4. **Supported Generation Response & Media Caching:**
   - When enabled and supported, extract the upstream result, retrieve the video bytes, and cache them through `MediaStore::put` with MIME type `video/mp4` (or `video/webm`).
   - `response_format = "url"` returns only a stable bridge URL (`/v1/videos/{id}`).
   - `response_format = "b64_json"` returns base64-encoded cached bytes.
   - Exactly one of `url` or `b64_json` is populated for each result.

5. **Media Retrieval:**
   - `GET /v1/videos/{id}` retrieves content bytes from `MediaStore::get` by opaque ID and returns the stored video MIME type.
   - Unknown IDs map to 404 through the shared API error contract.

6. **No Audio/TTS Expansion:**
   - This adapter covers video only. Audio generation, TTS, transcription, soundtracks, and audio/video combined capabilities are excluded and are not activated by the video feature flag.

7. **Upstream Boundary:**
   - Provider-specific framing and capability detection occur behind `gemini-adapter`; the video module consumes normalized adapter results and never builds `f.req` itself.

---

## 5. Acceptance Criteria & Traceability

### US-9 Traceability (Video & Experimental Capability)

| PRD US-9 Acceptance Criterion | Module Specification Coverage |
|---|---|
| Video adapter registered as experimental plugin, off-by-default | `VideoConfig.enabled` defaults to false; invariant 1 |
| If upstream lacks support, return clear 501 not 500 | `VideoError::NotImplemented`; explicit HTTP mapping; invariant 2 |
| If supported, return URL/base64 using same schema as image | `VideoGenerationResponse`/`VideoResult` mirrors image response; invariant 4 |

---

## 6. Testing Strategy

### 6.1 Unit Tests
- **Configuration Defaults:** Deserializing omitted `[video]` configuration yields `enabled = false`.
- **Disabled Adapter:** Calling the route/service while disabled returns HTTP 501 with error code `disabled`; mock upstream observes no generation request.
- **Unavailable Upstream:** Mock adapter returns unsupported capability → HTTP 501 with error code `not_implemented`, never 500.
- **Supported Generation:** Mock adapter returns a video source; verify media bytes are cached in `media-store` and URL response contains a stable local URL.
- **Base64 Response:** Verify `response_format = "b64_json"` yields decodable bytes identical to cached video content.
- **Retrieval:** Cached ID returns exact bytes and video MIME type; unknown ID maps to 404.
- **Response Shape:** URL and base64 modes each populate exactly one of `url`/`b64_json` and serialize using the image-generation-compatible `created`/`data` envelope.

### 6.2 Integration Tests
- `cargo test -p gemini-bridge-adapter-video` covers disabled, unavailable, and supported fixture cases.
- HTTP route test verifies API authentication is applied before service access and disabled behavior is returned as JSON 501.
- No live upstream dependency is required for deterministic tests; any real upstream acceptance is opt-in and credentials remain external to the repository.

---

## 7. Boundaries

- **Always:** Default to disabled; map disabled/unavailable capability to explicit HTTP 501; cache and serve supported video media through stable local IDs/URLs; preserve image-compatible response shape; keep provider wire details behind `gemini-adapter`.
- **Ask First:** Promoting video to default-enabled, adding background polling/job management, or supporting audio/video combined generation.
- **Never:** Return generic 500 for missing upstream capability; return raw upstream media URLs to clients; add audio/TTS scope; issue upstream generation requests while disabled.

---

## 8. Implementation Reference

- **Image Response Pattern:** `docs/specs/SPEC-image-gen.md` (OpenAI-shaped `created`/`data` URL or base64 output).
- **Media Storage Contract:** `docs/specs/SPEC-media-store.md` (content-addressed caching and retrieval).
- **Adapter Boundary:** `docs/specs/SPEC-gemini-adapter.md` (provider-specific wire protocol ownership).
- **Configuration Contract:** `docs/specs/SPEC-config.md` (TOML sections, typed config, defaults).
- **Task Implementation:** Task 3.2 (`crates/video-adapter/src/lib.rs`, `handler.rs`, configuration model, `tests/video_test.rs`).

---

## 9. Audio/TTS Scope Confirmation

This module specification is limited to video generation only. In accordance with PRD §2.3 and SPEC.md §10.8, audio generation, speech, and TTS capabilities are explicitly excluded from this repository's roadmap. No audio routes, parameters, data models, or feature flags are introduced.
