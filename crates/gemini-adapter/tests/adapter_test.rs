use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt;
use gemini_bridge_adapter_gemini::schema::{GeminiWebSchema, PathSegment};
use gemini_bridge_adapter_gemini::{
    DefaultGeminiAdapter, GeminiAdapter, GeminiAdapterError, parse_non_stream_response,
};
use gemini_bridge_config::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig};
use gemini_bridge_identity::{DefaultIdentityService, IdentityService};
use gemini_bridge_llm_service::{
    ContentPart, LlmAdapter, LlmError, LlmRequest, Message, ModelSelector, Role,
};
use insta::assert_snapshot;
use tempfile::tempdir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURE_RESPONSE: &str = include_str!("../fixtures/sample_response.txt");

fn make_config(temp_dir: &std::path::Path) -> Arc<BridgeConfig> {
    Arc::new(BridgeConfig {
        server: ServerConfig {
            bind_addr: "127.0.0.1".to_owned(),
            port: 8090,
            api_key: None,
            cors_enabled: false,
            metrics_enabled: false,
            ..ServerConfig::default()
        },
        storage: StorageConfig {
            data_dir: temp_dir.to_path_buf(),
            media_ttl_days: 30,
        },
        transport: TransportConfig {
            tls_profile: "chrome".to_owned(),
            proxy_url: None,
            timeout_secs: 5,
        },
        video: Default::default(),
    })
}

async fn make_identity(server: &MockServer) -> (Arc<DefaultIdentityService>, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let config = make_config(dir.path());
    let identity = DefaultIdentityService::with_base_url(&config, Some(server.uri())).unwrap();
    identity
        .import_credentials(
            "__Secure-1PSID=fake-psid; __Secure-1PSIDTS=fake-ts; SAPISID=fake-sapisid",
        )
        .await
        .unwrap();
    (Arc::new(identity), dir)
}

fn test_request() -> Arc<LlmRequest> {
    Arc::new(LlmRequest {
        model: ModelSelector {
            provider: "gemini-web".to_owned(),
            model: "gemini-pro".to_owned(),
            thinking_level: None,
        },
        messages: Arc::from([Message {
            role: Role::User,
            parts: Arc::from([ContentPart::Text("Hello Gemini".to_owned())]),
        }]),
        temperature: None,
        max_output_tokens: None,
        tools: Arc::from([]),
        metadata: BTreeMap::new(),
    })
}

fn bootstrap_body() -> &'static str {
    r#"
    <!doctype html>
    <script>
      window.WIZ_global_data = {
        "cfb2h": "boq_assistant-bard-web-server_20240101.00_p0",
        "SNlM0e": "AIzaSyFakeTokenForTesting1234567890",
        "FdrFJe": "1234567890123456789"
      };
    </script>
    "#
}

#[tokio::test]
async fn non_stream_parses_sample_response_fixture_and_snapshot_matches() {
    let text = parse_non_stream_response(FIXTURE_RESPONSE.as_bytes(), &GeminiWebSchema::default())
        .unwrap();
    assert_snapshot!(text, @"Hello from the sanitized Gemini fixture.");
}

#[tokio::test]
async fn non_stream_request_success_through_wiremock() {
    let server = MockServer::start().await;
    let (identity, _dir) = make_identity(&server).await;
    let config = make_config(_dir.path());

    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(bootstrap_body()))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(
            "/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE_RESPONSE))
        .mount(&server)
        .await;

    let adapter = DefaultGeminiAdapter::with_base_url(identity, config, server.uri()).unwrap();
    let response = adapter.generate_non_stream(test_request()).await.unwrap();

    assert_eq!(response.text, "Hello from the sanitized Gemini fixture.");
    assert_eq!(response.finish_reason, "stop");
    assert_eq!(adapter.provider_id(), "gemini-web");
}

