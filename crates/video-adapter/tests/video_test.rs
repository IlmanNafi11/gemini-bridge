//! Tests for Task 3.2 — experimental off-by-default video adapter.
//!
//! Spec: docs/specs/SPEC-video-adapter.md §6
//!
//! Tests exercise:
//! - Config default: `VideoConfig` omitted → `enabled = false`
//! - Disabled adapter returns `VideoError::Disabled`, never forwards upstream
//! - Unavailable upstream returns `VideoError::NotImplemented`, never 500
//! - Supported generation: bytes cached in media-store, URL result returned
//! - Base64 response: decodable bytes matching cached content
//! - Retrieval: cached ID → bytes + MIME; unknown ID → VideoError::MediaError
//! - Response shape: exactly one of `url`/`b64_json` per result, `created`/`data` envelope
//! - Request validation: empty prompt → Validation; invalid duration → Validation;
//!   invalid format → Validation
//! - HTTP status mapping: Disabled/NotImplemented → 501, Validation → 400

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use tempfile::TempDir;

use gemini_bridge_adapter_video::{
    DefaultVideoService, VideoAdapter, VideoConfig, VideoError, VideoGenerationRequest,
    VideoService, error_status,
};
use gemini_bridge_media_store::LocalMediaStore;

use gemini_bridge_upload::{
    MediaDownloader, MediaFetchPolicy, MediaKind, UploadError, fetch_media,
};
use url::Url;

struct TestLocalDownloader;

