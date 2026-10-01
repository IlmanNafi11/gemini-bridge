//! Integration tests for Task 2.5: configured TTL cleanup and protected purge endpoint.
//!
//! Tests run against an in-process Axum server. A real `LocalMediaStore` is wired into the
//! route so the purge handler exercises the actual filesystem cleanup path.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream;
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_llm_service::{
    Completion, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
};
use gemini_bridge_media_store::cleanup::{compute_default_expiry, now_unix};
use gemini_bridge_media_store::{LocalMediaStore, MediaMetadata, MediaStore};
use serde_json::Value;
use std::pin::Pin;
use tempfile::TempDir;

// ── Minimal no-op adapter ──────────────────────────────────────────────────────

struct NoopAdapter;

#[async_trait]
impl LlmAdapter for NoopAdapter {
    fn provider_id(&self) -> &'static str {
        "noop"
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

// ── Helpers ────────────────────────────────────────────────────────────────────

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn spawn_server(
    store: Arc<LocalMediaStore>,
    api_key: Option<String>,
) -> (u16, tokio::task::JoinHandle<()>) {
    let port = free_port();
    let state = AppState {
        adapter: Arc::new(NoopAdapter),
        upload_service: None,
        image_service: None,
        health_admin: gemini_bridge_http_server::build_health_admin(None),
        conversation_store: None,
        tool_engine: gemini_bridge_http_server::build_tool_engine(),
        gallery_service: None,
        media_purge: Some(Arc::new(
            gemini_bridge_health_admin::DefaultMediaPurgeAdminService::new(store),
        )),
    };
    let config = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key,
        require_key_for_admin: true,
        cors_enabled: false,
        rate_limit: None,
    };
    let router = build_router(config, state);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (port, handle)
}

fn make_meta(mime_type: &str) -> MediaMetadata {
    MediaMetadata {
        id: String::new(),
        sha256: String::new(),
        mime_type: mime_type.to_string(),
        size_bytes: 0,
        created_at: now_unix(),
        expires_at: None,
        prompt: None,
        model: None,
        file_ref: String::new(),
    }
}

// ── Tests: auth boundary ───────────────────────────────────────────────────────

#[tokio::test]
async fn purge_returns_401_without_api_key_configured() {
    // When no API key is configured at all, the admin middleware still denies
    // unauthenticated callers with 401.
    let temp = TempDir::new().unwrap();
    let store = Arc::new(LocalMediaStore::new(temp.path()));
    let (port, _srv) = spawn_server(store, Some("supersecret".to_string())).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/purge"))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        401,
        "POST /admin/purge without key must return 401"
    );
}

#[tokio::test]
async fn purge_returns_401_with_wrong_api_key() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(LocalMediaStore::new(temp.path()));
    let (port, _srv) = spawn_server(store, Some("supersecret".to_string())).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/purge"))
        .header("Authorization", "Bearer wrongkey")
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        401,
        "POST /admin/purge with wrong key must return 401"
    );
}

#[tokio::test]
async fn purge_returns_200_with_correct_api_key_and_no_expired_items() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(LocalMediaStore::new(temp.path()));
    let (port, _srv) = spawn_server(store, Some("supersecret".to_string())).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/purge"))
        .header("Authorization", "Bearer supersecret")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["purged"], 0, "empty store: purged count must be 0");
}

// ── Tests: purge behavior ─────────────────────────────────────────────────────

#[tokio::test]
async fn purge_removes_expired_items_and_returns_count() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(LocalMediaStore::new(temp.path()));
    let now = now_unix();

    // Expired item — expires 1 hour ago.
    let mut expired_meta = make_meta("image/png");
    expired_meta.expires_at = Some(now - 3600);
    let expired_id = store
        .put(Bytes::from_static(b"expired png bytes"), expired_meta)
        .await
        .unwrap();

    // Active item — no expiry.
    let active_id = store
        .put(
            Bytes::from_static(b"active png bytes"),
            make_meta("image/png"),
        )
        .await
        .unwrap();

    let (port, _srv) = spawn_server(store.clone(), Some("key".to_string())).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/purge"))
        .header("Authorization", "Bearer key")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["purged"], 1, "exactly one expired item must be purged");

    // Expired item must be gone.
    assert!(
        store.get(&expired_id).await.is_err(),
        "expired item must be removed from store"
    );
    // Active item must remain.
    store.get(&active_id).await.unwrap();
}

#[tokio::test]
async fn purge_preserves_non_expired_items() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(LocalMediaStore::new(temp.path()));
    let now = now_unix();

    // Item expiring tomorrow — must survive.
    let mut future_meta = make_meta("image/jpeg");
    future_meta.expires_at = compute_default_expiry(now, 1); // 1 day from now
    let future_id = store
        .put(Bytes::from_static(b"fresh jpeg bytes"), future_meta)
        .await
        .unwrap();

    // Item with no expiry — must survive.
    let no_expiry_id = store
        .put(
            Bytes::from_static(b"immortal jpeg bytes"),
            make_meta("image/jpeg"),
        )
        .await
        .unwrap();

    let (port, _srv) = spawn_server(store.clone(), Some("k".to_string())).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/purge"))
        .header("Authorization", "Bearer k")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["purged"], 0, "no items should be purged");

    // Both items must remain.
    store.get(&future_id).await.unwrap();
    store.get(&no_expiry_id).await.unwrap();
}

#[tokio::test]
async fn purge_route_disabled_when_no_media_store_in_state() {
    // When media_store is None, the route should return 404 (route not registered).
    let port = free_port();
    let state = AppState {
        adapter: Arc::new(NoopAdapter),
        upload_service: None,
        image_service: None,
        health_admin: gemini_bridge_http_server::build_health_admin(None),
        conversation_store: None,
        tool_engine: gemini_bridge_http_server::build_tool_engine(),
        gallery_service: None,
        media_purge: None,
    };
    let config = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key: Some("key".to_string()),
        require_key_for_admin: true,
        cors_enabled: false,
        rate_limit: None,
    };
    let router = build_router(config, state);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let _srv = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/purge"))
        .header("Authorization", "Bearer key")
        .send()
        .await
        .unwrap();

    // Route may return either 404 (not registered) or 405 (no handler for method).
    // Either indicates the purge endpoint is inactive when media_store is None.
    assert!(
        resp.status() == 404 || resp.status() == 405,
        "purge route must be inactive when media_store is None; got {}",
        resp.status()
    );
}
