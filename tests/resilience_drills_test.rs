//! Resilience drills: 405 auto-recovery, 429 forwarding, cookie-expiry state (Task 1.6).
//!
//! These tests exercise the boundary behaviour specified in SPEC-identity.md §3.4–3.5
//! and SPEC-health-admin.md §3.3 through a real Axum server + wiremock upstream.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use gemini_bridge_adapter_gemini::DefaultGeminiAdapter;
use gemini_bridge_config::{BridgeConfig, ServerConfig, StorageConfig, TransportConfig};
use gemini_bridge_http_server::{
    AppState, ServerConfig as HttpServerConfig, build_health_admin, build_router,
};
use gemini_bridge_identity::{DefaultIdentityService, IdentityService, SessionStatus};
use gemini_bridge_llm_service::LlmAdapter;
use reqwest::Client;
use serde_json::Value;
use tempfile::TempDir;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn bridge_config(tmp: &std::path::Path) -> BridgeConfig {
    BridgeConfig {
        server: ServerConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: 0,
            api_key: None,
            cors_enabled: false,
        },
        storage: StorageConfig {
            data_dir: tmp.to_path_buf(),
            media_ttl_days: 30,
        },
        transport: TransportConfig {
            tls_profile: "chrome".to_string(),
            proxy_url: None,
            timeout_secs: 5,
        },
    }
}

/// Minimal Gemini `/app` response that satisfies the bootstrap parser (uses FdrFJe, not f.sid).
const APP_HTML: &str = r#"<html><script>
var data = {"cfb2h":"bl-value-001","FdrFJe":"fsid-value","SNlM0e":"snlm0e-value"};
</script></html>"#;

/// A response body the gemini adapter stream parser accepts as a completion.
const STREAM_OK_BODY: &str =
    ")]}'\n{\"candidates\":[{\"parts\":[{\"text\":\"response text\"}]}]}\n";

async fn bind_listener() -> (tokio::net::TcpListener, u16) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// Build the full app (identity + adapter + health-admin + axum router) against
/// a wiremock server and return the base URL plus a live reqwest client.
async fn build_test_app(
    upstream: &MockServer,
    tmp: &std::path::Path,
) -> (String, Client, tokio::task::JoinHandle<()>) {
    let cfg = bridge_config(tmp);
    let cfg_arc = Arc::new(cfg.clone());

    let identity = Arc::new(
        DefaultIdentityService::with_base_url(&cfg, Some(upstream.uri()))
            .expect("identity service"),
    );
    identity
        .import_credentials(
            "__Secure-1PSID=fake-psid; __Secure-1PSIDTS=fake-ts; SAPISID=fake-sapisid",
        )
        .await
        .unwrap();

    let adapter = Arc::new(
        DefaultGeminiAdapter::with_base_url(identity.clone(), cfg_arc.clone(), upstream.uri())
            .expect("adapter"),
    );

    let health_admin = build_health_admin(Some(identity.clone() as Arc<dyn IdentityService>));

    let app_state = AppState {
        adapter: adapter.clone() as Arc<dyn LlmAdapter>,
        upload_service: None,
        image_service: None,
        health_admin,
    };

    let (listener, port) = bind_listener().await;
    let router = build_router(
        HttpServerConfig {
            bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
            api_key: None,
            require_key_for_admin: false,
            cors_enabled: false,
            rate_limit: None,
        },
        app_state,
    );

    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let base = format!("http://127.0.0.1:{port}");
    (base, Client::new(), handle)
}

/// Minimal chat completion request body.
fn chat_body() -> Value {
    serde_json::json!({
        "model": "gemini-web-flash",
        "messages": [{"role": "user", "content": "hi"}]
    })
}

// ─── E2 Resilience drills ─────────────────────────────────────────────────────

/// E2-1: A 405 on the first upstream request triggers one silent bootstrap
/// refresh and one retry; the client receives a 200 completion.
#[tokio::test]
async fn drill_405_recovery_succeeds_on_retry() {
    let upstream = MockServer::start().await;
    let tmp = TempDir::new().unwrap();

    // /app — always succeeds (both initial bootstrap and the refresh)
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(APP_HTML))
        .mount(&upstream)
        .await;

    // StreamGenerate — 405 first, then 200
    let call_count = Arc::new(AtomicU32::new(0));
    let call_count2 = call_count.clone();
    Mock::given(method("POST"))
        .and(path_regex(r"/.*StreamGenerate.*"))
        .respond_with(move |_: &wiremock::Request| {
            let n = call_count2.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(405)
            } else {
                ResponseTemplate::new(200).set_body_string(STREAM_OK_BODY)
            }
        })
        .mount(&upstream)
        .await;

    let (base, client, _h) = build_test_app(&upstream, tmp.path()).await;

    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&chat_body())
        .send()
        .await
        .unwrap();

    // Client must receive a success — not a 502 or error
    assert_eq!(
        resp.status().as_u16(),
        200,
        "client should see 200 after 405 recovery"
    );

    // StreamGenerate was called exactly twice (initial + one retry)
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "StreamGenerate must be called exactly twice on 405 recovery"
    );
}

