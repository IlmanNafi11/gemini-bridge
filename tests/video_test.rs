//! End-to-end tests for the experimental video HTTP surface.
//!
//! Acceptance:
//! - Disabled by default; unavailable upstream returns clear 501 not 500;
//!   when available, output follows image URL/b64 schema.

use std::net::TcpListener;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use futures::{Stream, stream};
use gemini_bridge_adapter_video::{
    DefaultVideoService, VideoAdapter, VideoConfig, VideoError, VideoGenerationRequest,
};
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_llm_service::{
    Completion, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
};
use gemini_bridge_media_store::LocalMediaStore;
use gemini_bridge_upload::{MediaDownloader, MediaKind, UploadError};
use reqwest::Url;
use serde_json::Value;
use tempfile::TempDir;

struct NoopAdapter;

#[async_trait]
impl LlmAdapter for NoopAdapter {
    fn provider_id(&self) -> &'static str {
        "noop"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Err(LlmError::Unavailable)
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        let empty: Pin<Box<dyn Stream<Item = Result<LlmEvent, LlmError>> + Send>> =
            Box::pin(stream::empty());
        Ok(empty)
    }
}

struct MockVideoAdapter {
    url: String,
}

#[async_trait]
impl VideoAdapter for MockVideoAdapter {
    async fn generate_video_raw(
        &self,
        _req: &VideoGenerationRequest,
    ) -> Result<String, VideoError> {
        Ok(self.url.clone())
    }
}

struct FixedVideoDownloader;

#[async_trait]
impl MediaDownloader for FixedVideoDownloader {
    async fn download(&self, _url: &Url, kind: MediaKind) -> Result<Bytes, UploadError> {
        assert_eq!(kind, MediaKind::Video);
        Ok(test_video_bytes())
    }
}

struct UnavailableVideoAdapter;

