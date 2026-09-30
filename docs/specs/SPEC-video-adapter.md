# Module Specification: `video-adapter`

**Module ID:** `video-adapter`  
**Crate:** `gemini-bridge-adapter-video` (`crates/video-adapter`)  
**Phase:** Fase 3 (Experimental)  
**Depends On:** `gemini-adapter`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-9  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Provide an experimental adapter for Gemini video generation. When enabled via configuration and supported by upstream, handle video generation requests and proxy/cache resulting video URLs using the media-store. When upstream video capability is unavailable or unconfigured, return an explicit, actionable `501 Not Implemented` error response rather than a generic 500 error.

---

## 2. Public API & Interfaces

```rust
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VideoError {
    #[error("Video generation capability is not available upstream")]
    NotImplemented,
    #[error("Video generation is disabled in bridge configuration")]
    Disabled,
    #[error("Upstream generation failed: {0}")]
    UpstreamFailure(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct VideoGenerationRequest {
    pub prompt: String,
    pub duration_seconds: Option<u32>,
    pub response_format: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoGenerationResponse {
    pub created: i64,
    pub data: Vec<VideoResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoResult {
    pub url: Option<String>,
    pub b64_json: Option<String>,
}

#[async_trait::async_trait]
pub trait VideoAdapter: Send + Sync {
    async fn generate_video(&self, req: VideoGenerationRequest) -> Result<VideoGenerationResponse, VideoError>;
}
```

---

## 3. Behavior & Invariants

1. **Off-by-Default:** The video adapter is disabled by default in `bridge.toml`. Requests to `/v1/videos/generations` when disabled return `501 Not Implemented` with a clear explanatory error message.
2. **Explicit 501:** If upstream does not support the requested video pipeline for the active account tier, the adapter returns `501 Not Implemented`, never `500 Internal Server Error`.
3. **Caching:** Successfully generated video content is downloaded and cached in `media-store` by SHA-256 hash and served via a bridge-proxied URL to prevent upstream expiry.

---

## 4. Testing Strategy

- Disabled state test verifying immediate `501 Not Implemented`.
- Mock upstream unsupported test verifying clean 501 conversion.
- Mock upstream success test verifying video URL extraction, caching, and response formatting.

---

## 5. Boundaries

- **Always:** Default to disabled; return explicit 501 on unsupported capability; proxy media through local bridge.
- **Ask First:** Promoting video adapter to default-enabled or introducing background polling workers.
- **Never:** Return unhandled 500 errors for missing upstream capabilities.
