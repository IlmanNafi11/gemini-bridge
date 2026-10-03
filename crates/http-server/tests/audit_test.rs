use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::Request;
use futures::stream;
use gemini_bridge_health_admin::DefaultHealthAdminService;
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_llm_service::{Completion, LlmAdapter, LlmError, LlmEventStream, LlmRequest};
use gemini_bridge_tool_calling::DefaultToolEngine;
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::fmt::MakeWriter;

struct NullAdapter;

#[async_trait]
impl LlmAdapter for NullAdapter {
    fn provider_id(&self) -> &'static str {
        "audit-test"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Err(LlmError::Unavailable)
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        Ok(Box::pin(stream::empty()) as Pin<Box<_>>)
    }
}

#[derive(Clone)]
struct BufferWriter(Arc<Mutex<Vec<u8>>>);

impl<'a> MakeWriter<'a> for BufferWriter {
    type Writer = BufferHandle;

    fn make_writer(&'a self) -> Self::Writer {
        BufferHandle(self.0.clone())
    }
}

struct BufferHandle(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for BufferHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn completed_request_emits_structured_audit_event_with_response_request_id() {
    let output = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(BufferWriter(output.clone()))
        .with_target(true)
        .finish();
    let router = build_router(
        ServerConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            api_key: None,
            require_key_for_admin: false,
            cors_enabled: false,
            rate_limit: None,
            metrics_enabled: false,
        },
        AppState {
            adapter: Arc::new(NullAdapter),
            upload_service: None,
            image_service: None,
            video_service: None,
            health_admin: Arc::new(DefaultHealthAdminService::new(None)),
            identity_service: None,
            conversation_store: None,
            tool_engine: Arc::new(DefaultToolEngine),
            gallery_service: None,
            media_purge: None,
        },
    );

    let response = async {
        router
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap()
    }
    .with_subscriber(subscriber)
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let request_id = response.headers()["x-request-id"].to_str().unwrap();
    let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("gemini_bridge::audit"));
    assert!(logs.contains("request completed"));
    assert!(logs.contains(&format!("\"request_id\":\"{request_id}\"")));
    assert!(logs.contains("\"method\":\"GET\""));
    assert!(logs.contains("\"path\":\"/healthz\""));
    assert!(logs.contains("\"status\":200"));
}
