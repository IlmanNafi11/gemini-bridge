//! End-to-end image generation and retrieval via the HTTP server.

use std::net::TcpListener;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use bytes::Bytes;
use futures::{Stream, stream};
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_image_gen::{
    DefaultImageGenService, ImageGenService, ImageGenerationRequest, ImageInput,
};
use gemini_bridge_llm_service::{
    Completion, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
};
use gemini_bridge_media_store::LocalMediaStore;
use gemini_bridge_upload::push_client::PushUploadClient;
use gemini_bridge_upload::{UploadError, UploadLimits, UploadServiceImpl};
use serde_json::json;
use tokio::task::JoinHandle;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\ngenerated image content";

struct NoopPush;

#[async_trait]
impl PushUploadClient for NoopPush {
    async fn initiate_session(
        &self,
        _mime_type: &str,
        _size_bytes: u64,
    ) -> Result<String, UploadError> {
        Ok("https://uploads.example/session".to_owned())
    }

    async fn upload_bytes(&self, _session_url: &str, _bytes: Bytes) -> Result<String, UploadError> {
        Ok("files/reference-1".to_owned())
    }
}

struct ImageAdapter {
    image_url: String,
    last_request: Mutex<Option<Arc<LlmRequest>>>,
}

#[async_trait]
impl LlmAdapter for ImageAdapter {
    fn provider_id(&self) -> &'static str {
        "test-image"
    }

    async fn complete(&self, request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        *self.last_request.lock().unwrap() = Some(request);
        Ok(Completion {
            text: format!("Generated image: {}", self.image_url),
            finish_reason: "stop".to_owned(),
            usage: None,
        })
    }

    async fn complete_raw(&self, request: Arc<LlmRequest>) -> Result<serde_json::Value, LlmError> {
        *self.last_request.lock().unwrap() = Some(request);
        Ok(json!({
            "candidates": [{
                "parts": [{
                    "text": format!("Generated image: {}", self.image_url)
                }]
            }]
        }))
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        let empty: Pin<Box<dyn Stream<Item = Result<LlmEvent, LlmError>> + Send>> =
            Box::pin(stream::empty());
        Ok(empty)
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn spawn_server(router: axum::Router, port: u16) -> JoinHandle<()> {
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    })
}

#[tokio::test]
async fn generated_image_is_cached_and_retrievable_by_proxy_url() {
    let image_origin = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/generated/googleusercontent.com/image.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(PNG))
        .mount(&image_origin)
        .await;

    let adapter = Arc::new(ImageAdapter {
        image_url: format!(
            "{}/generated/googleusercontent.com/image.png",
            image_origin.uri()
        ),
        last_request: Mutex::new(None),
    });
    let temp = tempfile::TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let upload = Arc::new(UploadServiceImpl::new(
        store.clone(),
        Arc::new(NoopPush),
        UploadLimits::default(),
    ));
    let image_service: Arc<dyn ImageGenService> = Arc::new(DefaultImageGenService::new(
        adapter.clone(),
        store,
        upload.clone(),
        "gemini-web-flash",
    ));

    let port = free_port();
    let router = build_router(
        ServerConfig {
            bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
            api_key: None,
            require_key_for_admin: false,
            cors_enabled: false,
            rate_limit: None,
        },
        AppState {
            adapter: adapter.clone(),
            upload_service: Some(upload),
            image_service: Some(image_service),
            health_admin: gemini_bridge_http_server::build_health_admin(None),
        },
    );
    let server = spawn_server(router, port).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/images/generations"))
        .json(&json!({
            "prompt": "A tiny red fox",
            "response_format": "url"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    let proxy_path = body["data"][0]["url"].as_str().unwrap();
    assert!(proxy_path.starts_with("/v1/images/"));
    assert!(!body.to_string().contains("googleusercontent.com"));

    let retrieved = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}{proxy_path}"))
        .send()
        .await
        .unwrap();
    assert_eq!(retrieved.status(), reqwest::StatusCode::OK);
    assert_eq!(retrieved.headers()["content-type"], "image/png");
    assert_eq!(retrieved.bytes().await.unwrap(), Bytes::from_static(PNG));

    let recorded = adapter.last_request.lock().unwrap();
    let request = recorded.as_ref().expect("adapter request recorded");
    assert_eq!(request.model.model, "gemini-web-flash");

    server.abort();
}

#[tokio::test]
async fn b64_response_decodes_to_generated_image_bytes() {
    let image_origin = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/generated/googleusercontent.com/image.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(PNG))
        .mount(&image_origin)
        .await;

    let adapter = Arc::new(ImageAdapter {
        image_url: format!(
            "{}/generated/googleusercontent.com/image.png",
            image_origin.uri()
        ),
        last_request: Mutex::new(None),
    });
    let temp = tempfile::TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let upload = Arc::new(UploadServiceImpl::new(
        store.clone(),
        Arc::new(NoopPush),
        UploadLimits::default(),
    ));
    let service: Arc<dyn ImageGenService> = Arc::new(DefaultImageGenService::new(
        adapter.clone(),
        store,
        upload.clone(),
        "gemini-web-flash",
    ));
    let port = free_port();
    let router = build_router(
        ServerConfig {
            bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
            api_key: None,
            require_key_for_admin: false,
            cors_enabled: false,
            rate_limit: None,
        },
        AppState {
            adapter,
            upload_service: Some(upload),
            image_service: Some(service),
            health_admin: gemini_bridge_http_server::build_health_admin(None),
        },
    );
    let server = spawn_server(router, port).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/images/generations"))
        .json(&json!({
            "prompt": "A blue square",
            "response_format": "b64_json"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["data"][0].get("url").is_none());
    let encoded = body["data"][0]["b64_json"].as_str().unwrap();
    assert_eq!(B64.decode(encoded).unwrap(), PNG);

    server.abort();
}

#[tokio::test]
async fn missing_image_id_returns_not_found() {
    let adapter = Arc::new(ImageAdapter {
        image_url: "https://lh3.googleusercontent.com/unused.png".to_owned(),
        last_request: Mutex::new(None),
    });
    let temp = tempfile::TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let upload = Arc::new(UploadServiceImpl::new(
        store.clone(),
        Arc::new(NoopPush),
        UploadLimits::default(),
    ));
    let service: Arc<dyn ImageGenService> = Arc::new(DefaultImageGenService::new(
        adapter.clone(),
        store,
        upload.clone(),
        "gemini-web-flash",
    ));
    let port = free_port();
    let router = build_router(
        ServerConfig {
            bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
            api_key: None,
            require_key_for_admin: false,
            cors_enabled: false,
            rate_limit: None,
        },
        AppState {
            adapter,
            upload_service: Some(upload),
            image_service: Some(service),
            health_admin: gemini_bridge_http_server::build_health_admin(None),
        },
    );
    let server = spawn_server(router, port).await;

    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/v1/images/missing"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    server.abort();
}

#[test]
fn request_deserializes_optional_references() {
    let request: ImageGenerationRequest = serde_json::from_value(json!({
        "prompt": "variation",
        "reference_images": [
            "https://example.com/reference.png",
            { "b64_json": B64.encode(PNG), "mime_type": "image/png" }
        ]
    }))
    .unwrap();

    let references = request.reference_images.unwrap();
    assert_eq!(references.len(), 2);
    assert!(matches!(&references[0], ImageInput::Url(value) if value.starts_with("https://")));
}
