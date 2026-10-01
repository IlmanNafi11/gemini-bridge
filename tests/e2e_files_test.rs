//! End-to-end file upload and retrieval via the HTTP server.

use async_trait::async_trait;
use bytes::Bytes;
use gemini_bridge_config::{
    BridgeConfig, ServerConfig as BridgeSrvCfg, StorageConfig, TransportConfig,
};
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_identity::DefaultIdentityService;
use gemini_bridge_media_store::LocalMediaStore;
use gemini_bridge_upload::push_client::PushUploadClient;
use gemini_bridge_upload::{UploadError, UploadLimits, UploadServiceImpl};
use serde_json::json;
use std::{net::TcpListener, sync::Arc};
use tokio::task::JoinHandle;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfile content";

struct RecordingPush;

#[async_trait]
impl PushUploadClient for RecordingPush {
    async fn initiate_session(
        &self,
        _mime_type: &str,
        _size_bytes: u64,
    ) -> Result<String, UploadError> {
        Ok("https://uploads.example/session".to_string())
    }
    async fn upload_bytes(&self, _session_url: &str, _bytes: Bytes) -> Result<String, UploadError> {
        Ok("files/test-ref".to_string())
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
async fn multipart_upload_returns_id_and_get_returns_same_bytes() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(")]}\n{}"))
        .mount(&upstream)
        .await;

    let port = free_port();
    let temp = tempfile::TempDir::new().unwrap();

    let fake_cookies = json!({
        "psid": "fake-psid",
        "psidts": "fake-psidts",
        "sapisid": "fake-sapisid",
        "imported_at": "2026-01-01T00:00:00Z"
    });
    std::fs::write(
        temp.path().join("cookies.json"),
        serde_json::to_string(&fake_cookies).unwrap(),
    )
    .expect("write fake cookies");

    let config = Arc::new(BridgeConfig {
        server: BridgeSrvCfg {
            bind_addr: "127.0.0.1".to_string(),
            port,
            api_key: None,
            cors_enabled: false,
        },
        storage: StorageConfig {
            data_dir: temp.path().to_path_buf(),
            media_ttl_days: 30,
        },
        transport: TransportConfig {
            tls_profile: "chrome".to_string(),
            proxy_url: None,
            timeout_secs: 10,
        },
    });

    let identity =
        Arc::new(DefaultIdentityService::with_base_url(&config, Some(upstream.uri())).unwrap());

    let adapter = Arc::new(
        gemini_bridge_adapter_gemini::DefaultGeminiAdapter::with_base_url(
            identity,
            config.clone(),
            upstream.uri(),
        )
        .unwrap(),
    );

    let upload_service = Arc::new(UploadServiceImpl::new(
        LocalMediaStore::new(temp.path()),
        Arc::new(RecordingPush),
        UploadLimits::default(),
    ));

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
            upload_service: Some(upload_service),
            image_service: None,
            health_admin: gemini_bridge_http_server::build_health_admin(None),
            conversation_store: None,
            tool_engine: gemini_bridge_http_server::build_tool_engine(),
            gallery_service: None,
            media_purge: None,
        },
    );

    let server = spawn_server(router, port).await;
    let client = reqwest::Client::new();

    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(PNG.to_vec())
            .file_name("test.png")
            .mime_str("image/png")
            .unwrap(),
    );

    let response = client
        .post(format!("http://127.0.0.1:{port}/v1/files"))
        .multipart(form)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let uploaded: serde_json::Value = response.json().await.unwrap();
    let id = uploaded["id"].as_str().unwrap();
    assert_eq!(uploaded["mime_type"], "image/png");
    assert_eq!(uploaded["file_ref"], "files/test-ref");

    let response = client
        .get(format!("http://127.0.0.1:{port}/v1/files/{id}"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers().get("content-type").unwrap(), "image/png");
    assert_eq!(response.bytes().await.unwrap(), Bytes::from_static(PNG));

    server.abort();
}
