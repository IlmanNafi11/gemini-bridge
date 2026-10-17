//! V.3 dual-auth middleware auth-matrix tests (RED first).
//!
//! Contract (SPEC-session-vault.md §4/§5/§10):
//! - Protected `/v1/*` routes require BOTH the configured `api_key` AND a
//!   valid `X-Session-Token` naming a vault session.
//! - Missing/wrong api_key  → `401 invalid_api_key`  (existing behavior kept).
//! - Missing session token  → `401 invalid_session_token`.
//! - Wrong/unknown token    → `401 invalid_session_token`.
//! - A session row in `needs_reauth` → `401 needs_reauth`.
//! - Auth failures short-circuit before any upstream call (zero hits).
//! - Rate limiting buckets are per-session-token when one is presented.
//!
//! These tests drive `build_router` through a real TCP listener, matching the
//! e2e style used by `tests/e2e_chat_test.rs`.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_llm_service::{Completion, LlmAdapter, LlmError, LlmEventStream, LlmRequest};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::{Duration, sleep};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Serializes tests that mutate the process-wide `BRIDGE_SECRET`.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const API_KEY: &str = "test-api-key-that-is-long-enough-32chars";
const COOKIE_OK: &str = "__Secure-1PSID=psid-a; __Secure-1PSIDTS=ts-a; SAPISID=sapisid-a";

/// Upstream adapter that records call counts via `Arc`; the auth matrix must
/// never reach it, so zero hits on auth failures proves short-circuit.
struct RecordingAdapter {
    hits: AtomicUsize,
}

#[async_trait]
impl LlmAdapter for RecordingAdapter {
    fn provider_id(&self) -> &'static str {
        "auth-matrix-test"
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

/// Minimal wiremock that always returns 200 so the vault service can probe.
/// The tests do NOT register any expectations on this mock — auth failures must
/// short-circuit before any upstream call, so zero hits on `upstream_mock`
/// (verified by the mock's `mount()` call doing nothing on `verify()`) proves it.
async fn upstream_mock() -> MockServer {
    MockServer::start().await
}

/// Build a server with an api_key and a vault seeded with one `Valid` session
/// and one `NeedsReauth` session. `BRIDGE_SECRET` is set only during vault init
/// and removed immediately after so it never bleeds into other tests.
async fn spawn_with_vault() -> Spawned {
    spawn_with_vault_with_rate_limit(None).await
}

/// Variant for the per-session rate-limit isolation test.
async fn spawn_with_vault_with_rate_limit(
    rate_limit: Option<gemini_bridge_http_server::TokenBucketConfig>,
) -> Spawned {
    let _guard = ENV_LOCK.lock().await;
    // SAFETY: env_lock serializes all vault construction in this process.
    unsafe { std::env::set_var("BRIDGE_SECRET", "auth-matrix-test-secret-0123456789") };

    let port = free_port();
    let data_dir = std::env::temp_dir().join(format!("auth-matrix-test-{port}"));
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

    // Vault-backed identity: the dual-auth middleware looks up X-Session-Token here.
    let vault =
        Arc::new(gemini_bridge_identity::VaultService::new(&config, None).expect("vault service"));
    let credentials = gemini_bridge_identity::session::parse_cookie_header(COOKIE_OK)
        .expect("test cookie parses");

    let valid_token = gemini_bridge_identity::generate_session_token();
    let reauth_token = gemini_bridge_identity::generate_session_token();
    {
        let mut store = vault.store();
        store
            .upsert_session(gemini_bridge_identity::NewVaultSession {
                id: "sess-valid",
                token_hash: &gemini_bridge_identity::hash_session_token(&valid_token),
                label: None,
                credentials: &credentials,
                status: gemini_bridge_identity::SessionStatus::Valid,
            })
            .expect("seed valid session");
        store
            .upsert_session(gemini_bridge_identity::NewVaultSession {
                id: "sess-reauth",
                token_hash: &gemini_bridge_identity::hash_session_token(&reauth_token),
                label: None,
                credentials: &credentials,
                status: gemini_bridge_identity::SessionStatus::NeedsReauth,
            })
            .expect("seed needs_reauth session");
    }

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
        rate_limit,
        metrics_enabled: false,
    };
    let router = build_router(srv_config.clone(), state);
    let listener = TcpListener::bind(&srv_config.bind_addr).await.unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // Give the server a moment to bind so the first reqwest call is never refused.
    sleep(Duration::from_millis(50)).await;

