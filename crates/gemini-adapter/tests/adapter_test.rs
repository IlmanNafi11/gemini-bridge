use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use gemini_bridge_adapter_gemini::schema::{GeminiWebSchema, PathSegment};
use gemini_bridge_adapter_gemini::{
    DefaultGeminiAdapter, GeminiAdapter, GeminiAdapterError, parse_non_stream_response,
};
use gemini_bridge_config::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig};
use gemini_bridge_identity::{DefaultIdentityService, IdentityService};
use gemini_bridge_llm_service::{
    ContentPart, LlmAdapter, LlmRequest, Message, ModelSelector, Role,
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
async fn repeated_405_is_bounded_and_maps_to_protocol_error() {
    let server = MockServer::start().await;
    let (identity, _dir) = make_identity(&server).await;
    let config = make_config(_dir.path());

    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(bootstrap_body()))
        .expect(2)
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
    let err = adapter
        .generate_non_stream(test_request())
        .await
        .err()
        .unwrap();

    assert!(matches!(err, GeminiAdapterError::SchemaMismatch(_)));
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
    let err = adapter
        .generate_non_stream(test_request())
        .await
        .err()
        .unwrap();

    assert!(matches!(err, GeminiAdapterError::RateLimited));
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
    let err = adapter
        .generate_non_stream(test_request())
        .await
        .err()
        .unwrap();

    assert!(matches!(err, GeminiAdapterError::NeedsAuth));
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
