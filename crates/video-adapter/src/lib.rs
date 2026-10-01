//! Experimental off-by-default video generation adapter.
//!
//! Implements `POST /v1/videos/generations` and `GET /v1/videos/{id}`.
//!
//! **Disabled by default.** Enable explicitly in `bridge.toml`:
//! ```toml
//! [video]
//! enabled = true
//! ```
//!
//! When disabled, or when the upstream account does not support video generation,
//! the endpoint returns HTTP `501 Not Implemented` with an actionable JSON error
//! body — never a generic `500 Internal Server Error`.
//!
//! Spec: `docs/specs/SPEC-video-adapter.md`

pub mod handler;

use async_trait::async_trait;
use bytes::Bytes;
pub use handler::DefaultVideoService;
use http::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use gemini_bridge_config::VideoConfig;

// ── Public request / response types ──────────────────────────────────────────

/// Request body for `POST /v1/videos/generations`.
#[derive(Debug, Clone, Deserialize)]
pub struct VideoGenerationRequest {
    /// Text description of the video to generate (required, non-empty after trimming).
    pub prompt: String,
    /// Optional requested duration in seconds (must be 1..=60 if provided).
    pub duration_seconds: Option<u32>,
    /// Output format: `"url"` (default) or `"b64_json"`. Other values return HTTP 400.
    pub response_format: Option<String>,
}

/// OpenAI-shaped response returned after generation completes.
///
/// Matches the image generation schema (`created` + `data`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoGenerationResponse {
    /// Unix timestamp of result creation.
    pub created: i64,
    /// Generated video results.
    pub data: Vec<VideoResult>,
}

/// A single generated video result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoResult {
    /// Bridge-proxied local retrieval URL (`/v1/videos/{id}`).
    ///
    /// Present when `response_format = "url"` (the default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Base64-encoded cached video bytes.
    ///
    /// Present when `response_format = "b64_json"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
}

// ── Error types ───────────────────────────────────────────────────────────────

/// Errors produced by the video generation pipeline.
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

/// Map a [`VideoError`] to the appropriate HTTP status code.
///
/// - [`VideoError::Disabled`] and [`VideoError::NotImplemented`] → `501 Not Implemented`
/// - [`VideoError::Validation`] → `400 Bad Request`
/// - [`VideoError::UpstreamFailure`] → `502 Bad Gateway`
/// - [`VideoError::MediaError`] → `500 Internal Server Error`
pub fn error_status(error: &VideoError) -> StatusCode {
    match error {
        VideoError::Disabled | VideoError::NotImplemented => StatusCode::NOT_IMPLEMENTED,
        VideoError::Validation(_) => StatusCode::BAD_REQUEST,
        VideoError::UpstreamFailure(_) => StatusCode::BAD_GATEWAY,
        VideoError::MediaError(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Stable error code string for JSON responses.
pub fn error_code(error: &VideoError) -> &'static str {
    match error {
        VideoError::Disabled => "video_generation_disabled",
        VideoError::NotImplemented => "video_generation_not_implemented",
        VideoError::Validation(_) => "validation_error",
        VideoError::UpstreamFailure(_) => "upstream_failure",
        VideoError::MediaError(_) => "storage_error",
    }
}

// ── Adapter trait ─────────────────────────────────────────────────────────────

/// Low-level adapter that bridges to the upstream video generation capability.
///
/// The concrete implementation lives behind `gemini-adapter`; tests supply
/// fakes.  The adapter returns a raw video URL that the service downloads and
/// caches.
///
/// Returning `Err(VideoError::NotImplemented)` signals that the upstream account
/// or pipeline does not support video; the caller maps this to HTTP 501.
#[async_trait]
pub trait VideoAdapter: Send + Sync {
    /// Trigger upstream video generation and return the raw content URL.
    ///
    /// Return `Err(VideoError::NotImplemented)` when the upstream pipeline is
    /// unavailable rather than panicking or returning an unrelated error.
    async fn generate_video_raw(&self, req: &VideoGenerationRequest) -> Result<String, VideoError>;
}

// ── Service trait ─────────────────────────────────────────────────────────────

/// High-level video service used by the HTTP route handlers.
///
/// Coordinates validation, the upstream adapter, media caching, and format
/// selection.
#[async_trait]
pub trait VideoService: Send + Sync {
    /// Validate the request, dispatch to the adapter, cache the result in
    /// `media-store`, and return a stable URL or base64 response.
    async fn generate(
        &self,
        req: VideoGenerationRequest,
    ) -> Result<VideoGenerationResponse, VideoError>;

    /// Retrieve a previously cached video by its local opaque `id`.
    ///
    /// Returns `(content_bytes, mime_type)`.
    async fn get_video(&self, id: &str) -> Result<(Bytes, String), VideoError>;
}