#[tokio::test]
async fn llm_adapter_trait_implementation_streams_parsed_events() {
    let server = MockServer::start().await;
    let (identity, _dir) = make_identity(&server).await;
    let config = make_config(_dir.path());

    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(bootstrap_body()))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(
            "/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE_RESPONSE))
        .mount(&server)
        .await;

    let adapter = DefaultGeminiAdapter::with_base_url(identity, config, server.uri()).unwrap();
    let completion = adapter.complete(test_request()).await.unwrap();
    assert_eq!(completion.text, "Hello from the sanitized Gemini fixture.");

    let events: Vec<_> = adapter
        .stream(test_request())
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        Ok(gemini_bridge_llm_service::LlmEvent::TextDelta(text))
            if text == "Hello from the sanitized Gemini fixture."
    ));
    assert!(matches!(
        &events[1],
        Ok(gemini_bridge_llm_service::LlmEvent::Completed(summary))
            if summary.finish_reason == "stop"
    ));
}
#[tokio::test]
async fn stream_yields_complete_frames_before_upstream_finishes_and_drop_cancels_read() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::time::{Duration, timeout};

    let identity_server = MockServer::start().await;
    let (identity, _dir) = make_identity(&identity_server).await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(bootstrap_body()))
        .mount(&identity_server)
        .await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_url = format!("http://{}", listener.local_addr().unwrap());
    let (closed_tx, closed_rx) = oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut bytes = [0_u8; 1024];
            let count = socket.read(&mut bytes).await.unwrap();
            if count == 0 {
                return;
            }
            request.extend_from_slice(&bytes[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let frame = format!(
            ")]}}'\n{}\n",
            serde_json::json!([null, [null, null], null, null, [["candidate_id", ["First frame"]]]])
        );
        let split = frame.len() / 2;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        for part in [&frame.as_bytes()[..split], &frame.as_bytes()[split..]] {
            socket
                .write_all(format!("{:X}\r\n", part.len()).as_bytes())
                .await
                .unwrap();
            socket.write_all(part).await.unwrap();
            socket.write_all(b"\r\n").await.unwrap();
            socket.flush().await.unwrap();
        }

        // Wait for the client to drop the stream (connection close) before
        // signaling completion.
        let mut byte = [0_u8; 1024];
        loop {
            match socket.read(&mut byte).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = closed_tx.send(());
    });

    let config = make_config(_dir.path());
    let adapter = DefaultGeminiAdapter::with_base_url(identity, config, upstream_url).unwrap();
    let mut events = adapter.stream(test_request()).await.unwrap();
    let first = timeout(Duration::from_secs(2), events.next())
        .await
        .expect("first frame must arrive while upstream remains open")
        .unwrap()
        .unwrap();
    assert!(
        matches!(first, gemini_bridge_llm_service::LlmEvent::TextDelta(text) if text == "First frame")
    );

    drop(events);
    timeout(Duration::from_secs(2), closed_rx)
        .await
        .expect("dropping downstream stream must close the upstream response")
        .expect("upstream close notification");
    upstream.await.unwrap();
}

#[tokio::test]
async fn mid_stream_upstream_close_surfaces_error_event_not_hang() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::{Duration, timeout};

    let identity_server = MockServer::start().await;
    let (identity, _dir) = make_identity(&identity_server).await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(bootstrap_body()))
        .mount(&identity_server)
        .await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut bytes = [0_u8; 1024];
            let count = socket.read(&mut bytes).await.unwrap();
            if count == 0 {
                return;
            }
            request.extend_from_slice(&bytes[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let frame = format!(
            ")]}}'\n{}\n",
            serde_json::json!([null, [null, null], null, null, [["candidate_id", ["Partial"]]]])
        );
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        socket
            .write_all(format!("{:X}\r\n", frame.len()).as_bytes())
            .await
            .unwrap();
        socket.write_all(frame.as_bytes()).await.unwrap();
        socket.write_all(b"\r\n").await.unwrap();
        socket.flush().await.unwrap();
        // Abruptly abort the connection mid-stream, without the terminal 0-length
        // chunk: the downstream stream must surface a transport error event.
    });

    let config = make_config(_dir.path());
    let adapter = DefaultGeminiAdapter::with_base_url(identity, config, upstream_url).unwrap();
    let mut events = adapter.stream(test_request()).await.unwrap();
    let first = timeout(Duration::from_secs(2), events.next())
        .await
        .expect("first frame must arrive before the close")
        .unwrap()
        .unwrap();
    assert!(
        matches!(first, gemini_bridge_llm_service::LlmEvent::TextDelta(text) if text == "Partial")
    );

    let second = timeout(Duration::from_secs(2), events.next())
        .await
        .expect("abrupt upstream close must surface an error event, not hang");
    assert!(matches!(
        second,
        Some(Err(gemini_bridge_llm_service::LlmError::Unavailable))
    ));
    upstream.await.unwrap();
}

