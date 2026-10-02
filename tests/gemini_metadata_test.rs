//! Integration tests for Task 3.1: Code execution and citations surfacing.
//!
//! Verifies that:
//! 1. Non-streaming `/v1/chat/completions` surfaces `gemini_metadata` with
//!    `code_execution` and `citations` when present in upstream response.
//! 2. Streaming `/v1/chat/completions` surfaces `gemini_metadata` on the final
//!    terminating chunk only; preceding chunks omit `gemini_metadata`.
//! 3. Responses with no metadata omit the `gemini_metadata` field entirely.
//! 4. Standard OpenAI fields (`choices`, `usage`, `finish_reason`, `content`)
//!    are completely preserved and unmodified.
//! 5. Insecure citation URIs (external http, file, javascript) are sanitized out.

use std::net::TcpListener;
use std::sync::Arc;

use gemini_bridge_adapter_gemini::DefaultGeminiAdapter;
use gemini_bridge_config::{
    BridgeConfig, ServerConfig as BridgeSrvCfg, StorageConfig, TransportConfig,
};
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_identity::DefaultIdentityService;
use reqwest::Client;
use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn gemini_app_response() -> String {
    r#"<html><script>
    var data = {"cfb2h":"test-bl","FdrFJe":"test-fsid","SNlM0e":"test-snlm0e"};
    </script></html>"#
        .to_string()
}

