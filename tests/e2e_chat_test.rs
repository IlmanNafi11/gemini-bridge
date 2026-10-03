//! End-to-end: non-streaming OpenAI chat via local HTTP server + mock upstream.
//!
//! These tests spin up a real Axum server on a random port and a WireMock
//! upstream that returns canned Gemini Web responses. No credentials leave this
//! process; no real upstream is contacted.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use gemini_bridge_http_server::{AppState, ServerConfig, ServerOptions, build_router_with_options};
use gemini_bridge_llm_service::{
    Completion, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
};
use serde_json::{Value, json};
use tokio::sync::Notify;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct PendingStreamAdapter {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl LlmAdapter for PendingStreamAdapter {
    fn provider_id(&self) -> &'static str {
        "pending-stream"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Err(LlmError::Unavailable)
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        let entered = self.entered.clone();
        let release = self.release.clone();
        Ok(Box::pin(stream::unfold(
            Some((entered, release, false)),
            |state| async move {
                let (entered, release, waiting) = state?;
                if waiting {
                    entered.notify_one();
                    release.notified().await;
                    None
                } else {
                    Some((
                        Ok(LlmEvent::TextDelta("first visible token".to_owned())),
                        Some((entered, release, true)),
                    ))
                }
            },
        )))
    }
}

#[tokio::test]
async fn first_http_sse_event_arrives_before_upstream_stream_completes() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (mut state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    state.adapter = Arc::new(PendingStreamAdapter {
        entered: entered.clone(),
        release: release.clone(),
    });
    spawn_server(state, srv_config).await;

    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .json(&json!({
                "model": "gemini-web-flash",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": true
            }))
            .send()
            .await
            .unwrap()
    });

    let mut response = tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .expect("HTTP headers should arrive without waiting for the first stream event")
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");

    let first_chunk = tokio::time::timeout(Duration::from_secs(2), response.chunk())
        .await
        .expect("SSE event should arrive while adapter stream remains blocked")
        .unwrap()
        .expect("response should contain an SSE event");
    let event = String::from_utf8(first_chunk.to_vec()).unwrap();
    assert!(
        event.contains("first visible token"),
        "unexpected event: {event}"
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("adapter must still be waiting for upstream completion");

    release.notify_one();
    let rest = tokio::time::timeout(Duration::from_secs(2), response.text())
        .await
        .expect("SSE response should finish after releasing upstream")
        .unwrap();
    assert!(rest.contains("data: [DONE]"));
}

struct SilentStreamAdapter;

#[async_trait]
impl LlmAdapter for SilentStreamAdapter {
    fn provider_id(&self) -> &'static str {
        "silent-stream"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Err(LlmError::Unavailable)
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        Ok(Box::pin(stream::pending()))
    }
}