#[async_trait]
impl MediaDownloader for TestLocalDownloader {
    async fn download(&self, url: &Url, kind: MediaKind) -> Result<Bytes, UploadError> {
        fetch_media(url, kind, &MediaFetchPolicy::test_local()).await
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_request(prompt: &str) -> VideoGenerationRequest {
    VideoGenerationRequest {
        prompt: prompt.to_owned(),
        duration_seconds: None,
        response_format: None,
    }
}

/// Fake adapter that indicates video is supported and returns a fixed URL.
struct FakeVideoAdapter {
    video_url: String,
}

#[async_trait]
impl VideoAdapter for FakeVideoAdapter {
    async fn generate_video_raw(
        &self,
        _req: &VideoGenerationRequest,
    ) -> Result<String, VideoError> {
        Ok(self.video_url.clone())
    }
}

/// Fake adapter that reports video is unavailable upstream.
struct UnavailableAdapter;

#[async_trait]
impl VideoAdapter for UnavailableAdapter {
    async fn generate_video_raw(
        &self,
        _req: &VideoGenerationRequest,
    ) -> Result<String, VideoError> {
        Err(VideoError::NotImplemented)
    }
}

/// Minimal ftyp-box bytes to stand in for real MP4 video content.
fn test_video_bytes() -> Bytes {
    Bytes::from_static(b"\x00\x00\x00\x18ftypisom\x00\x00\x00\x00isommp42")
}

fn make_store(dir: &TempDir) -> LocalMediaStore {
    LocalMediaStore::new(dir.path())
}

// ── §6.1 Configuration defaults ───────────────────────────────────────────────

#[test]
fn video_config_default_disabled() {
    let cfg: VideoConfig = serde_json::from_str("{}").unwrap();
    assert!(
        !cfg.enabled,
        "VideoConfig must default to enabled = false when omitted from TOML"
    );
}

#[test]
fn video_config_explicit_enabled() {
    let cfg: VideoConfig = serde_json::from_str(r#"{"enabled": true}"#).unwrap();
    assert!(cfg.enabled);
}

// ── §6.1 Error HTTP status mapping ────────────────────────────────────────────

#[test]
fn disabled_error_maps_to_501() {
    assert_eq!(
        error_status(&VideoError::Disabled).as_u16(),
        501,
        "Disabled must map to HTTP 501"
    );
}

#[test]
fn not_implemented_error_maps_to_501() {
    assert_eq!(
        error_status(&VideoError::NotImplemented).as_u16(),
        501,
        "NotImplemented must map to HTTP 501"
    );
}

#[test]
fn validation_error_maps_to_400() {
    assert_eq!(
        error_status(&VideoError::Validation("bad".into())).as_u16(),
        400
    );
}

#[test]
fn upstream_failure_maps_to_502() {
    assert_eq!(
        error_status(&VideoError::UpstreamFailure("err".into())).as_u16(),
        502
    );
}

#[test]
fn media_error_maps_to_500() {
    assert_eq!(
        error_status(&VideoError::MediaError("err".into())).as_u16(),
        500
    );
}

// ── §6.1 Disabled adapter returns Disabled error without touching upstream ────

#[tokio::test]
async fn disabled_service_returns_disabled_error() {
    let dir = TempDir::new().unwrap();
    let config = VideoConfig { enabled: false };
    let store = make_store(&dir);
    let adapter = Arc::new(FakeVideoAdapter {
        video_url: "https://example.com/video.mp4".into(),
    });
    let service = DefaultVideoService::new(config, adapter, store);

    let err = service
        .generate(make_request("make a cat video"))
        .await
        .unwrap_err();

    assert!(
        matches!(err, VideoError::Disabled),
        "Disabled service must return VideoError::Disabled, got: {err:?}"
    );
}

// ── §6.1 Unavailable upstream returns NotImplemented (never 500) ──────────────

#[tokio::test]
async fn unavailable_upstream_returns_not_implemented() {
    let dir = TempDir::new().unwrap();
    let config = VideoConfig { enabled: true };
    let store = make_store(&dir);
    let adapter = Arc::new(UnavailableAdapter);
    let service = DefaultVideoService::new(config, adapter, store);

    let err = service
        .generate(make_request("test video"))
        .await
        .unwrap_err();

    assert!(
        matches!(err, VideoError::NotImplemented),
        "Unavailable upstream must yield VideoError::NotImplemented, got: {err:?}"
    );
}

// ── §6.1 Request validation ───────────────────────────────────────────────────

#[tokio::test]
async fn empty_prompt_returns_validation_error() {
    let dir = TempDir::new().unwrap();
    let config = VideoConfig { enabled: true };
    let store = make_store(&dir);
    let adapter = Arc::new(FakeVideoAdapter {
        video_url: "https://example.com/video.mp4".into(),
    });
    let service = DefaultVideoService::new(config, adapter, store);

    let err = service
        .generate(VideoGenerationRequest {
            prompt: "   ".into(),
            duration_seconds: None,
            response_format: None,
        })
        .await
        .unwrap_err();

    assert!(
        matches!(err, VideoError::Validation(_)),
        "Empty prompt must return Validation error, got: {err:?}"
    );
}

#[tokio::test]
async fn duration_zero_returns_validation_error() {
    let dir = TempDir::new().unwrap();
    let config = VideoConfig { enabled: true };
    let store = make_store(&dir);
    let adapter = Arc::new(FakeVideoAdapter {
        video_url: "https://example.com/video.mp4".into(),
    });
    let service = DefaultVideoService::new(config, adapter, store);

    let err = service
        .generate(VideoGenerationRequest {
            prompt: "test".into(),
            duration_seconds: Some(0),
            response_format: None,
        })
        .await
        .unwrap_err();

    assert!(
        matches!(err, VideoError::Validation(_)),
        "duration_seconds = 0 must return Validation, got: {err:?}"
    );
}

#[tokio::test]
async fn duration_over_60_returns_validation_error() {
    let dir = TempDir::new().unwrap();
    let config = VideoConfig { enabled: true };
    let store = make_store(&dir);
    let adapter = Arc::new(FakeVideoAdapter {
        video_url: "https://example.com/video.mp4".into(),
    });
    let service = DefaultVideoService::new(config, adapter, store);

    let err = service
        .generate(VideoGenerationRequest {
            prompt: "test".into(),
            duration_seconds: Some(61),
            response_format: None,
        })
        .await
        .unwrap_err();

    assert!(
        matches!(err, VideoError::Validation(_)),
        "duration_seconds = 61 must return Validation, got: {err:?}"
    );
}

#[tokio::test]
async fn invalid_response_format_returns_validation_error() {
    let dir = TempDir::new().unwrap();
    let config = VideoConfig { enabled: true };
    let store = make_store(&dir);
    let adapter = Arc::new(FakeVideoAdapter {
        video_url: "https://example.com/video.mp4".into(),
    });
    let service = DefaultVideoService::new(config, adapter, store);

    let err = service
        .generate(VideoGenerationRequest {
            prompt: "test".into(),
            duration_seconds: None,
            response_format: Some("mp4".into()),
        })
        .await
        .unwrap_err();

    assert!(
        matches!(err, VideoError::Validation(_)),
        "Unknown response_format must return Validation, got: {err:?}"
    );
}

// ── §6.1 Supported generation — URL response shape ────────────────────────────

#[tokio::test]
async fn supported_generation_url_response_shape() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let video_url = format!("{}/clip.mp4", server.uri());

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(test_video_bytes().as_ref())
                .insert_header("content-type", "video/mp4"),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let config = VideoConfig { enabled: true };
    let store = make_store(&dir);
    let adapter = Arc::new(FakeVideoAdapter { video_url });
    let service =
        DefaultVideoService::with_downloader(config, adapter, store, Arc::new(TestLocalDownloader));

    let resp = service.generate(make_request("cat video")).await.unwrap();

    assert!(
        !resp.data.is_empty(),
        "Response must have at least one result"
    );
    assert!(resp.created > 0, "created timestamp must be positive");

    let result = &resp.data[0];
    assert!(
        result.url.is_some(),
        "URL format: url field must be populated"
    );
    assert!(
        result.b64_json.is_none(),
        "URL format: b64_json must be absent"
    );

    let url = result.url.as_ref().unwrap();
    assert!(
        url.starts_with("/v1/videos/"),
        "Result URL must be a bridge-local path, got: {url}"
    );
}

// ── §6.1 Supported generation — b64_json response ────────────────────────────

#[tokio::test]
async fn supported_generation_b64_response_decodable() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let expected_bytes = test_video_bytes();
    let video_url = format!("{}/clip.mp4", server.uri());

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(expected_bytes.as_ref())
                .insert_header("content-type", "video/mp4"),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let config = VideoConfig { enabled: true };
    let store = make_store(&dir);
    let adapter = Arc::new(FakeVideoAdapter { video_url });
    let service =
        DefaultVideoService::with_downloader(config, adapter, store, Arc::new(TestLocalDownloader));

    let req = VideoGenerationRequest {
        prompt: "cat video".into(),
        duration_seconds: None,
        response_format: Some("b64_json".into()),
    };
    let resp = service.generate(req).await.unwrap();

    let result = &resp.data[0];
    assert!(result.b64_json.is_some(), "b64_json must be present");
    assert!(result.url.is_none(), "url must be absent in b64_json mode");

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(result.b64_json.as_ref().unwrap())
        .expect("b64_json must be valid base64");
    assert_eq!(
        decoded,
        expected_bytes.as_ref(),
        "Decoded bytes must match what the upstream served"
    );
}

// ── §6.1 Media retrieval ──────────────────────────────────────────────────────

#[tokio::test]
async fn get_video_returns_bytes_and_mime() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let expected_bytes = test_video_bytes();
    let video_url = format!("{}/clip.mp4", server.uri());

