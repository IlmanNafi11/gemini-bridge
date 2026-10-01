//! End-to-end tests for health, readiness, admin status, and reauth surfaces (Task 1.5).

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
    Completion, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
};
use serde_json::Value;
use tokio::task::JoinHandle;

// ── Minimal mock IdentityService ──────────────────────────────────────────────

struct MockIdentity {
    status: SessionStatus,
    build_label: Option<String>,
    cookie_age: Option<Duration>,
    /// If Some, `import_credentials` + `bootstrap` succeed and return this status.
    reauth_result: Option<Result<SessionStatus, IdentityError>>,
}

impl MockIdentity {
    fn valid() -> Self {
        Self {
            status: SessionStatus::Valid,
            build_label: Some("20240101abcd".to_string()),
            cookie_age: Some(Duration::from_secs(3600)),
            reauth_result: None,
        }
    }

    fn needs_reauth() -> Self {
        Self {
            status: SessionStatus::NeedsReauth,
            build_label: None,
            cookie_age: Some(Duration::from_secs(999_999)),
            reauth_result: None,
        }
    }

    fn unconfigured() -> Self {
        Self {
            status: SessionStatus::Unconfigured,
            build_label: None,
            cookie_age: None,
            reauth_result: None,
        }
    }

    fn with_reauth_success(mut self) -> Self {
        self.reauth_result = Some(Ok(SessionStatus::Valid));
        self
    }

    fn with_reauth_failure(mut self) -> Self {
        self.reauth_result = Some(Err(IdentityError::Transport));
        self
    }
}

#[async_trait]
impl IdentityService for MockIdentity {
    async fn bootstrap(&self) -> Result<SessionBootstrap, IdentityError> {
        match &self.reauth_result {
            Some(Ok(_)) => Ok(SessionBootstrap {
                bl: "new_bl".to_string(),
                snlm0e: "snlm0e".to_string(),
                fsid: "fsid".to_string(),
            }),
            Some(Err(_)) => Err(IdentityError::NeedsReauth),
            None => Err(IdentityError::NeedsReauth),
        }
    }

    async fn refresh_1psidts(&self) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            status: self.status,
            build_label: self.build_label.clone(),
            cookie_age: self.cookie_age,
            checked_at: std::time::SystemTime::now(),
        }
    }

    fn apply_auth_headers(&self, _headers: &mut HeaderMap) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn import_credentials(&self, _raw: &str) -> Result<(), IdentityError> {
        match &self.reauth_result {
            Some(Ok(_)) => Ok(()),
            Some(Err(_)) => Err(IdentityError::NeedsReauth),
            None => Err(IdentityError::NeedsReauth),
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

fn build_test_router(
    identity: Arc<dyn IdentityService>,
    api_key: Option<String>,
) -> (axum::Router, u16) {
    let port = free_port();
    let cfg = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key,
        require_key_for_admin: true,
        cors_enabled: false,
        rate_limit: None,
    };
    // Use the NullLlmAdapter placeholder: build_router needs an adapter field.
    let adapter = Arc::new(NullAdapter);
    let state = AppState {
        adapter,
        upload_service: None,
        image_service: None,
        health_admin: gemini_bridge_http_server::build_health_admin(Some(identity)),
        conversation_store: None,
        tool_engine: gemini_bridge_http_server::build_tool_engine(),
        gallery_service: None,
        media_purge: None,
    };
    (build_router(cfg, state), port)
}

struct NullAdapter;

#[async_trait]
impl LlmAdapter for NullAdapter {
    fn provider_id(&self) -> &'static str {
        "null"
    }

    async fn complete(&self, _req: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Err(LlmError::Unavailable)
    }

    async fn complete_raw(&self, _req: Arc<LlmRequest>) -> Result<serde_json::Value, LlmError> {
        Err(LlmError::Unavailable)
    }

    async fn stream(&self, _req: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        let empty: Pin<Box<dyn futures::Stream<Item = Result<LlmEvent, LlmError>> + Send>> =
            Box::pin(stream::empty());
        Ok(empty)
    }
}

// ── Tests: /healthz ───────────────────────────────────────────────────────────

#[tokio::test]
async fn healthz_returns_200_with_process_info() {
    let identity = Arc::new(MockIdentity::valid());
    let (router, port) = build_test_router(identity, None);
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200, "/healthz must always return 200");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(
        body["uptime_secs"].as_u64().is_some(),
        "uptime_secs must be a number"
    );
    assert!(
        body["version"].as_str().is_some(),
        "version must be present"
    );
}

#[tokio::test]
async fn healthz_is_public_even_with_api_key_configured() {
    let identity = Arc::new(MockIdentity::unconfigured());
    let (router, port) = build_test_router(identity, Some("secret".to_string()));
    let _server = spawn_server(router, port).await;

    // No Authorization header — must still get 200
    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn healthz_uptime_is_non_decreasing() {
    let identity = Arc::new(MockIdentity::valid());
    let (router, port) = build_test_router(identity, None);
    let _server = spawn_server(router, port).await;

    let client = reqwest::Client::new();
    let b1: Value = client
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let b2: Value = client
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let u1 = b1["uptime_secs"].as_u64().unwrap();
    let u2 = b2["uptime_secs"].as_u64().unwrap();
    assert!(u2 >= u1, "uptime must be non-decreasing: {u1} -> {u2}");
}

// ── Tests: /readyz ────────────────────────────────────────────────────────────

#[tokio::test]
async fn readyz_returns_200_for_valid_session() {
    let identity = Arc::new(MockIdentity::valid());
    let (router, port) = build_test_router(identity, None);
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/readyz"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session_status"], "valid");
    assert!(body["build_label"].as_str().is_some());
    assert!(body["cookie_age_secs"].as_u64().is_some());
    assert!(body["checked_at"].as_i64().is_some());
}