/// E2-2: If the second attempt also returns 405, the client receives HTTP 502
/// and no further upstream retries are issued.
#[tokio::test]
async fn drill_405_double_failure_returns_502() {
    let upstream = MockServer::start().await;
    let tmp = TempDir::new().unwrap();

    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(APP_HTML))
        .mount(&upstream)
        .await;

    let call_count = Arc::new(AtomicU32::new(0));
    let call_count2 = call_count.clone();
    Mock::given(method("POST"))
        .and(path_regex(r"/.*StreamGenerate.*"))
        .respond_with(move |_: &wiremock::Request| {
            call_count2.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(405) // always 405
        })
        .mount(&upstream)
        .await;

    let (base, client, _h) = build_test_app(&upstream, tmp.path()).await;

    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&chat_body())
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status().as_u16(),
        502,
        "double-405 should surface as 502 to client"
    );

    // Must NOT have retried more than twice total
    assert!(
        call_count.load(Ordering::SeqCst) <= 2,
        "StreamGenerate must not be called more than twice"
    );
}

/// A 405 followed by a failed bootstrap refresh is reported as 502 and is not retried.
#[tokio::test]
async fn drill_405_refresh_failure_returns_502_without_retry() {
    let upstream = MockServer::start().await;
    let tmp = TempDir::new().unwrap();

    let app_calls = Arc::new(AtomicU32::new(0));
    let app_calls_for_response = app_calls.clone();
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(move |_: &wiremock::Request| {
            if app_calls_for_response.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(200).set_body_string(APP_HTML)
            } else {
                ResponseTemplate::new(401)
            }
        })
        .mount(&upstream)
        .await;

    let post_calls = Arc::new(AtomicU32::new(0));
    let post_calls_for_response = post_calls.clone();
    Mock::given(method("POST"))
        .and(path_regex(r"/.*StreamGenerate.*"))
        .respond_with(move |_: &wiremock::Request| {
            post_calls_for_response.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(405)
        })
        .mount(&upstream)
        .await;

    let (base, client, _server) = build_test_app(&upstream, tmp.path()).await;
    let response = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&chat_body())
        .send()
        .await
        .unwrap();

    assert_eq!(response.status().as_u16(), 502);
    assert_eq!(app_calls.load(Ordering::SeqCst), 2);
    assert_eq!(post_calls.load(Ordering::SeqCst), 1);
}

/// E2-3: A 429 from upstream is forwarded as 429 to the client;
/// no 405 bootstrap-retry logic fires.
#[tokio::test]
async fn drill_429_forwarded_without_retry() {
    let upstream = MockServer::start().await;
    let tmp = TempDir::new().unwrap();

    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(APP_HTML))
        .mount(&upstream)
        .await;

    let call_count = Arc::new(AtomicU32::new(0));
    let call_count2 = call_count.clone();
    Mock::given(method("POST"))
        .and(path_regex(r"/.*StreamGenerate.*"))
        .respond_with(move |_: &wiremock::Request| {
            call_count2.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(429)
        })
        .mount(&upstream)
        .await;

    let (base, client, _h) = build_test_app(&upstream, tmp.path()).await;

    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&chat_body())
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status().as_u16(),
        429,
        "429 must be forwarded to client"
    );

    // No retry for 429
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "StreamGenerate must be called exactly once on 429"
    );
}

/// E2-4: When the session is `NeedsReauth`, the readiness endpoint reports it
/// as 503 and the bridge does not enter a crash loop.
#[tokio::test]
async fn drill_cookie_expiry_surfaces_needs_reauth() {
    let upstream = MockServer::start().await;
    let tmp = TempDir::new().unwrap();

    // /app returns 401 — triggers NeedsReauth
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&upstream)
        .await;

    let cfg = bridge_config(tmp.path());
    let cfg_arc = Arc::new(cfg.clone());

    let identity = Arc::new(
        DefaultIdentityService::with_base_url(&cfg, Some(upstream.uri()))
            .expect("identity service"),
    );
    identity
        .import_credentials(
            "__Secure-1PSID=expired-psid; __Secure-1PSIDTS=expired-ts; SAPISID=expired-sapisid",
        )
        .await
        .unwrap();

    // Trigger bootstrap — this should set status = NeedsReauth
    let _ = identity.bootstrap().await; // error expected

    let health_admin = build_health_admin(Some(identity.clone() as Arc<dyn IdentityService>));

    let adapter = Arc::new(
        DefaultGeminiAdapter::with_base_url(identity.clone(), cfg_arc, upstream.uri())
            .expect("adapter"),
    );
    let app_state = AppState {
        adapter: adapter.clone() as Arc<dyn LlmAdapter>,
        upload_service: None,
        image_service: None,
        health_admin,
    };

    let (listener, port) = bind_listener().await;
    let _h = tokio::spawn(async move {
        axum::serve(
            listener,
            build_router(
                HttpServerConfig {
                    bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
                    api_key: None,
                    require_key_for_admin: false,
                    cors_enabled: false,
                    rate_limit: None,
                },
                app_state,
            ),
        )
        .await
        .unwrap();
    });

    let client = Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let readyz = client.get(format!("{base}/readyz")).send().await.unwrap();

    // Session is expired → readyz must return 503
    assert_eq!(
        readyz.status().as_u16(),
        503,
        "expired session should return 503 from /readyz"
    );

    let body: Value = readyz.json().await.unwrap();
    assert_eq!(
        body["session_status"], "needs_reauth",
        "session_status field must be 'needs_reauth'"
    );
}