    Spawned {
        port,
        adapter,
        sessions: vec![
            ("sess-valid".to_owned(), valid_token),
            ("sess-reauth".to_owned(), reauth_token),
        ],
        vault,
    }
}

struct Spawned {
    port: u16,
    adapter: Arc<RecordingAdapter>,
    /// (session id, raw session token) pairs seeded into the vault.
    sessions: Vec<(String, String)>,
    /// Handle so tests can seed extra rows after startup.
    vault: Arc<gemini_bridge_identity::VaultService>,
}

fn chat_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1/chat/completions")
}

fn chat_body() -> Value {
    json!({"model": "gemini-web-flash", "messages": [{"role": "user", "content": "hi"}]})
}

/// Fire a POST /v1/chat/completions with optional auth headers.
async fn post_chat(
    port: u16,
    api_key: Option<&str>,
    session_token: Option<&str>,
) -> (reqwest::StatusCode, Value) {
    let client = reqwest::Client::new();
    let mut req = client.post(chat_url(port)).json(&chat_body());
    if let Some(key) = api_key {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    if let Some(token) = session_token {
        req = req.header("x-session-token", token);
    }
    let resp = req.send().await.expect("request succeeds");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(json!({"parse_error": true}));
    (status, body)
}

async fn hits_are_zero(spawned: &Spawned) {
    assert_eq!(
        spawned.adapter.hits.load(Ordering::SeqCst),
        0,
        "auth failure must short-circuit before any upstream call"
    );
}

// ── Auth matrix ───────────────────────────────────────────────────────────────

/// No headers at all → api_key is missing → existing middleware returns
/// `401 invalid_api_key` (this already works; confirm it is preserved).
#[tokio::test]
async fn no_api_key_is_rejected_with_invalid_api_key_before_session_check() {
    let spawned = spawn_with_vault().await;
    let (status, body) = post_chat(spawned.port, None, None).await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["type"], "authentication_error");
    assert_eq!(body["error"]["code"], "invalid_api_key");
    hits_are_zero(&spawned).await;
}

/// Correct api_key but no `X-Session-Token` → `401 invalid_session_token`.
#[tokio::test]
async fn api_key_without_session_token_is_rejected_with_invalid_session_token() {
    let spawned = spawn_with_vault().await;
    let (status, body) = post_chat(spawned.port, Some(API_KEY), None).await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["type"], "authentication_error");
    assert_eq!(body["error"]["code"], "invalid_session_token");
    hits_are_zero(&spawned).await;
}

/// Correct api_key but unknown token → `401 invalid_session_token`.
#[tokio::test]
async fn api_key_with_unknown_token_is_rejected_with_invalid_session_token() {
    let spawned = spawn_with_vault().await;
    let (status, body) = post_chat(spawned.port, Some(API_KEY), Some("tok-unknown")).await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["type"], "authentication_error");
    assert_eq!(body["error"]["code"], "invalid_session_token");
    hits_are_zero(&spawned).await;
}

/// Wrong api_key with a valid session token → api_key check fires first
/// → `401 invalid_api_key` (api_key is the outer gate).
#[tokio::test]
async fn wrong_api_key_with_valid_token_is_rejected_with_invalid_api_key() {
    let spawned = spawn_with_vault().await;
    let valid = &spawned.sessions[0].1;
    let (status, body) = post_chat(
        spawned.port,
        Some("wrong-key-32chars-here-xxx"),
        Some(valid),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "invalid_api_key");
    hits_are_zero(&spawned).await;
}

/// Valid api_key + token naming a `needs_reauth` row → fail-fast with
/// `401 needs_reauth` (no upstream call, session-level error surfaced).
#[tokio::test]
async fn needs_reauth_session_fails_fast_with_needs_reauth_code() {
    let spawned = spawn_with_vault().await;
    let reauth = &spawned.sessions[1].1;
    let (status, body) = post_chat(spawned.port, Some(API_KEY), Some(reauth)).await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["type"], "authentication_error");
    assert_eq!(body["error"]["code"], "needs_reauth");
    hits_are_zero(&spawned).await;
}

/// Happy path: valid api_key + valid token reaches the upstream adapter.
#[tokio::test]
async fn valid_api_key_and_valid_token_reaches_upstream_adapter() {
    let spawned = spawn_with_vault().await;
    let valid = &spawned.sessions[0].1;
    let (status, body) = post_chat(spawned.port, Some(API_KEY), Some(valid)).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
    assert_eq!(
        spawned.adapter.hits.load(Ordering::SeqCst),
        1,
        "upstream adapter must be called exactly once"
    );
}