#[tokio::test]
async fn readyz_returns_503_for_needs_reauth() {
    let identity = Arc::new(MockIdentity::needs_reauth());
    let (router, port) = build_test_router(identity, None);
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/readyz"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 503);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session_status"], "needs_reauth");
}

#[tokio::test]
async fn readyz_returns_503_for_unconfigured() {
    let identity = Arc::new(MockIdentity::unconfigured());
    let (router, port) = build_test_router(identity, None);
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/readyz"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 503);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session_status"], "unconfigured");
}

#[tokio::test]
async fn readyz_returns_503_for_ip_flagged() {
    struct IpFlaggedIdentity;
    #[async_trait]
    impl IdentityService for IpFlaggedIdentity {
        async fn bootstrap(&self) -> Result<SessionBootstrap, IdentityError> {
            Err(IdentityError::IpFlagged)
        }
        async fn refresh_1psidts(&self) -> Result<(), IdentityError> {
            Ok(())
        }
        async fn snapshot(&self) -> SessionSnapshot {
            SessionSnapshot {
                status: SessionStatus::IpFlagged,
                build_label: None,
                cookie_age: None,
                checked_at: std::time::SystemTime::now(),
            }
        }
        fn apply_auth_headers(&self, _h: &mut HeaderMap) -> Result<(), IdentityError> {
            Ok(())
        }
        async fn import_credentials(&self, _r: &str) -> Result<(), IdentityError> {
            Err(IdentityError::NeedsReauth)
        }
    }

    let identity = Arc::new(IpFlaggedIdentity);
    let (router, port) = build_test_router(identity, None);
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/readyz"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 503);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session_status"], "ip_flagged");
}

#[tokio::test]
async fn readyz_is_public_even_with_api_key_configured() {
    let identity = Arc::new(MockIdentity::valid());
    let (router, port) = build_test_router(identity, Some("secret".to_string()));
    let _server = spawn_server(router, port).await;

    // No auth header
    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/readyz"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
}

// ── Tests: /admin/status ──────────────────────────────────────────────────────

#[tokio::test]
async fn admin_status_returns_401_without_api_key() {
    // api_key is None but require_key_for_admin is true → must refuse with 401
    let identity = Arc::new(MockIdentity::valid());
    let (router, port) = build_test_router(identity, None);
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/admin/status"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn admin_status_returns_401_with_wrong_key() {
    let identity = Arc::new(MockIdentity::valid());
    let (router, port) = build_test_router(identity, Some("correct".to_string()));
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/admin/status"))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn admin_status_returns_200_with_correct_key() {
    let identity = Arc::new(MockIdentity::valid());
    let (router, port) = build_test_router(identity, Some("mykey".to_string()));
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/admin/status"))
        .bearer_auth("mykey")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session_status"], "valid");
    assert!(body["build_label"].as_str().is_some());
    assert!(body["checked_at"].as_i64().is_some());
    // Must not contain raw cookie or secret fields
    let s = body.to_string();
    assert!(
        !s.contains("psid"),
        "raw credential must not appear in response"
    );
}

// ── Tests: /admin/reauth ──────────────────────────────────────────────────────

#[tokio::test]
async fn admin_reauth_returns_401_without_key() {
    let identity = Arc::new(MockIdentity::needs_reauth().with_reauth_success());
    let (router, port) = build_test_router(identity, Some("k".to_string()));
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/reauth"))
        .json(
            &serde_json::json!({"cookie": "__Secure-1PSID=abc; __Secure-1PSIDTS=def; SAPISID=ghi"}),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn admin_reauth_succeeds_with_valid_cookie_and_key() {
    let identity = Arc::new(MockIdentity::needs_reauth().with_reauth_success());
    let (router, port) = build_test_router(identity, Some("k".to_string()));
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/reauth"))
        .bearer_auth("k")
        .json(
            &serde_json::json!({"cookie": "__Secure-1PSID=abc; __Secure-1PSIDTS=def; SAPISID=ghi"}),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    // session_status should reflect the new state after bootstrap
    assert!(body["session_status"].as_str().is_some());
}

#[tokio::test]
async fn admin_reauth_returns_error_on_failure() {
    let identity = Arc::new(MockIdentity::needs_reauth().with_reauth_failure());
    let (router, port) = build_test_router(identity, Some("k".to_string()));
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/reauth"))
        .bearer_auth("k")
        .json(
            &serde_json::json!({"cookie": "__Secure-1PSID=abc; __Secure-1PSIDTS=def; SAPISID=ghi"}),
        )
        .send()
        .await
        .unwrap();

    // Should be an error status (400 or 502) with JSON body
    assert!(resp.status().is_client_error() || resp.status().is_server_error());
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_some() || body.get("session_status").is_some());
}

#[tokio::test]
async fn admin_reauth_rejects_missing_cookie_field() {
    let identity = Arc::new(MockIdentity::needs_reauth().with_reauth_success());
    let (router, port) = build_test_router(identity, Some("k".to_string()));
    let _server = spawn_server(router, port).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/reauth"))
        .bearer_auth("k")
        .json(&serde_json::json!({})) // no "cookie" field
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}
