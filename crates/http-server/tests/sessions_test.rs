//! V.4 `/v1/sessions` route tests (RED first).
//!
//! Contract (SPEC-session-vault.md §6.1, tasks/todo.md V.4):
//! - `POST /v1/sessions` (api_key only) parses the cookie, probes `/app`,
//!   persists encrypted, returns `{id, token, session_status}` exactly once.
//!   Re-POST with the same label replaces atomically; a failed probe leaves
//!   the prior session untouched.
//! - `GET /v1/sessions/current` (api_key + token) returns only the caller's
//!   session state.
//! - `DELETE /v1/sessions/{id}` (api_key + token matching `{id}`) purges the
//!   row; subsequent requests with that token → `401 invalid_session_token`.
//! - Cross-access (token of A against id of B) is rejected; unknown id → not
//!   found semantics, never another tenant's data.
//! - Wrong/missing api_key → `401 invalid_api_key`; the create route is not
//!   session-token gated.
//!
//! Style follows `auth_matrix_test.rs`: real TCP listener, wiremock upstream,
//! per-test data dir.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use common::COOKIE_OK;
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_llm_service::{Completion, LlmAdapter, LlmError, LlmEventStream, LlmRequest};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::{Duration, sleep};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Serializes tests that mutate the process-wide `BRIDGE_SECRET`.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const API_KEY: &str = "test-api-key-that-is-long-enough-32chars";

/// Probe-failure HTML is irrelevant; failure tests respond 401 on `/app`.
const SAMPLE_GEMINI_HTML: &str = r#"
<!doctype html>
<html>
<script>
window.WIZ_global_data = {
  "cfb2h": "boq_assistant-bard-web-server_20240101.00_p0",
  "SNlM0e": "AIzaSyFakeTokenForTesting1234567890",
  "FdrFJe": "1234567890123456789"
};
</script>
</head>
<body>
</body>
</html>
"#;

struct RecordingAdapter {
    hits: AtomicUsize,
}

#[async_trait]
impl LlmAdapter for RecordingAdapter {
    fn provider_id(&self) -> &'static str {
        "sessions-test"
    }
    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(Completion {
            text: "ok".to_owned(),
            finish_reason: "stop".to_owned(),
            usage: None,
            metadata: None,
        })
    }
    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Err(LlmError::Unavailable)
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind free port")
        .local_addr()
        .unwrap()
        .port()
}

struct Spawned {
    port: u16,
    /// Base URL of the running bridge, e.g. `http://127.0.0.1:PORT`.
    base_url: String,
    adapter: Arc<RecordingAdapter>,
    vault: Arc<gemini_bridge_identity::VaultService>,
}

/// Start the bridge with a vault and an upstream whose `/app` returns
/// `SAMPLE_GEMINI_HTML` `probe_times` times, then 401 afterwards.
async fn spawn_with_probe_budget(probe_times: u64) -> (Spawned, MockServer) {
    let _guard = ENV_LOCK.lock().await;
    // SAFETY: env_lock serializes all vault construction in this process.
    unsafe { std::env::set_var("BRIDGE_SECRET", "sessions-test-secret-0123456789-abcdef") };

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_GEMINI_HTML))
        .up_to_n_times(probe_times)
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&upstream)
        .await;

    let port = free_port();
    let data_dir = std::env::temp_dir().join(format!("sessions-test-{port}"));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let config = Arc::new(gemini_bridge_config::BridgeConfig {
        server: gemini_bridge_config::ServerConfig {
            bind_addr: "127.0.0.1".to_owned(),
            port,
            api_key: Some(API_KEY.to_owned()),
            ..gemini_bridge_config::ServerConfig::default()
        },
        storage: gemini_bridge_config::StorageConfig {
            data_dir: data_dir.clone(),
            media_ttl_days: 30,
        },
        transport: gemini_bridge_config::TransportConfig {
            tls_profile: "chrome".to_owned(),
            proxy_url: None,
            timeout_secs: 5,
            stream_idle_timeout_secs: 30,
        },
        video: Default::default(),
    });

    let vault = Arc::new(
        gemini_bridge_identity::VaultService::new(&config, Some(upstream.uri()))
            .expect("vault service"),
    );

    // Drop BRIDGE_SECRET immediately — it must not be live during test execution.
    // SAFETY: env_lock held until after removal.
    unsafe { std::env::remove_var("BRIDGE_SECRET") };

    let adapter = Arc::new(RecordingAdapter {
        hits: AtomicUsize::new(0),
    });
    let state = AppState {
        adapter: adapter.clone(),
        upload_service: None,
        image_service: None,
        video_service: None,
        health_admin: gemini_bridge_http_server::build_health_admin(None),
        identity_service: None,
        vault: Some(vault.clone()),
        conversation_store: None,
        tool_engine: gemini_bridge_http_server::build_tool_engine(),
        gallery_service: None,
        media_purge: None,
    };
    let srv_config = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key: Some(API_KEY.to_owned()),
        require_key_for_admin: false,
        cors_enabled: false,
        rate_limit: None,
        metrics_enabled: false,
    };
    let router = build_router(srv_config, state);
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).await.unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // Give the server a moment to bind so the first reqwest call is never refused.
    sleep(Duration::from_millis(50)).await;

    let spawned = Spawned {
        port,
        base_url: format!("http://127.0.0.1:{port}"),
        adapter,
        vault,
    };
    (spawned, upstream)
}