async fn make_state(wiremock_uri: &str, port: u16) -> (AppState, ServerConfig) {
    let data_dir = std::env::temp_dir().join(format!("gemini-bridge-metadata-test-{port}"));
    let _ = std::fs::create_dir_all(&data_dir);

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
    .unwrap();

    let config = Arc::new(BridgeConfig {
        server: BridgeSrvCfg {
            bind_addr: "127.0.0.1".to_string(),
            port,
            api_key: None,
            cors_enabled: false,
            metrics_enabled: false,
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
        api_key: None,
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
            conversation_store: None,
            tool_engine: gemini_bridge_http_server::build_tool_engine(),
            gallery_service: None,
            media_purge: None,
        },
        srv_config,
    )
}

async fn spawn_server(state: AppState, srv_config: ServerConfig) {
    let router = build_router(srv_config.clone(), state);
    let listener = tokio::net::TcpListener::bind(&srv_config.bind_addr)
        .await
        .unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
}

fn parse_sse(raw: &str) -> (Vec<String>, bool) {
    let mut data_lines = Vec::new();
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn non_stream_surfaces_code_execution_and_citations() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("^/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gemini_app_response()))
        .mount(&mock)
        .await;

    let upstream_payload = json!({
        "candidates": [{
            "parts": [{"text": "Here is the calculation result: 42"}],
            "candidateId": "cand-001"
        }],
        "code_execution": [{
            "language": "python",
            "code": "result = 6 * 7\nprint(result)",
            "output": "42\n"
        }],
        "citations": [{
            "start_index": 0,
            "end_index": 33,
            "uri": "https://docs.python.org/3/tutorial/",
            "title": "Python Tutorial"
        }],
        "conversation_id": "conv-test-123"
    });
    let stream_body = format!(
        ")]}}'\n{}\n",
        serde_json::to_string(&upstream_payload).unwrap()
    );

    Mock::given(method("POST"))
        .and(path_regex("^/_/BardChatUi/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string(stream_body))
        .mount(&mock)
        .await;

    let port = free_port();
    let (state, srv_config) = make_state(&mock.uri(), port).await;
    spawn_server(state, srv_config).await;

    let client = Client::new();
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "Calculate 6 * 7"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();

    // Standard OpenAI fields must be intact.
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "Here is the calculation result: 42"
    );
    assert_eq!(body["choices"][0]["finish_reason"], "stop");

    // gemini_metadata extension field must be present.
    let metadata = &body["gemini_metadata"];
    assert!(metadata.is_object(), "gemini_metadata should be present");

    // Code execution block must be correctly structured.
    let code_exec = &metadata["code_execution"];
    assert_eq!(code_exec.as_array().unwrap().len(), 1);
    assert_eq!(code_exec[0]["language"], "python");
    assert_eq!(code_exec[0]["code"], "result = 6 * 7\nprint(result)");
    assert_eq!(code_exec[0]["stdout"], "42\n");
    assert!(code_exec[0].get("stderr").is_none());

    // Citation must be present with sanitized HTTPS URI.
    let citations = &metadata["citations"];
    assert_eq!(citations.as_array().unwrap().len(), 1);
    assert_eq!(citations[0]["start_index"], 0);
    assert_eq!(citations[0]["end_index"], 33);
    assert_eq!(citations[0]["uri"], "https://docs.python.org/3/tutorial/");
    assert_eq!(citations[0]["title"], "Python Tutorial");

    // Conversation ID must be preserved.
    assert_eq!(metadata["conversation_id"], "conv-test-123");
}

#[tokio::test]
async fn non_stream_omits_metadata_when_absent() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("^/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gemini_app_response()))
        .mount(&mock)
        .await;

    let upstream_payload = json!({
        "candidates": [{
            "parts": [{"text": "Plain answer without metadata"}],
            "candidateId": "cand-002"
        }]
    });
    let stream_body = format!(
        ")]}}'\n{}\n",
        serde_json::to_string(&upstream_payload).unwrap()
    );

    Mock::given(method("POST"))
        .and(path_regex("^/_/BardChatUi/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string(stream_body))
        .mount(&mock)
        .await;

    let port = free_port();
    let (state, srv_config) = make_state(&mock.uri(), port).await;
    spawn_server(state, srv_config).await;

    let client = Client::new();
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();

    assert_eq!(
        body["choices"][0]["message"]["content"],
        "Plain answer without metadata"
    );
    assert!(
        body.get("gemini_metadata").is_none(),
        "gemini_metadata should be omitted when absent"
    );
}

#[tokio::test]
async fn non_stream_sanitizes_insecure_citations() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("^/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gemini_app_response()))
        .mount(&mock)
        .await;

    let upstream_payload = json!({
        "candidates": [{
            "parts": [{"text": "Citations test"}],
            "candidateId": "cand-003"
        }],
        "citations": [
            {
                "start_index": 0,
                "end_index": 5,
                "uri": "http://insecure-external.com/leak",
                "title": "Insecure"
            },
            {
                "start_index": 5,
                "end_index": 10,
                "uri": "file:///etc/passwd",
                "title": "Local file"
            },
            {
                "start_index": 10,
                "end_index": 14,
                "uri": "https://secure-site.org/info",
                "title": "Secure Site"
            }
        ]
    });
    let stream_body = format!(
        ")]}}'\n{}\n",
        serde_json::to_string(&upstream_payload).unwrap()
    );

    Mock::given(method("POST"))
        .and(path_regex("^/_/BardChatUi/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string(stream_body))
        .mount(&mock)
        .await;

    let port = free_port();
    let (state, srv_config) = make_state(&mock.uri(), port).await;
    spawn_server(state, srv_config).await;

    let client = Client::new();
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "Cite sources"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();

    let citations = &body["gemini_metadata"]["citations"];
    let citations_arr = citations.as_array().expect("citations must be an array");
    assert_eq!(citations_arr.len(), 1);
    assert_eq!(citations_arr[0]["uri"], "https://secure-site.org/info");
    assert_eq!(citations_arr[0]["title"], "Secure Site");
}

#[tokio::test]
async fn streaming_attaches_metadata_only_to_final_chunk() {
    let mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("^/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gemini_app_response()))
        .mount(&mock)
        .await;

    let frame1 = json!({
        "candidates": [{
            "parts": [{"text": "Hel"}]
        }]
    });
    let frame2 = json!({
        "candidates": [{
            "parts": [{"text": "Hello world"}]
        }],
        "code_execution": [{
            "language": "python",
            "code": "print('hello')",
            "output": "hello\n"
        }],
        "citations": [{
            "start_index": 0,
            "end_index": 11,
            "uri": "https://example.com/hello",
            "title": "Hello"
        }]
    });

    let stream_body = format!(
        ")]}}'\n{}\n{}\n",
        serde_json::to_string(&frame1).unwrap(),
        serde_json::to_string(&frame2).unwrap()
    );

    Mock::given(method("POST"))
        .and(path_regex("^/_/BardChatUi/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string(stream_body))
        .mount(&mock)
        .await;

    let port = free_port();
    let (state, srv_config) = make_state(&mock.uri(), port).await;
    spawn_server(state, srv_config).await;

    let client = Client::new();
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("Accept", "text/event-stream")
        .json(&json!({
            "model": "gemini-web-flash",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );

    let raw = res.text().await.unwrap();
    let (data_lines, has_done) = parse_sse(&raw);

    assert!(has_done, "Stream must end with [DONE]");
    assert!(
        data_lines.len() >= 2,
        "Must produce text chunk(s) and a terminal chunk"
    );

    // Intermediate chunks must NOT have gemini_metadata.
    for line in &data_lines[..data_lines.len() - 1] {
        let chunk: Value = serde_json::from_str(line).unwrap();
        assert!(
            chunk.get("gemini_metadata").is_none(),
            "Intermediate chunk must not have gemini_metadata: {line}"
        );
    }

    // Final chunk (with finish_reason: "stop") MUST carry gemini_metadata.
    let final_chunk: Value = serde_json::from_str(data_lines.last().unwrap()).unwrap();
    assert_eq!(final_chunk["choices"][0]["finish_reason"], "stop");

    let meta = &final_chunk["gemini_metadata"];
    assert!(
        meta.is_object(),
        "Final chunk must have gemini_metadata, got: {final_chunk}"
    );

    let code_exec = &meta["code_execution"];
    assert_eq!(code_exec[0]["language"], "python");
    assert_eq!(code_exec[0]["code"], "print('hello')");
    assert_eq!(code_exec[0]["stdout"], "hello\n");

    let citations = &meta["citations"];
    assert_eq!(citations[0]["uri"], "https://example.com/hello");
}