#[async_trait]
impl VideoAdapter for UnavailableVideoAdapter {
    async fn generate_video_raw(
        &self,
        _req: &VideoGenerationRequest,
    ) -> Result<String, VideoError> {
        Err(VideoError::NotImplemented)
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn test_video_bytes() -> Bytes {
    Bytes::from_static(b"\x00\x00\x00\x18ftypisom\x00\x00\x00\x00isommp42")
}

async fn spawn_server(
    video_service: Option<Arc<dyn gemini_bridge_adapter_video::VideoService>>,
) -> (String, u16) {
    let port = free_port();
    let state = AppState {
        adapter: Arc::new(NoopAdapter),
        upload_service: None,
        image_service: None,
        video_service,
        health_admin: gemini_bridge_http_server::build_health_admin(None),
        identity_service: None,
        conversation_store: None,
        tool_engine: gemini_bridge_http_server::build_tool_engine(),
        gallery_service: None,
        media_purge: None,
    };
    let router = build_router(
        ServerConfig {
            bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
            api_key: None,
            require_key_for_admin: false,
            cors_enabled: false,
            rate_limit: None,
            metrics_enabled: false,
        },
        state,
    );
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://127.0.0.1:{port}"), port)
}

#[tokio::test]
async fn disabled_video_route_is_registered_and_returns_actionable_501() {
    let (base, _) = spawn_server(None).await;
    let response = reqwest::Client::new()
        .post(format!("{base}/v1/videos/generations"))
        .json(&serde_json::json!({"prompt": "a comet over the ocean"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::NOT_IMPLEMENTED);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "video_generation_disabled");
}

#[tokio::test]
async fn unavailable_upstream_returns_clear_501_not_500() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let service = Arc::new(DefaultVideoService::new(
        VideoConfig { enabled: true },
        Arc::new(UnavailableVideoAdapter),
        store,
    ));

    let (base, _) = spawn_server(Some(service)).await;
    let response = reqwest::Client::new()
        .post(format!("{base}/v1/videos/generations"))
        .json(&serde_json::json!({"prompt": "render a cinematic video"}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        reqwest::StatusCode::NOT_IMPLEMENTED,
        "Unavailable upstream must return 501 Not Implemented, never 500"
    );
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "video_generation_not_implemented");
}

#[tokio::test]
async fn validation_error_returns_400() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let service = Arc::new(DefaultVideoService::new(
        VideoConfig { enabled: true },
        Arc::new(MockVideoAdapter {
            url: "https://example.com/test.mp4".into(),
        }),
        store,
    ));

    let (base, _) = spawn_server(Some(service)).await;

    // Empty prompt → 400
    let response = reqwest::Client::new()
        .post(format!("{base}/v1/videos/generations"))
        .json(&serde_json::json!({"prompt": "   "}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

    // Invalid duration → 400
    let response = reqwest::Client::new()
        .post(format!("{base}/v1/videos/generations"))
        .json(&serde_json::json!({
            "prompt": "flying eagle",
            "duration_seconds": 120
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

    // Invalid format → 400
    let response = reqwest::Client::new()
        .post(format!("{base}/v1/videos/generations"))
        .json(&serde_json::json!({
            "prompt": "flying eagle",
            "response_format": "gif"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn supported_generation_returns_openai_image_schema() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let service = Arc::new(DefaultVideoService::with_downloader(
        VideoConfig { enabled: true },
        Arc::new(MockVideoAdapter {
            url: "https://video.example/clip.mp4".into(),
        }),
        store,
        Arc::new(FixedVideoDownloader),
    ));

    let (base, _) = spawn_server(Some(service)).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/v1/videos/generations"))
        .json(&serde_json::json!({
            "prompt": "timelapse of sunset over mountains",
            "duration_seconds": 10,
            "response_format": "url"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.unwrap();

    assert!(body["created"].as_i64().unwrap() > 0);
    let data = body["data"].as_array().expect("data array");
    assert_eq!(data.len(), 1);

    let video_url = data[0]["url"].as_str().expect("url string");
    assert!(video_url.starts_with("/v1/videos/"));
    assert!(data[0]["b64_json"].is_null());

    // Test GET /v1/videos/{id} retrieval
    let get_resp = reqwest::Client::new()
        .get(format!("{base}{video_url}"))
        .send()
        .await
        .unwrap();

    assert_eq!(get_resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        get_resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "video/mp4"
    );
    let retrieved_bytes = get_resp.bytes().await.unwrap();
    assert_eq!(retrieved_bytes, test_video_bytes());
}

#[tokio::test]
async fn b64_json_response_format_returns_valid_base64() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let service = Arc::new(DefaultVideoService::with_downloader(
        VideoConfig { enabled: true },
        Arc::new(MockVideoAdapter {
            url: "https://video.example/clip.mp4".into(),
        }),
        store,
        Arc::new(FixedVideoDownloader),
    ));

    let (base, _) = spawn_server(Some(service)).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/v1/videos/generations"))
        .json(&serde_json::json!({
            "prompt": "northern lights aurora",
            "response_format": "b64_json"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.unwrap();

    let data = body["data"].as_array().expect("data array");
    let b64 = data[0]["b64_json"].as_str().expect("b64_json string");
    assert!(data[0]["url"].is_null());

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("valid base64");
    assert_eq!(decoded, test_video_bytes());
}

#[tokio::test]
async fn get_video_unknown_id_returns_404() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let service = Arc::new(DefaultVideoService::new(
        VideoConfig { enabled: true },
        Arc::new(MockVideoAdapter {
            url: "https://example.com/mock.mp4".into(),
        }),
        store,
    ));

    let (base, _) = spawn_server(Some(service)).await;

    let get_resp = reqwest::Client::new()
        .get(format!("{base}/v1/videos/nonexistent-id"))
        .send()
        .await
        .unwrap();

    assert_eq!(get_resp.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = get_resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "resource_not_found");
}
