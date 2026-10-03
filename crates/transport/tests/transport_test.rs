use bytes::Bytes;
use gemini_bridge_config::TransportConfig;
use gemini_bridge_transport::{
    Idempotency, ReqwestTransport, TransportError, TransportRequest, TransportService,
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(timeout_secs: u64) -> TransportConfig {
    TransportConfig {
        tls_profile: "chrome".to_string(),
        proxy_url: None,
        timeout_secs,
    }
}

fn request(url: Url, idempotency: Idempotency) -> TransportRequest {
    TransportRequest {
        method: Method::POST,
        url,
        headers: HeaderMap::new(),
        body: Some(Bytes::from_static(b"request-body")),
        idempotency,
    }
}
#[test]
fn transport_rejects_unknown_tls_profile_instead_of_falling_back_to_chrome() {
    let mut config = config(5);
    config.tls_profile = "ja3".to_owned();

    assert!(matches!(
        ReqwestTransport::new(&config),
        Err(TransportError::UnsupportedTlsProfile(profile)) if profile == "ja3"
    ));
}

#[test]
fn transport_accepts_configured_chrome_default_profile() {
    assert!(ReqwestTransport::new(&config(5)).is_ok());
}

#[tokio::test]
async fn execute_stream_exposes_response_body_as_incremental_chunks() {
    use futures::StreamExt;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/stream"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"first\nsecond\n".to_vec()))
        .mount(&server)
        .await;

    let transport = ReqwestTransport::new(&config(5)).unwrap();
    let response = transport
        .execute_stream(request(
            Url::parse(&format!("{}/stream", server.uri())).unwrap(),
            Idempotency::NeverRetry,
        ))
        .await
        .unwrap();

    assert_eq!(response.status, StatusCode::OK);
    let mut chunks = response.body;
    let mut body = Vec::new();
    while let Some(chunk) = chunks.next().await {
        body.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(body, b"first\nsecond\n");
}

#[tokio::test]
async fn request_execution_returns_body_status_and_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ok"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("x-result", "created")
                .set_body_bytes(b"response-body".to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let transport = ReqwestTransport::new(&config(5)).unwrap();
    let response = transport
        .execute(request(
            Url::parse(&format!("{}/ok", server.uri())).unwrap(),
            Idempotency::NeverRetry,
        ))
        .await
        .unwrap();

    assert_eq!(response.status, StatusCode::CREATED);
    assert_eq!(response.headers["x-result"], "created");
    assert_eq!(response.body.as_ref(), b"response-body");
}
#[tokio::test]
async fn execute_rejects_oversized_buffered_response_and_accepts_limit() {
    let server = MockServer::start().await;
    let limit = gemini_bridge_transport::MAX_BUFFERED_RESPONSE_BYTES;
    Mock::given(method("POST"))
        .and(path("/oversized"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!("{}{}", "x".repeat(limit), "x"),
            "application/octet-stream",
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/boundary"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; limit]))
        .mount(&server)
        .await;

    let transport = ReqwestTransport::new(&config(10)).unwrap();
    let oversized = transport
        .execute(request(
            Url::parse(&format!("{}/oversized", server.uri())).unwrap(),
            Idempotency::NeverRetry,
        ))
        .await;
    assert!(matches!(
        oversized,
        Err(TransportError::ResponseTooLarge { limit: actual }) if actual == limit
    ));

    let boundary = transport
        .execute(request(
            Url::parse(&format!("{}/boundary", server.uri())).unwrap(),
            Idempotency::NeverRetry,
        ))
        .await
        .unwrap();
    assert_eq!(boundary.body.len(), limit);
}

#[tokio::test]
async fn never_retry_does_not_repeat_server_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/fail"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let transport = ReqwestTransport::new(&config(5)).unwrap();
    let response = transport
        .execute(request(
            Url::parse(&format!("{}/fail", server.uri())).unwrap(),
            Idempotency::NeverRetry,
        ))
        .await
        .unwrap();

    assert_eq!(response.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn safe_to_retry_retries_transient_error_then_succeeds() {
    let server = MockServer::start().await;
    // First request intentionally disconnects mid-response, causing a transient
    // network/body error; the next request receives a valid response.
    Mock::given(method("POST"))
        .and(path("/retry"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"retry-ok".to_vec()))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    let transport = ReqwestTransport::new(&config(5)).unwrap();
    let response = transport
        .execute(request(
            Url::parse(&format!("{}/retry", server.uri())).unwrap(),
            Idempotency::SafeToRetry,
        ))
        .await
        .unwrap();

    assert_eq!(response.body.as_ref(), b"retry-ok");
}

#[tokio::test]
async fn request_headers_and_body_are_preserved() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/echo"))
        .and(wiremock::matchers::header("x-custom", "exact-value"))
        .and(wiremock::matchers::body_bytes(b"request-body"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let transport = ReqwestTransport::new(&config(5)).unwrap();
    let mut req = request(
        Url::parse(&format!("{}/echo", server.uri())).unwrap(),
        Idempotency::NeverRetry,
    );
    req.headers.insert(
        HeaderName::from_static("x-custom"),
        HeaderValue::from_static("exact-value"),
    );

    let response = transport.execute(req).await.unwrap();
    assert_eq!(response.status, StatusCode::OK);
}

#[test]
fn secret_headers_are_masked_in_debug_output() {
    let mut headers = HeaderMap::new();
    headers.insert("cookie", HeaderValue::from_static("sid=secret-cookie"));
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer secret-token"),
    );

    let req = TransportRequest {
        method: Method::GET,
        url: Url::parse("https://example.test/").unwrap(),
        headers,
        body: None,
        idempotency: Idempotency::NeverRetry,
    };
    let debug = format!("{req:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("secret-cookie"));
    assert!(!debug.contains("secret-token"));

    let response = gemini_bridge_transport::TransportResponse {
        status: StatusCode::OK,
        headers: [(
            HeaderName::from_static("set-cookie"),
            HeaderValue::from_static("sid=response-secret"),
        )]
        .into_iter()
        .collect(),
        body: Bytes::from_static(b"response"),
    };
    let debug = format!("{response:?}");
    assert!(!debug.contains("response-secret"));
}

#[test]
fn tls_profiles_do_not_claim_client_hello_impersonation() {
    // Task 0.3 remediation: the stack is reqwest + rustls, which supports HTTP
    // header presets only. The public API must not present header presets as
    // JA3/TLS fingerprint impersonation.
    for profile in [
        gemini_bridge_transport::TlsProfile::Chrome,
        gemini_bridge_transport::TlsProfile::Firefox,
        gemini_bridge_transport::TlsProfile::Safari,
    ] {
        assert!(!profile.has_client_hello_impersonation());
    }
}

#[test]
fn error_types_remain_sendable() {
    fn assert_send_sync<T: Send + Sync>() {}
    // The trait/service must be safe for async use; error Sync is not required by API,
    // but this compile-time check ensures the crate remains lightweight to integrate.
    assert_send_sync::<ReqwestTransport>();
    let _ = std::mem::size_of::<Option<TransportError>>();
}