#[tokio::test]
async fn repeated_405_is_bounded_and_preserves_stale_build_label() {
    let server = MockServer::start().await;
    let (identity, _dir) = make_identity(&server).await;
    let config = make_config(_dir.path());

    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "__Secure-1PSIDTS=fresh-ts; Path=/; Secure")
                .set_body_string(bootstrap_body()),
        )
        .expect(3)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(
            "/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate",
        ))
        .respond_with(ResponseTemplate::new(405))
        .expect(2)
        .mount(&server)
        .await;

    let adapter = DefaultGeminiAdapter::with_base_url(identity, config, server.uri()).unwrap();
    let err = adapter.complete(test_request()).await.err().unwrap();

    assert!(matches!(err, LlmError::Unavailable));
}

#[tokio::test]
async fn second_attempt_429_preserves_rate_limit_taxonomy() {
    let server = MockServer::start().await;
    let (identity, _dir) = make_identity(&server).await;
    let config = make_config(_dir.path());

    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "__Secure-1PSIDTS=fresh-ts; Path=/; Secure")
                .set_body_string(bootstrap_body()),
        )
        .expect(3)
        .mount(&server)
        .await;

    let post_calls = Arc::new(AtomicUsize::new(0));
    let post_calls_for_response = post_calls.clone();
    Mock::given(method("POST"))
        .and(path(
            "/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate",
        ))
        .respond_with(move |_request: &wiremock::Request| {
            if post_calls_for_response.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(405)
            } else {
                ResponseTemplate::new(429)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let adapter = DefaultGeminiAdapter::with_base_url(identity, config, server.uri()).unwrap();
    let err = adapter.complete(test_request()).await.err().unwrap();

    assert!(matches!(err, LlmError::RateLimited));
}

#[tokio::test]
async fn stream_retry_preserves_second_attempt_rate_limit_taxonomy() {
    let server = MockServer::start().await;
    let (identity, _dir) = make_identity(&server).await;
    let config = make_config(_dir.path());

    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "__Secure-1PSIDTS=fresh-ts; Path=/; Secure")
                .set_body_string(bootstrap_body()),
        )
        .expect(3)
        .mount(&server)
        .await;

    let post_calls = Arc::new(AtomicUsize::new(0));
    let post_calls_for_response = post_calls.clone();
    Mock::given(method("POST"))
        .and(path(
            "/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate",
        ))
        .respond_with(move |_request: &wiremock::Request| {
            if post_calls_for_response.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(405)
            } else {
                ResponseTemplate::new(429)
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let adapter = DefaultGeminiAdapter::with_base_url(identity, config, server.uri()).unwrap();
    let err = adapter.stream(test_request()).await.err().unwrap();

    assert!(matches!(err, LlmError::RateLimited));
    assert_eq!(post_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn status_429_maps_to_rate_limited() {
    let server = MockServer::start().await;
    let (identity, _dir) = make_identity(&server).await;
    let config = make_config(_dir.path());

    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(bootstrap_body()))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(
            "/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate",
        ))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let adapter = DefaultGeminiAdapter::with_base_url(identity, config, server.uri()).unwrap();
    let err = adapter.complete(test_request()).await.err().unwrap();

    assert!(matches!(err, LlmError::RateLimited));
}

#[tokio::test]
async fn status_401_maps_to_needs_auth() {
    let server = MockServer::start().await;
    let (identity, _dir) = make_identity(&server).await;
    let config = make_config(_dir.path());

    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(bootstrap_body()))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(
            "/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate",
        ))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let adapter = DefaultGeminiAdapter::with_base_url(identity, config, server.uri()).unwrap();
    let err = adapter.complete(test_request()).await.err().unwrap();

    assert!(matches!(err, LlmError::Authentication));
}

#[test]
fn malformed_payload_returns_schema_mismatch_without_panicking() {
    let err = parse_non_stream_response(b"not json at all", &GeminiWebSchema::default())
        .err()
        .unwrap();
    assert!(matches!(err, GeminiAdapterError::SchemaMismatch(_)));
}

#[test]
fn schema_positional_mismatch_returns_error_safely() {
    let mismatched = GeminiWebSchema {
        user_message: 0,
        conversation_id: 1,
        response_id: 2,
        candidate_text_path: vec![
            PathSegment::Field("nonexistent".to_owned()),
            PathSegment::Index(0),
        ],
    };
    let err = parse_non_stream_response(FIXTURE_RESPONSE.as_bytes(), &mismatched)
        .err()
        .unwrap();
    assert!(matches!(err, GeminiAdapterError::SchemaMismatch(_)));
}