#[tokio::test]
async fn idle_http_sse_stream_emits_keepalive_comment() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (mut state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    state.adapter = Arc::new(SilentStreamAdapter);
    spawn_server(state, srv_config).await;

    let mut response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let keepalive = tokio::time::timeout(Duration::from_secs(17), response.chunk())
        .await
        .expect("idle SSE stream must emit a keepalive")
        .unwrap()
        .expect("keepalive response chunk");
    let keepalive = String::from_utf8(keepalive.to_vec()).unwrap();
    assert!(
        keepalive.starts_with(':'),
        "expected SSE comment, got {keepalive:?}"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind free port")
        .local_addr()
        .unwrap()
        .port()
}

fn gemini_app_response() -> String {
    r#"<html><script>
    var data = {"cfb2h":"test-build-label","FdrFJe":"test-fsid","SNlM0e":"test-snlm0e"};
    </script></html>"#
        .to_string()
}

fn gemini_stream_generate_response(text: &str) -> String {
    // The adapter skips the anti-XSSI line and recursively searches for this
    // schema path: candidates[0].parts[0].text.
    let inner = json!({
        "candidates": [{
            "parts": [{"text": text}]
        }]
    });
    format!(")]}}'\n{inner}\n")
}
/// Pre-seeds fake credentials on disk so the identity service starts in Stale
/// (not Unconfigured) state and can build auth headers.
async fn make_state(
    wiremock_uri: &str,
    port: u16,
    api_key: Option<String>,
) -> (AppState, ServerConfig) {
    use gemini_bridge_adapter_gemini::DefaultGeminiAdapter;
    use gemini_bridge_config::{
        BridgeConfig, ServerConfig as BridgeSrvCfg, StorageConfig, TransportConfig,
    };
    use gemini_bridge_identity::DefaultIdentityService;

    let data_dir = std::env::temp_dir().join(format!("gemini-bridge-test-{port}"));
    std::fs::create_dir_all(&data_dir).expect("create test data dir");

    // Pre-seed fake credentials so auth headers can be built without real cookies.
    let fake_cookies = json!({
        "psid": "fake-psid",
        "psidts": "fake-psidts",
        "sapisid": "fake-sapisid",
        "imported_at": "2026-01-01T00:00:00Z"
    });
    std::fs::write(
        data_dir.join("cookies.json"),
        serde_json::to_string(&fake_cookies).unwrap(),
    )
    .expect("write fake cookies");

    let config = Arc::new(BridgeConfig {
        server: BridgeSrvCfg {
            bind_addr: "127.0.0.1".to_string(),
            port,
            api_key: api_key.clone(),
            ..BridgeSrvCfg::default()
        },
        storage: StorageConfig {
            data_dir,
            media_ttl_days: 30,
        },
        transport: TransportConfig {
            tls_profile: "chrome".to_string(),
            proxy_url: None,
            timeout_secs: 10,
        },
        video: Default::default(),
    });

    let identity = Arc::new(
        DefaultIdentityService::with_base_url(&config, Some(wiremock_uri.to_string()))
            .expect("identity service"),
    );

    let adapter = Arc::new(
        DefaultGeminiAdapter::with_base_url(identity.clone(), config.clone(), wiremock_uri)
            .expect("adapter"),
    );

    let srv_config = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key,
        require_key_for_admin: false,
        cors_enabled: false,
        rate_limit: None,
        metrics_enabled: false,
    };

    (
        AppState {
            adapter,
            upload_service: None,
            image_service: None,
            video_service: None,
            health_admin: gemini_bridge_http_server::build_health_admin(Some(identity.clone())),
            identity_service: Some(identity.clone()),
            conversation_store: None,
            tool_engine: gemini_bridge_http_server::build_tool_engine(),
            gallery_service: None,
            media_purge: None,
        },
        srv_config,
    )
}

/// Spawn an Axum server on the given config, returning quickly.
async fn spawn_server(state: AppState, srv_config: ServerConfig) {
    spawn_server_with_options(state, srv_config, ServerOptions::default()).await;
}

async fn spawn_server_with_options(
    state: AppState,
    srv_config: ServerConfig,
    options: ServerOptions,
) {
    let router = build_router_with_options(srv_config.clone(), state, options);
    let listener = tokio::net::TcpListener::bind(&srv_config.bind_addr)
        .await
        .unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
}

// ---------------------------------------------------------------------------
// Test: POST /v1/chat/completions returns OpenAI-shaped response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_chat_completions_returns_openai_shape() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("^/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gemini_app_response()))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex("^/_/BardChatUi/data"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(gemini_stream_generate_response("Hello from mock Gemini!")),
        )
        .mount(&mock_server)
        .await;

    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    spawn_server(state, srv_config).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .expect("request should reach server");

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.expect("should be JSON");
    assert_eq!(body["object"], "chat.completion");
    assert!(
        body["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("chatcmpl-"))
    );
    assert_eq!(body["model"], "gemini-web-flash");
    let content = &body["choices"][0]["message"]["content"];
    assert!(
        content.is_string(),
        "content should be a string, got {content}"
    );
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
}

