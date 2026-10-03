# Module Specification: `video-adapter`

**Module ID:** `video-adapter`
**Crate:** `gemini-bridge-adapter-video` (`crates/video-adapter`)
**Phase:** Fase 3 (contract-only; production generation not shipped)
**Depends On:** `gemini-adapter` and `media-store` are intended future implementation boundaries; no production adapter is wired. `config` rejects `video.enabled = true`.
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-9, §4.9
**Status:** Contract reference only; no shipped production video-generation path

---

## 1. Objective & Responsibility

This document records the shape reserved for a possible future Gemini Web video-generation adapter. Production video generation is **not shipped**: the application binary does not wire a production adapter, and setting `[video].enabled = true` is rejected during configuration loading. The registered video HTTP routes are contract-only and return HTTP 501; they do not dispatch generation or retrieve video media.

**Contract-only scope:**
- Preserve the planned request/response and error shapes for future review; they are not a promise of currently usable generation.
- Keep `POST /v1/videos/generations` and `GET /v1/videos/{id}` registered under the same configured API-key policy as other `/v1/*` routes, returning JSON 501 while production service wiring is absent.
- Reject the unsupported `video.enabled = true` configuration rather than silently accepting a no-op flag.

**Not shipped:**
- Video generation, capability detection against Gemini, video URL extraction/retrieval/caching, and video retrieval by ID.
- Video plugin registration or runtime enablement.

Audio/TTS, video editing, transformations, background polling/job management, and general HTTP/server lifecycle remain outside this contract.

---

## 2. Reserved API Shapes

The models and errors below describe a potential future contract only. They do not imply that the current binary generates video or that `VideoConfig.enabled` is an accepted production switch.

### 2.1 Request and Response Models

```rust
use serde::{Deserialize, Serialize};

/// Reserved request body for `POST /v1/videos/generations`.
#[derive(Debug, Clone, Deserialize)]
pub struct VideoGenerationRequest {
    pub prompt: String,
    pub duration_seconds: Option<u32>,
    pub response_format: Option<String>,
}

/// Reserved OpenAI-shaped response; no production generator returns this today.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoGenerationResponse {
    pub created: i64,
    pub data: Vec<VideoResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoResult {
    pub url: Option<String>,
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

/// Reserved future mapping; the current unconfigured route returns `Disabled` / HTTP 501.
pub fn error_status(error: &VideoError) -> http::StatusCode;
```

### 2.3 Reserved Adapter and Configuration Shapes

The trait and config types below are design references, not wired production APIs. In the current binary, video configuration is parsed for compatibility but `enabled = true` is rejected during validation.

---

## 3. Route Contract

| Method | Path | Auth | Current behavior |
|---|---|---|---|
| `POST` | `/v1/videos/generations` | Same API-key policy as other public API routes | Registered contract-only route; JSON HTTP 501 (`disabled`) because no production service is wired |
| `GET` | `/v1/videos/{id}` | Same API-key policy as other public API routes | Registered contract-only route; JSON HTTP 501 (`disabled`) because no production service is wired |

The route registration reserves paths and a stable disabled error only. It neither calls Gemini nor reads/writes video media. `[video].enabled = true` is a configuration error, not an opt-in path.

---

## 4. Behavior & Invariants

1. The application binary does not register or instantiate a production video adapter.
2. Video routes remain registered and return JSON HTTP 501 with code `disabled` while the service is absent.
3. Configuration loading rejects `video.enabled = true` with an explicit unsupported-production-adapter validation error.
4. The request/response models, future unsupported-capability mapping, URL/base64 schema, validation rules, and media-store integration below are reserved design only; they are not current executable behavior.
5. No video generation request reaches Gemini and no video is cached/retrieved by this route.
6. Audio/TTS remains outside scope.

---

## 5. Future Design Acceptance (Not Current Release Acceptance)

If production video is approved and implemented later, the API may use image-like `{created, data:[{url | b64_json}]}` responses, validate prompt/duration/format, map unsupported upstream capability to 501, and cache returned bytes through `media-store`. Until that implementation and its focused tests exist, these criteria are **not complete** and no supported-generation behavior is claimed.

Current deterministic contract checks are limited to: enabled config rejection; route registration/auth behavior; and disabled JSON 501 response. Live video compatibility is not established.

---

## 6. Boundaries

- **Always:** Keep production video explicitly unsupported; reject enablement; preserve the JSON 501 route contract; keep provider wire details behind `gemini-adapter` if future implementation is approved.
- **Ask First:** Shipping video generation, enabling it in production, adding background polling/job management, or supporting combined audio/video.
- **Never:** Claim that production video is shipped, accept an ignored `video.enabled` flag, or present reserved response models as proof of generation/retrieval behavior.

---

## 7. Implementation Reference

- **Configuration Contract:** `docs/specs/SPEC-config.md` and `crates/config/src/loader.rs` reject `video.enabled = true`.
- **Current Route:** `crates/http-server/src/handlers/videos.rs` emits JSON 501 while no service is wired.
- **Current production wiring:** `src/main.rs` does not construct a production video service.
- **Task:** Task 3.2 is re-scoped as deterministic disabled-route/configuration contract coverage, not video-generation implementation.
