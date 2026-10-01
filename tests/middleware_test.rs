//! Integration tests for middleware (Task 1.7):
//! - Rate limiting enforcement and 429 Retry-After mapping
//! - Request ID propagation
//! - Auth before rate limit ordering
//! - Redaction behavior in diagnostic output

use std::net::TcpListener;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::HeaderMap;
use futures::stream;
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_identity::{
    IdentityError, IdentityService, SessionBootstrap, SessionSnapshot, SessionStatus,
};
use gemini_bridge_llm_service::{
    Completion, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest, Usage,
};
use gemini_bridge_middleware::{RedactionFilter, TokenBucketConfig};
use serde_json::Value;
use tokio::task::JoinHandle;

struct MockIdentity;

#[async_trait]
impl IdentityService for MockIdentity {
    async fn bootstrap(&self) -> Result<SessionBootstrap, IdentityError> {
        Ok(SessionBootstrap {
            bl: "mock-bl".to_string(),
            snlm0e: "mock-snlm0e".to_string(),
            fsid: "mock-fsid".to_string(),
        })
    }

    async fn refresh_1psidts(&self) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            status: SessionStatus::Valid,
            build_label: Some("mock-bl".to_string()),
            cookie_age: Some(Duration::from_secs(60)),
            checked_at: std::time::SystemTime::now(),
        }
    }

    fn apply_auth_headers(&self, _headers: &mut HeaderMap) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn import_credentials(&self, _raw: &str) -> Result<(), IdentityError> {
        Ok(())
    }
}

struct MockAdapter;

#[async_trait]
impl LlmAdapter for MockAdapter {
    fn provider_id(&self) -> &'static str {
        "mock"
    }

    async fn complete(&self, _req: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Ok(Completion {
            text: "Hello from mock adapter".to_string(),
            finish_reason: "stop".to_string(),
            usage: Some(Usage {
                prompt_tokens: 5,
                completion_tokens: 5,
            }),
            metadata: None,
        })
    }

    async fn complete_raw(&self, _req: Arc<LlmRequest>) -> Result<Value, LlmError> {
        Ok(serde_json::json!({ "result": "ok" }))
    }

    async fn stream(&self, _req: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
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

async fn spawn_server(router: axum::Router, port: u16) -> JoinHandle<()> {
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    })
}

fn build_test_router_with_rate_limit(
    api_key: Option<String>,
    rate_limit: Option<TokenBucketConfig>,
) -> (axum::Router, u16) {
    let port = free_port();
    let cfg = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key,
        require_key_for_admin: false,
        cors_enabled: false,
        rate_limit,
    };
    let adapter = Arc::new(MockAdapter);
    let identity = Arc::new(MockIdentity);
    let state = AppState {
        adapter,
        upload_service: None,
        image_service: None,
        health_admin: gemini_bridge_http_server::build_health_admin(Some(identity)),
        conversation_store: None,
    };
    (build_router(cfg, state), port)
}

#[tokio::test]
async fn rate_limiter_rejects_with_429_and_retry_after() {
    let rate_limit = TokenBucketConfig {
        capacity: 2,
        refill_tokens: 1,
        refill_interval: Duration::from_secs(60),
    };
    let (router, port) = build_test_router_with_rate_limit(None, Some(rate_limit));
    let _server = spawn_server(router, port).await;

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/models");

    // Request 1: allowed
    let resp1 = client.get(&url).send().await.unwrap();
    assert_eq!(resp1.status(), 200);
    assert!(resp1.headers().contains_key("x-request-id"));

    // Request 2: allowed
    let resp2 = client.get(&url).send().await.unwrap();
    assert_eq!(resp2.status(), 200);

    // Request 3: rate limited (capacity was 2)
    let resp3 = client.get(&url).send().await.unwrap();
    assert_eq!(resp3.status(), 429);
    assert!(resp3.headers().contains_key("x-request-id"));
    assert!(resp3.headers().contains_key("retry-after"));

    let body: Value = resp3.json().await.unwrap();
    assert_eq!(body["error"]["type"], "rate_limit_exceeded");
    assert_eq!(body["error"]["code"], "rate_limit_exceeded");
}

#[tokio::test]
async fn authentication_runs_before_rate_limit() {
    let rate_limit = TokenBucketConfig {
        capacity: 1,
        refill_tokens: 1,
        refill_interval: Duration::from_secs(60),
    };
    let (router, port) =
        build_test_router_with_rate_limit(Some("secret-token".to_string()), Some(rate_limit));
    let _server = spawn_server(router, port).await;

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/models");

    // Unauthenticated requests return 401 without consuming rate-limit tokens
    let resp1 = client.get(&url).send().await.unwrap();
    assert_eq!(resp1.status(), 401);

    let resp2 = client.get(&url).send().await.unwrap();
    assert_eq!(resp2.status(), 401);

    // Authenticated request should still succeed because unauthenticated requests didn't drain bucket
    let resp3 = client
        .get(&url)
        .header("Authorization", "Bearer secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp3.status(), 200);
}

#[test]
fn redaction_filter_redacts_auth_headers_and_gemini_cookies() {
    let headers_log = "Got headers: Authorization: Bearer top-secret-key-123, Cookie: __Secure-1PSID=abc.def; __Secure-1PSIDTS=xyz; user=normal";
    let redacted = RedactionFilter::redact_str(headers_log);

    assert!(!redacted.contains("top-secret-key-123"));
    assert!(!redacted.contains("abc.def"));
    assert!(!redacted.contains("xyz"));
    assert!(redacted.contains("user=normal"));
    assert!(redacted.contains("***REDACTED***"));
}

#[test]
fn captured_tracing_output_contains_no_sentinel_secrets() {
    use std::sync::Mutex;
    use tracing_subscriber::fmt::MakeWriter;

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

    let output = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(BufferWriter(output.clone()))
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        let raw_diagnostic = "Diagnostic log: Bearer secret-sentinel-key; __Secure-1PSID=cookie-sentinel; api_key=another-sentinel";
        let sanitized = RedactionFilter::redact_str(raw_diagnostic);
        tracing::info!(message = %sanitized, "audit emission");
    });

    let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(!logs.contains("secret-sentinel-key"));
    assert!(!logs.contains("cookie-sentinel"));
    assert!(!logs.contains("another-sentinel"));
    assert!(logs.contains("***REDACTED***"));
}