// ---------------------------------------------------------------------------
// Test: GET /v1/models lists virtual model IDs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_models_lists_virtual_models() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    spawn_server(state, srv_config).await;

    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .send()
        .await
        .expect("request should reach server");

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["object"], "list");

    let ids: Vec<&str> = body["data"]
        .as_array()
        .expect("data should be array")
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();

    assert!(
        ids.contains(&"gemini-web-flash"),
        "should include gemini-web-flash; got {ids:?}"
    );
    assert!(
        ids.contains(&"gemini-web-pro"),
        "should include gemini-web-pro"
    );
    assert!(
        ids.contains(&"gemini-web-thinking"),
        "should include gemini-web-thinking"
    );
}

// ---------------------------------------------------------------------------
// Test: API key enforcement
// ---------------------------------------------------------------------------

#[tokio::test]
async fn api_key_is_enforced_when_configured() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, Some("secret-key".into())).await;
    spawn_server(state, srv_config).await;

    let client = reqwest::Client::new();

    // Missing key → 401.
    let resp_no_key = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({"model":"gemini-web-flash","messages":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp_no_key.status(), 401);

    // Wrong key → 401.
    let resp_wrong_key = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("Authorization", "Bearer wrong-key")
        .json(&json!({"model":"gemini-web-flash","messages":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp_wrong_key.status(), 401);
}
#[tokio::test]
async fn wrong_admin_key_is_rejected_with_constant_time_compare() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, Some("secret-key".into())).await;
    spawn_server(state, srv_config).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/admin/status"))
        .header("Authorization", "Bearer secret-keX")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

// ---------------------------------------------------------------------------
// Test: x-request-id header propagated in response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_id_echoes_valid_input_and_generates_uuid_v4_when_absent() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex("^/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gemini_app_response()))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex("^/_/BardChatUi/data"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(gemini_stream_generate_response("ok")),
        )
        .mount(&mock_server)
        .await;

    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    spawn_server(state, srv_config).await;

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let body = json!({"model":"gemini-web-flash","messages":[{"role":"user","content":"hi"}]});
    let incoming = "client-request-01";
    let echoed = client
        .post(&url)
        .header("x-request-id", incoming)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(echoed.headers()["x-request-id"], incoming);

    let generated = client.post(url).json(&body).send().await.unwrap();
    let request_id = generated.headers()["x-request-id"].to_str().unwrap();
    assert!(
        is_uuid_v4(request_id),
        "generated request ID was {request_id:?}"
    );
}

