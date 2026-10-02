//! End-to-end acceptance tests for opt-in metrics and the local status dashboard (Task 3.6).

use std::net::TcpListener;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use axum::http::HeaderMap;
use futures::stream;
use gemini_bridge_health_admin::DefaultHealthAdminService;
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_identity::{
    IdentityError, IdentityService, SessionBootstrap, SessionSnapshot, SessionStatus,
};
use gemini_bridge_llm_service::{
    Completion, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
};

const API_KEY: &str = "metrics-test-key";

struct LocalIdentity;

#[async_trait]
impl IdentityService for LocalIdentity {
    async fn bootstrap(&self) -> Result<SessionBootstrap, IdentityError> {
        Err(IdentityError::NeedsReauth)
    }

    async fn refresh_1psidts(&self) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            status: SessionStatus::Valid,
            build_label: Some("local-build-label".to_string()),
            cookie_age: Some(Duration::from_secs(42)),
            checked_at: SystemTime::now(),
        }
    }

    fn apply_auth_headers(&self, _headers: &mut HeaderMap) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn import_credentials(&self, _raw: &str) -> Result<(), IdentityError> {
        Err(IdentityError::NeedsReauth)
    }
}

struct NullAdapter;

#[async_trait]
impl LlmAdapter for NullAdapter {
    fn provider_id(&self) -> &'static str {
        "null"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Err(LlmError::Unavailable)
    }

    async fn complete_raw(&self, _request: Arc<LlmRequest>) -> Result<serde_json::Value, LlmError> {
        Err(LlmError::Unavailable)
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        let empty: Pin<Box<dyn futures::Stream<Item = Result<LlmEvent, LlmError>> + Send>> =
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

async fn spawn_server(
    metrics_enabled: bool,
) -> (reqwest::Client, String, tokio::task::JoinHandle<()>) {
    let port = free_port();
    let identity: Arc<dyn IdentityService> = Arc::new(LocalIdentity);
    let health_admin = Arc::new(DefaultHealthAdminService::new(Some(identity)));
    let state = AppState {
        adapter: Arc::new(NullAdapter),
        upload_service: None,
        image_service: None,
        video_service: None,
        health_admin,
        conversation_store: None,
        tool_engine: gemini_bridge_http_server::build_tool_engine(),
        gallery_service: None,
        media_purge: None,
    };
    let config = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key: Some(API_KEY.to_string()),
        require_key_for_admin: true,
        cors_enabled: false,
        rate_limit: None,
        metrics_enabled,
    };
    let router = build_router(config, state);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    (
        reqwest::Client::new(),
        format!("http://127.0.0.1:{port}"),
        handle,
    )
}

#[tokio::test]
async fn metrics_endpoint_is_absent_when_disabled() {
    let (client, base_url, server) = spawn_server(false).await;

    let response = client
        .get(format!("{base_url}/metrics"))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    server.abort();
}

#[tokio::test]
async fn metrics_endpoint_requires_the_admin_api_key_when_enabled() {
    let (client, base_url, server) = spawn_server(true).await;

    let response = client
        .get(format!("{base_url}/metrics"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    server.abort();
}

#[tokio::test]
async fn dashboard_requires_the_admin_api_key() {
    let (client, base_url, server) = spawn_server(false).await;

    let response = client
        .get(format!("{base_url}/admin/dashboard"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    server.abort();
}

#[tokio::test]
async fn metrics_scrape_reports_request_error_and_latency_series() {
    let (client, base_url, server) = spawn_server(true).await;

    let health = client
        .get(format!("{base_url}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), reqwest::StatusCode::OK);

    let missing = client
        .get(format!("{base_url}/missing"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let scrape = client
        .get(format!("{base_url}/metrics"))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(scrape.status(), reqwest::StatusCode::OK);
    assert_eq!(
        scrape.headers()[reqwest::header::CONTENT_TYPE],
        "text/plain; version=0.0.4; charset=utf-8"
    );
    let body = scrape.text().await.unwrap();

    assert!(body.contains("# TYPE gemini_bridge_http_requests_total counter"));
    assert!(body.contains(
        "gemini_bridge_http_requests_total{method=\"GET\",route=\"/healthz\",status_class=\"2xx\"} 1"
    ));
    assert!(body.contains("# TYPE gemini_bridge_http_errors_total counter"));
    assert!(body.contains(
        "gemini_bridge_http_errors_total{method=\"GET\",route=\"unmatched\",status_class=\"4xx\"} 1"
    ));
    assert!(body.contains("# TYPE gemini_bridge_http_request_duration_seconds histogram"));
    assert!(body.contains(
        "gemini_bridge_http_request_duration_seconds_count{method=\"GET\",route=\"/healthz\"} 1"
    ));
    assert!(!body.contains("route=\"/metrics\""));

    server.abort();
}

#[tokio::test]
async fn dashboard_renders_minimal_local_process_and_session_state() {
    let (client, base_url, server) = spawn_server(false).await;

    let response = client
        .get(format!("{base_url}/admin/dashboard"))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.headers()[reqwest::header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    let body = response.text().await.unwrap();
    assert!(body.contains("Gemini Bridge Status"));
    assert!(body.contains("Process: ok"));
    assert!(body.contains("Session: valid"));
    assert!(body.contains("Build label: local-build-label"));
    assert!(!body.contains(API_KEY));
    assert!(!body.contains("<script"));
    assert!(!body.contains("http://"));
    assert!(!body.contains("https://"));

    server.abort();
}