/// E2-5: `refresh_1psidts` updates `psidts` in session state when the `/app`
/// response carries a `Set-Cookie: __Secure-1PSIDTS` header; session becomes Valid.
#[tokio::test]
async fn refresh_1psidts_updates_cookie_from_set_cookie_header() {
    let upstream = MockServer::start().await;
    let tmp = TempDir::new().unwrap();

    // /app returns success with a Set-Cookie carrying a new 1PSIDTS value
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(APP_HTML)
                .insert_header(
                    "set-cookie",
                    "__Secure-1PSIDTS=new-rotated-value; Path=/; Secure; HttpOnly",
                ),
        )
        .mount(&upstream)
        .await;

    let cfg = bridge_config(tmp.path());
    let identity = DefaultIdentityService::with_base_url(&cfg, Some(upstream.uri())).unwrap();
    identity
        .import_credentials(
            "__Secure-1PSID=fake-psid; __Secure-1PSIDTS=old-value; SAPISID=fake-sapisid",
        )
        .await
        .unwrap();

    // Rotation should succeed and update psidts in state
    identity
        .refresh_1psidts()
        .await
        .expect("refresh_1psidts must succeed");

    // The snapshot should show the session is now Valid
    let snap = identity.snapshot().await;
    assert_eq!(
        snap.status,
        SessionStatus::Valid,
        "session should be Valid after rotation"
    );

    // Verify the new psidts is reflected in auth headers (Cookie header must contain new-rotated-value)
    let mut headers = axum::http::HeaderMap::new();
    identity.apply_auth_headers(&mut headers).unwrap();
    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        cookie.contains("new-rotated-value"),
        "cookie header must contain refreshed 1PSIDTS value; got: {cookie}"
    );
}

/// E2-6: `refresh_1psidts` transitions to `NeedsReauth` when bootstrap fails.
#[tokio::test]
async fn refresh_1psidts_sets_needs_reauth_on_failure() {
    let upstream = MockServer::start().await;
    let tmp = TempDir::new().unwrap();

    // /app returns 401 — rotation cannot succeed
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&upstream)
        .await;

    let cfg = bridge_config(tmp.path());
    let identity = DefaultIdentityService::with_base_url(&cfg, Some(upstream.uri())).unwrap();
    identity
        .import_credentials(
            "__Secure-1PSID=fake-psid; __Secure-1PSIDTS=old-value; SAPISID=fake-sapisid",
        )
        .await
        .unwrap();

    let result = identity.refresh_1psidts().await;
    assert!(
        result.is_err(),
        "refresh_1psidts must fail when /app returns 401"
    );

    let snap = identity.snapshot().await;
    assert_eq!(
        snap.status,
        SessionStatus::NeedsReauth,
        "session must be NeedsReauth after failed rotation"
    );
}

/// E2-7: concurrent `refresh_1psidts` callers are coalesced — only one rotation
/// request is issued to `/app` regardless of how many concurrent callers there are.
#[tokio::test]
async fn refresh_1psidts_concurrent_calls_are_coalesced() {
    let upstream = MockServer::start().await;
    let tmp = TempDir::new().unwrap();

    // /app returns exactly once — coalescing means exactly one request.
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(APP_HTML)
                .insert_header(
                    "set-cookie",
                    "__Secure-1PSIDTS=coalesced-value; Path=/; Secure",
                ),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let cfg = bridge_config(tmp.path());
    let identity =
        Arc::new(DefaultIdentityService::with_base_url(&cfg, Some(upstream.uri())).unwrap());
    identity
        .import_credentials(
            "__Secure-1PSID=fake-psid; __Secure-1PSIDTS=old-value; SAPISID=fake-sapisid",
        )
        .await
        .unwrap();

    // Fire two concurrent refreshes.
    let id1 = identity.clone();
    let id2 = identity.clone();
    let (r1, r2) = tokio::join!(id1.refresh_1psidts(), id2.refresh_1psidts());

    // Both must succeed (the second one coalesces onto the first).
    assert!(r1.is_ok(), "first refresh must succeed");
    assert!(r2.is_ok(), "second (coalesced) refresh must succeed");

    // Session is valid after coalesced refresh.
    let snap = identity.snapshot().await;
    assert_eq!(snap.status, SessionStatus::Valid);

    // WireMock expectation enforces exactly-1 upstream call.
    upstream.verify().await;
}