fn is_uuid_v4(value: &str) -> bool {
    value.len() == 36
        && value.as_bytes()[8] == b'-'
        && value.as_bytes()[13] == b'-'
        && value.as_bytes()[18] == b'-'
        && value.as_bytes()[23] == b'-'
        && value.as_bytes()[14] == b'4'
        && matches!(
            value.as_bytes()[19].to_ascii_lowercase(),
            b'8' | b'9' | b'a' | b'b'
        )
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
}
#[tokio::test]
async fn responses_include_security_headers_and_restrict_cors_origin() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    let origin = format!("http://127.0.0.1:{port}");
    let options = ServerOptions {
        cors_origins: vec![origin.clone()],
        concurrency_limit: 4,
        body_limit_bytes: 10 * 1024 * 1024,
    };
    let mut srv_config = srv_config;
    srv_config.cors_enabled = true;
    spawn_server_with_options(state, srv_config, options).await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .header("origin", origin.clone())
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(
        response.headers()["content-security-policy"],
        "default-src 'none'"
    );
    assert_eq!(response.headers()["access-control-allow-origin"], origin);

    let denied = client
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert!(!denied.headers().contains_key("access-control-allow-origin"));
}
#[tokio::test]
async fn malformed_authorization_headers_are_rejected() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, Some("secret-key".into())).await;
    spawn_server(state, srv_config).await;

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");

    // Bearer scheme with trailing whitespace inside the token must not match.
    let resp = client
        .post(&url)
        .header("Authorization", "Bearer secret-key ")
        .json(&json!({"model":"gemini-web-flash","messages":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Empty token must not match.
    let resp = client
        .post(&url)
        .header("Authorization", "Bearer ")
        .json(&json!({"model":"gemini-web-flash","messages":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Non-bearer schemes are rejected.
    let resp = client
        .post(&url)
        .header("Authorization", "Basic c2VjcmV0LWtleQ==")
        .json(&json!({"model":"gemini-web-flash","messages":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // The scheme must use the canonical `Bearer ` spelling.
    let resp = client
        .post(&url)
        .header("Authorization", "bearer secret-key")
        .json(&json!({"model":"gpt-5","messages":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn oversized_request_body_is_rejected() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    let options = ServerOptions {
        cors_origins: vec![],
        concurrency_limit: 4,
        body_limit_bytes: 1024 * 1024,
    };
    spawn_server_with_options(state, srv_config, options).await;

    let huge = "x".repeat(2 * 1024 * 1024);
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": huge}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 413);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
}
#[tokio::test]
async fn unsupported_method_returns_openai_error_envelope() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    spawn_server(state, srv_config).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 405);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "method_not_allowed");
}

// ---------------------------------------------------------------------------
// Test: unknown model returns OpenAI-shaped error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_model_returns_openai_error_shape() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    spawn_server(state, srv_config).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({"model":"gpt-5","messages":[{"role":"user","content":"hi"}]}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"]["message"].is_string(),
        "error.message must be a string"
    );
    assert!(
        body["error"]["type"].is_string(),
        "error.type must be a string"
    );
}

#[derive(Clone, Copy)]
enum FailureKind {
    Authentication,
    RateLimited,
    Unavailable,
    Unsupported,
    ContinuityRejected,
    Protocol,
}

struct FailingAdapter(FailureKind);

impl FailingAdapter {
    fn error(&self) -> LlmError {
        match self.0 {
            FailureKind::Authentication => LlmError::Authentication,
            FailureKind::RateLimited => LlmError::RateLimited,
            FailureKind::Unavailable => LlmError::Unavailable,
            FailureKind::Unsupported => LlmError::Unsupported("test capability"),
            FailureKind::ContinuityRejected => LlmError::ContinuityRejected,
            FailureKind::Protocol => LlmError::Protocol("invalid upstream frame".to_owned()),
        }
    }
}

#[async_trait]
impl LlmAdapter for FailingAdapter {
    fn provider_id(&self) -> &'static str {
        "failing"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Err(self.error())
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        Err(self.error())
    }
}

#[tokio::test]
async fn llm_errors_map_to_stable_http_status_type_and_code() {
    let cases = [
        (
            FailureKind::Authentication,
            401,
            "authentication_error",
            Some("invalid_api_key"),
        ),
        (
            FailureKind::RateLimited,
            429,
            "rate_limit_exceeded",
            Some("rate_limit_exceeded"),
        ),
        (FailureKind::Unavailable, 503, "service_unavailable", None),
        (FailureKind::Unsupported, 400, "invalid_request_error", None),
        (
            FailureKind::ContinuityRejected,
            410,
            "invalid_request_error",
            None,
        ),
        (FailureKind::Protocol, 502, "provider_error", None),
    ];

    for (kind, expected_status, expected_type, expected_code) in cases {
        let mock_server = MockServer::start().await;
        let port = free_port();
        let (mut state, srv_config) = make_state(&mock_server.uri(), port, None).await;
        state.adapter = Arc::new(FailingAdapter(kind));
        spawn_server(state, srv_config).await;

        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .json(&json!({
                "model": "gemini-web-flash",
                "messages": [{"role": "user", "content": "hello"}]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), expected_status);
        let envelope: Value = response.json().await.unwrap();
        assert!(envelope["error"]["message"].is_string());
        assert_eq!(envelope["error"]["type"], expected_type);
        assert_eq!(envelope["error"]["code"].as_str(), expected_code);
    }
}

#[tokio::test]
async fn stream_with_conversation_id_is_rejected_before_starting_sse() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    spawn_server(state, srv_config).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true,
            "conversation_id": "conversation-01"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 400);
    assert_ne!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let envelope: Value = response.json().await.unwrap();
    assert_eq!(envelope["error"]["type"], "invalid_request_error");
    assert_eq!(envelope["error"]["code"], Value::Null);
}

// ---------------------------------------------------------------------------
// Task 1.1: Streaming SSE chat path
// ---------------------------------------------------------------------------

/// Build a multi-frame Gemini StreamGenerate response body where each frame
/// contains the *cumulative* text accumulated so far.
fn gemini_multi_frame_response(frames: &[&str]) -> String {
    frames
        .iter()
        .map(|text| gemini_stream_generate_response(text))
        .collect::<Vec<_>>()
        .join("")
}

/// Parse raw SSE text into `(data_lines, has_done)`.
///
/// Returns all `data: <json>` payloads (the JSON string, not including `data: `
/// prefix) and whether `data: [DONE]` appeared.
fn parse_sse(raw: &str) -> (Vec<String>, bool) {
    let mut data_lines: Vec<String> = Vec::new();
    let mut has_done = false;
    for line in raw.lines() {
        if let Some(payload) = line.strip_prefix("data: ") {
            if payload == "[DONE]" {
                has_done = true;
            } else {
                data_lines.push(payload.to_owned());
            }
        }
    }
    (data_lines, has_done)
}
struct ErrorStreamAdapter;

#[async_trait]
impl LlmAdapter for ErrorStreamAdapter {
    fn provider_id(&self) -> &'static str {
        "error-stream"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Err(LlmError::Unavailable)
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        Ok(Box::pin(stream::iter([
            Ok(LlmEvent::TextDelta("before error".to_owned())),
            Err(LlmError::Protocol("bad \"frame\"".to_owned())),
        ])))
    }
}

#[tokio::test]
async fn midstream_error_is_final_json_event_without_done_sentinel() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (mut state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    state.adapter = Arc::new(ErrorStreamAdapter);
    spawn_server(state, srv_config).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let raw = response.text().await.unwrap();
    let (data, has_done) = parse_sse(&raw);
    assert!(
        !has_done,
        "failed stream must not advertise clean completion"
    );
    assert_eq!(data.len(), 2);
    let error: Value = serde_json::from_str(&data[1]).unwrap();
    assert!(error["error"].as_str().unwrap().contains("bad \"frame\""));
}

#[tokio::test]
async fn concurrency_limit_one_rejects_second_stream_until_first_drains() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (mut state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    state.adapter = Arc::new(PendingStreamAdapter {
        entered: entered.clone(),
        release: release.clone(),
    });
    let options = ServerOptions {
        cors_origins: vec![],
        concurrency_limit: 1,
        body_limit_bytes: 1024 * 1024,
    };
    spawn_server_with_options(state, srv_config, options).await;

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let body = json!({
        "model": "gemini-web-flash",
        "messages": [{"role": "user", "content": "Hello"}],
        "stream": true
    });
    let first = tokio::spawn({
        let client = client.clone();
        let url = url.clone();
        let body = body.clone();
        async move { client.post(url).json(&body).send().await.unwrap() }
    });
    entered.notified().await;

    let first = tokio::time::timeout(Duration::from_secs(2), first)
        .await
        .expect("first stream should open")
        .unwrap();
    assert_eq!(first.status(), 200);

    let overloaded = client.post(&url).json(&body).send().await.unwrap();
    assert_eq!(overloaded.status(), 503);
    let envelope: Value = overloaded.json().await.unwrap();
    assert_eq!(envelope["error"]["message"], "Server is overloaded");
    assert_eq!(envelope["error"]["type"], "service_unavailable");
    assert_eq!(envelope["error"]["code"], "server_overloaded");

    release.notify_one();
    let drained = tokio::time::timeout(Duration::from_secs(2), first.text())
        .await
        .expect("first stream must finish once released")
        .unwrap();
    assert!(drained.contains("data: [DONE]"));
}

#[tokio::test]
async fn configured_cors_allows_chat_preflight_from_allowed_origin() {
    let mock_server = MockServer::start().await;
    let port = free_port();
    let (state, mut srv_config) = make_state(&mock_server.uri(), port, None).await;
    srv_config.cors_enabled = true;
    let origin = format!("http://127.0.0.1:{port}");
    let options = ServerOptions {
        cors_origins: vec![origin.clone()],
        concurrency_limit: 4,
        body_limit_bytes: 1024 * 1024,
    };
    spawn_server_with_options(state, srv_config, options).await;

    let response = reqwest::Client::new()
        .request(
            reqwest::Method::OPTIONS,
            format!("http://127.0.0.1:{port}/v1/chat/completions"),
        )
        .header("origin", &origin)
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "authorization,content-type",
        )
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());
    assert_eq!(response.headers()["access-control-allow-origin"], origin);
    assert!(
        response.headers()["access-control-allow-methods"]
            .to_str()
            .unwrap()
            .contains("POST")
    );
    assert!(
        response.headers()["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("content-type")
    );
}

#[tokio::test]
async fn stream_true_returns_sse_chunks_with_done_sentinel() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("^/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gemini_app_response()))
        .mount(&mock_server)
        .await;

    // Three cumulative frames: "Hi", "Hi there", "Hi there!"
    let body = gemini_multi_frame_response(&["Hi", "Hi there", "Hi there!"]);
    Mock::given(method("POST"))
        .and(path_regex("^/_/BardChatUi/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&mock_server)
        .await;

    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    spawn_server(state, srv_config).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("Accept", "text/event-stream")
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true
        }))
        .send()
        .await
        .expect("request should reach server");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "text/event-stream"
    );

    let raw = resp.text().await.expect("should be text");
    let (data_lines, has_done) = parse_sse(&raw);

    // Must have [DONE] terminal sentinel.
    assert!(has_done, "SSE stream must end with data: [DONE]");

    // Must have at least one chunk.
    assert!(
        !data_lines.is_empty(),
        "must produce at least one data chunk"
    );

    // Every data payload must be valid JSON shaped as ChatCompletionChunk.
    for line in &data_lines {
        let chunk: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("SSE payload not valid JSON: {e}\n  payload: {line}"));
        assert_eq!(chunk["object"], "chat.completion.chunk");
        assert!(
            chunk["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("chatcmpl-")),
            "chunk id must be a chatcmpl-... string"
        );
        assert_eq!(chunk["model"], "gemini-web-flash");
        assert!(chunk["choices"].is_array(), "choices must be an array");
    }
}