/// Token-bound per-session rate limiting: exhausting session A's bucket must
/// not throttle session B — the bucket key is `{api_key_digest}:{session_id}`,
/// not a shared "authenticated" bucket.
#[tokio::test]
async fn rate_limit_bucket_is_per_session_token() {
    let spawned =
        spawn_with_vault_with_rate_limit(Some(gemini_bridge_http_server::TokenBucketConfig {
            capacity: 1,
            refill_tokens: 0,
            refill_interval: Duration::from_secs(60),
        }))
        .await;
    // Both sessions must be `Valid` for this test: bucket isolation is about
    // two live tenants, not the auth matrix's reauth probe subject.
    let token_a = &spawned.sessions[0].1;
    let token_b = {
        let extra = gemini_bridge_identity::generate_session_token();
        let credentials = gemini_bridge_identity::session::parse_cookie_header(COOKIE_OK)
            .expect("test cookie parses");
        spawned
            .vault
            .store()
            .upsert_session(gemini_bridge_identity::NewVaultSession {
                id: "sess-valid-b",
                token_hash: &gemini_bridge_identity::hash_session_token(&extra),
                label: None,
                credentials: &credentials,
                status: gemini_bridge_identity::SessionStatus::Valid,
            })
            .expect("seed second valid session");
        extra
    };
    // Session A's first request takes the only token in its bucket.
    let first = post_chat(spawned.port, Some(API_KEY), Some(token_a)).await;
    assert_eq!(first.0, reqwest::StatusCode::OK, "first call for A passes");

    // Session A's second call is throttled: its bucket is empty.
    let throttled = post_chat(spawned.port, Some(API_KEY), Some(token_a)).await;
    assert_eq!(
        throttled.0,
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "second call for A is throttled"
    );
    assert_eq!(throttled.1["error"]["code"], "rate_limit_exceeded");

    // Session B is a separate bucket: its first call still passes.
    let other = post_chat(spawned.port, Some(API_KEY), Some(&token_b)).await;
    assert_eq!(
        other.0,
        reqwest::StatusCode::OK,
        "session B must have its own bucket"
    );
}

// ── Legacy /admin paths (unchanged) ──────────────────────────────────────────

/// /admin/* routes must continue to require the admin api_key and must NOT be
/// gated by `X-Session-Token` (admin is session-scoped infrastructure).
#[tokio::test]
async fn admin_route_is_not_gated_by_session_token() {
    let upstream = upstream_mock().await;
    let port = free_port();

    let config = Arc::new(gemini_bridge_config::BridgeConfig {
        server: gemini_bridge_config::ServerConfig {
            bind_addr: "127.0.0.1".to_owned(),
            port,
            api_key: Some(API_KEY.to_owned()),
            ..gemini_bridge_config::ServerConfig::default()
        },
        storage: gemini_bridge_config::StorageConfig {
            data_dir: std::env::temp_dir().join(format!("auth-admin-test-{port}")),
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

    // Use DefaultIdentityService pointing at wiremock so the admin health
    // endpoint can answer.
    let identity = Arc::new(
        gemini_bridge_identity::DefaultIdentityService::with_base_url(
            &config,
            Some(upstream.uri()),
        )
        .expect("identity"),
    );

    let adapter = Arc::new(RecordingAdapter {
        hits: AtomicUsize::new(0),
    });
    let state = AppState {
        adapter,
        upload_service: None,
        image_service: None,
        video_service: None,
        health_admin: gemini_bridge_http_server::build_health_admin(Some(identity.clone())),
        identity_service: Some(identity.clone()),
        vault: None,
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
    let router = build_router(srv_config.clone(), state);
    let listener = TcpListener::bind(&srv_config.bind_addr).await.unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    sleep(Duration::from_millis(50)).await;

    // Wiremock: /readyz returns 200 so health_admin can answer.
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><script>var data = {"cfb2h":"l","FdrFJe":"f","SNlM0e":"s"};</script></html>"#,
        ))
        .mount(&upstream)
        .await;

    let client = reqwest::Client::new();

    // Admin route with api_key (no session token) → 200 or 503, never 401.
    let resp = client
        .get(format!("http://127.0.0.1:{port}/admin/status"))
        .header("authorization", format!("Bearer {API_KEY}"))
        .send()
        .await
        .expect("request succeeds");
    // /admin/status is gated by api_key only; it may return 503 if the
    // wiremock hasn't responded yet, but it must NOT be 401.
    assert_ne!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "/admin must not require X-Session-Token"
    );
}
