//! End-to-end: non-streaming OpenAI chat via local HTTP server + mock upstream.
//!
//! These tests spin up a real Axum server on a random port and a WireMock
//! upstream that returns canned Gemini Web responses. No credentials leave this
//! process; no real upstream is contacted.

use std::net::TcpListener;
use std::sync::Arc;

use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use gemini_bridge_http_server::{AppState, ServerConfig, build_router};

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
            cors_enabled: false,
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
    });

    let identity = Arc::new(
        DefaultIdentityService::with_base_url(&config, Some(wiremock_uri.to_string()))
            .expect("identity service"),
    );

    let adapter = Arc::new(
        DefaultGeminiAdapter::with_base_url(identity, config.clone(), wiremock_uri)
            .expect("adapter"),
    );

    let srv_config = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key,
        require_key_for_admin: false,
        cors_enabled: false,
    };

    (AppState { adapter }, srv_config)
}

/// Spawn an Axum server on the given config, returning quickly.
async fn spawn_server(state: AppState, srv_config: ServerConfig) {
    let router = build_router(srv_config.clone(), state);
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

// ---------------------------------------------------------------------------
// Test: x-request-id header propagated in response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_id_is_echoed_in_response() {
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

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({"model":"gemini-web-flash","messages":[{"role":"user","content":"hi"}]}))
        .send()
        .await
        .unwrap();

    assert!(
        resp.headers().contains_key("x-request-id"),
        "x-request-id header must be present in response"
    );
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