#[tokio::test]
async fn stream_true_prefix_diff_produces_suffix_deltas_not_full_snapshots() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("^/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gemini_app_response()))
        .mount(&mock_server)
        .await;

    // Two frames: first full text, second extends it.
    let body = gemini_multi_frame_response(&["Part1", "Part1Part2"]);
    Mock::given(method("POST"))
        .and(path_regex("^/_/BardChatUi/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&mock_server)
        .await;

    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    spawn_server(state, srv_config).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    let raw = resp.text().await.unwrap();
    let (data_lines, _) = parse_sse(&raw);

    // Concatenate all delta content — must equal the full text without repetition.
    let combined: String = data_lines
        .iter()
        .filter_map(|line| {
            serde_json::from_str::<Value>(line).ok().and_then(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_owned)
            })
        })
        .collect();

    // If prefix-diff is correct the combined deltas should equal the full text.
    assert_eq!(combined, "Part1Part2");
}

#[tokio::test]
async fn stream_false_still_returns_json_object_not_sse() {
    // Regression: adding stream=true routing must not break stream=false.
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("^/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gemini_app_response()))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex("^/_/BardChatUi/data"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(gemini_stream_generate_response("Non-stream response")),
        )
        .mount(&mock_server)
        .await;

    let port = free_port();
    let (state, srv_config) = make_state(&mock_server.uri(), port, None).await;
    spawn_server(state, srv_config).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
}