async fn spawn() -> (Spawned, MockServer) {
    spawn_with_probe_budget(u64::MAX).await
}

// ── POST /v1/sessions ─────────────────────────────────────────────────────────

/// Happy path: create returns `{id, token, session_status}` with a `ses_`-prefixed
/// token; the token then authenticates a chat request (dual-auth loop).
#[tokio::test]
async fn create_session_returns_id_token_status_and_token_authenticates_chat() {
    let (spawned, _upstream) = spawn().await;

    let (status, body) = common::create_session(
        &spawned.base_url,
        API_KEY,
        COOKIE_OK,
        Some("client-a"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "body: {body}");
    assert_eq!(body["session_status"], "valid");
    let token = body["token"].as_str().expect("token present");
    assert!(token.starts_with("ses_"), "token shape: {token}");
    assert!(
        body["id"].is_string() && !body["id"].as_str().unwrap().is_empty(),
        "id present"
    );

    // The returned token authenticates a chat request.
    let chat = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", spawned.base_url))
        .bearer_auth(API_KEY)
        .header("x-session-token", token)
        .json(&json!({"model": "gemini-web-flash", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .expect("chat request succeeds");
    assert_eq!(chat.status(), reqwest::StatusCode::OK);
    let parsed: Value = chat.json().await.unwrap();
    assert_eq!(parsed["choices"][0]["message"]["content"], "ok");
    assert_eq!(spawned.adapter.hits.load(Ordering::SeqCst), 1);
}

/// Create requires the api_key; without it → `401 invalid_api_key`.
#[tokio::test]
async fn create_session_requires_api_key() {
    let (spawned, _upstream) = spawn().await;

    let response = reqwest::Client::new()
        .post(format!("{}/v1/sessions", spawned.base_url))
        .json(&json!({"cookie": COOKIE_OK}))
        .send()
        .await
        .expect("request succeeds");
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_api_key");
}

/// Probe failure (upstream 401) → error response, and the prior session row is
/// left completely untouched (status and stored cookie preserved).
#[tokio::test]
async fn failed_probe_returns_error_and_preserves_prior_session() {
    // Budget 1: the first create consumes the OK probe; the re-POST hits 401.
    let (spawned, _upstream) = spawn_with_probe_budget(1).await;

    let (status, body) = common::create_session(
        &spawned.base_url,
        API_KEY,
        COOKIE_OK,
        Some("client-a"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "body: {body}");
    let token = body["token"].as_str().unwrap().to_owned();
    let id = body["id"].as_str().unwrap().to_owned();

    // Re-POST with an invalid-probe cookie → 4xx with an error body.
    let (repost_status, repost_body) = common::create_session(
        &spawned.base_url,
        API_KEY,
        COOKIE_OK,
        Some("client-a"),
    )
    .await;
    assert!(
        repost_status.is_client_error() || repost_status.is_server_error(),
        "probe failure must be an error, got {repost_status}"
    );
    assert!(
        repost_body["error"].is_object(),
        "error envelope expected: {repost_body}"
    );

    // Prior session untouched: token still authenticates, status still valid.
    let current = reqwest::Client::new()
        .get(format!("{}/v1/sessions/current", spawned.base_url))
        .bearer_auth(API_KEY)
        .header("x-session-token", &token)
        .send()
        .await
        .expect("current request succeeds");
    assert_eq!(current.status(), reqwest::StatusCode::OK);
    let current_body: Value = current.json().await.unwrap();
    assert_eq!(current_body["session_status"], "valid");

    // And the stored id matches the original create.
    assert_eq!(spawned.vault.store().get(&id).unwrap().unwrap().id, id);
}

// ── GET /v1/sessions/current ──────────────────────────────────────────────────

/// Current returns only the caller's session state.
#[tokio::test]
async fn current_returns_caller_session_state_only() {
    let (spawned, _upstream) = spawn().await;

    let (status, body) = common::create_session(&spawned.base_url, API_KEY, COOKIE_OK, None).await;
    assert_eq!(status, reqwest::StatusCode::OK, "body: {body}");
    let token = body["token"].as_str().unwrap().to_owned();
    let id = body["id"].as_str().unwrap().to_owned();

    let response = reqwest::Client::new()
        .get(format!("{}/v1/sessions/current", spawned.base_url))
        .bearer_auth(API_KEY)
        .header("x-session-token", &token)
        .send()
        .await
        .expect("current request succeeds");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let current: Value = response.json().await.unwrap();
    assert_eq!(current["session_status"], "valid");
    assert_eq!(current["id"], id);
    // Never expose secret material.
    assert!(current.get("cookie").is_none());
    assert!(current.get("token").is_none());
}

// ── DELETE /v1/sessions/{id} ──────────────────────────────────────────────────

/// Delete invalidates the token: subsequent requests → `401 invalid_session_token`.
#[tokio::test]
async fn delete_invalidates_token_for_subsequent_requests() {
    let (spawned, _upstream) = spawn().await;

    let (status, body) = common::create_session(&spawned.base_url, API_KEY, COOKIE_OK, None).await;
    assert_eq!(status, reqwest::StatusCode::OK, "body: {body}");
    let token = body["token"].as_str().unwrap().to_owned();
    let id = body["id"].as_str().unwrap().to_owned();

    let response = reqwest::Client::new()
        .delete(format!("{}/v1/sessions/{}", spawned.base_url, id))
        .bearer_auth(API_KEY)
        .header("x-session-token", &token)
        .send()
        .await
        .expect("delete request succeeds");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "delete should succeed"
    );

    // Token no longer resolves anywhere.
    let chat = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", spawned.base_url))
        .bearer_auth(API_KEY)
        .header("x-session-token", &token)
        .json(&json!({"model": "gemini-web-flash", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .expect("chat request succeeds");
    assert_eq!(chat.status(), reqwest::StatusCode::UNAUTHORIZED);
    let chat_body: Value = chat.json().await.unwrap();
    assert_eq!(chat_body["error"]["code"], "invalid_session_token");

    // current likewise fails.
    let current = reqwest::Client::new()
        .get(format!("{}/v1/sessions/current", spawned.base_url))
        .bearer_auth(API_KEY)
        .header("x-session-token", &token)
        .send()
        .await
        .expect("current request succeeds");
    assert_eq!(current.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// Cross-tenant access is rejected: tenant B's token cannot delete tenant A's
/// session, and an unknown id never returns another tenant's data.
#[tokio::test]
async fn cross_tenant_delete_is_rejected() {
    let (spawned, _upstream) = spawn().await;

    let (_, body_a) = common::create_session(&spawned.base_url, API_KEY, COOKIE_OK, Some("a")).await;
    let (_, body_b) = common::create_session(&spawned.base_url, API_KEY, COOKIE_OK, Some("b")).await;
    let token_a = body_a["token"].as_str().unwrap().to_owned();
    let id_b = body_b["id"].as_str().unwrap().to_owned();

    // A's token cannot delete B's session.
    let response = reqwest::Client::new()
        .delete(format!("{}/v1/sessions/{}", spawned.base_url, id_b))
        .bearer_auth(API_KEY)
        .header("x-session-token", &token_a)
        .send()
        .await
        .expect("cross delete request succeeds");
    assert!(
        response.status().is_client_error(),
        "cross-tenant delete must be rejected, got {}",
        response.status()
    );

    // B is still usable afterwards (its row was untouched).
    let current = reqwest::Client::new()
        .get(format!("{}/v1/sessions/current", spawned.base_url))
        .bearer_auth(API_KEY)
        .header("x-session-token", body_b["token"].as_str().unwrap())
        .send()
        .await
        .expect("current request succeeds");
    assert_eq!(current.status(), reqwest::StatusCode::OK);
}

/// Deleting an unknown id is a not-found-style rejection, not a 500.
#[tokio::test]
async fn delete_unknown_id_is_rejected_not_500() {
    let (spawned, _upstream) = spawn().await;

    let (_, body) = common::create_session(&spawned.base_url, API_KEY, COOKIE_OK, None).await;
    let token = body["token"].as_str().unwrap().to_owned();

    let response = reqwest::Client::new()
        .delete(format!("{}/v1/sessions/ses-does-not-exist", spawned.base_url))
        .bearer_auth(API_KEY)
        .header("x-session-token", &token)
        .send()
        .await
        .expect("delete request succeeds");
    assert!(
        response.status().is_client_error(),
        "unknown id must be rejected, got {}",
        response.status()
    );
}