    // Two requests: one for generate, one for get (same mock path, mount twice or use AnyTimes).
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(expected_bytes.as_ref())
                .insert_header("content-type", "video/mp4"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().unwrap();
    let config = VideoConfig { enabled: true };
    let store = make_store(&dir);
    let adapter = Arc::new(FakeVideoAdapter {
        video_url: video_url.clone(),
    });
    let service =
        DefaultVideoService::with_downloader(config, adapter, store, Arc::new(TestLocalDownloader));

    // First: generate and cache the video.
    let resp = service.generate(make_request("cat video")).await.unwrap();
    let local_path = resp.data[0].url.as_ref().unwrap();
    let id = local_path.trim_start_matches("/v1/videos/");

    // Then: retrieve by the local ID.
    let (bytes, mime) = service.get_video(id).await.unwrap();
    assert_eq!(
        bytes.as_ref(),
        expected_bytes.as_ref(),
        "Retrieved bytes must match generated bytes"
    );
    assert!(
        mime.contains("video"),
        "MIME type should be a video type, got: {mime}"
    );
}

#[tokio::test]
async fn get_video_unknown_id_returns_media_error() {
    let dir = TempDir::new().unwrap();
    let config = VideoConfig { enabled: true };
    let store = make_store(&dir);
    let adapter = Arc::new(FakeVideoAdapter {
        video_url: "https://example.com/video.mp4".into(),
    });
    let service = DefaultVideoService::new(config, adapter, store);

    let err = service.get_video("does-not-exist").await.unwrap_err();
    assert!(
        matches!(err, VideoError::MediaError(_)),
        "Unknown ID must yield VideoError::MediaError, got: {err:?}"
    );
}
