//! Integration tests for Task 2.4: gallery JSON and embedded HTML path.
//!
//! Tests run against an in-process Axum server backed by a real LocalMediaStore
//! on a temp directory. The gallery crate is exercised through HTTP routes.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream;
use gemini_bridge_gallery::{DefaultGalleryService, GalleryService};
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_llm_service::{
    Completion, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
};
use gemini_bridge_media_store::{LocalMediaStore, MediaMetadata, MediaStore};
use tempfile::TempDir;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\ntest image content";

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

// ── Server helpers ─────────────────────────────────────────────────────────────

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn spawn_server(port: u16, store: LocalMediaStore) -> tokio::task::JoinHandle<()> {
    let gallery_service: Arc<dyn GalleryService> =
        Arc::new(DefaultGalleryService::new(Arc::new(store)));
    let state = AppState {
        adapter: Arc::new(NoopAdapter),
        upload_service: None,
        image_service: None,
        health_admin: gemini_bridge_http_server::build_health_admin(None),
        conversation_store: None,
        tool_engine: gemini_bridge_http_server::build_tool_engine(),
        gallery_service: Some(gallery_service),
    };
    let config = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key: None,
        require_key_for_admin: false,
        cors_enabled: false,
        rate_limit: None,
    };
    let router = build_router(config, state);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    })
}

fn make_meta(
    mime: &str,
    prompt: Option<&str>,
    model: Option<&str>,
    created_at: i64,
) -> MediaMetadata {
    MediaMetadata {
        id: String::new(),
        sha256: String::new(),
        mime_type: mime.to_string(),
        size_bytes: 0,
        created_at,
        expires_at: None,
        prompt: prompt.map(|s| s.to_string()),
        file_ref: String::new(),
        model: model.map(|s| s.to_string()),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

/// Empty store returns 200 with empty data and total 0.
#[tokio::test]
async fn gallery_empty_store_returns_empty_response() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let port = free_port();
    let srv = spawn_server(port, store).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
    assert_eq!(body["total"].as_u64().unwrap(), 0);

    srv.abort();
}

/// Populated store returns items newest-first.
#[tokio::test]
async fn gallery_returns_items_newest_first() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let id1 = store
        .put(
            Bytes::from_static(PNG),
            make_meta("image/png", Some("older"), None, 1000),
        )
        .await
        .unwrap();
    let id2 = store
        .put(
            Bytes::from_static(b"PNG newer"),
            make_meta("image/png", Some("newer"), None, 2000),
        )
        .await
        .unwrap();

    let port = free_port();
    let srv = spawn_server(port, store).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
    assert_eq!(body["total"].as_u64().unwrap(), 2);
    // Newest first: id2 (created_at=2000) should come first.
    assert_eq!(data[0]["id"].as_str().unwrap(), id2);
    assert_eq!(data[1]["id"].as_str().unwrap(), id1);

    srv.abort();
}

/// Filter by prompt (case-insensitive substring).
#[tokio::test]
async fn gallery_filter_by_prompt() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    store
        .put(
            Bytes::from_static(PNG),
            make_meta("image/png", Some("Red Fox"), None, 1000),
        )
        .await
        .unwrap();
    store
        .put(
            Bytes::from_static(b"PNG2"),
            make_meta("image/png", Some("Blue Sky"), None, 2000),
        )
        .await
        .unwrap();

    let port = free_port();
    let srv = spawn_server(port, store).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery?prompt=red"))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(body["total"].as_u64().unwrap(), 1);
    assert_eq!(data[0]["prompt"].as_str().unwrap(), "Red Fox");

    srv.abort();
}

/// Filter by model (exact match).
#[tokio::test]
async fn gallery_filter_by_model() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    store
        .put(
            Bytes::from_static(PNG),
            make_meta("image/png", None, Some("gemini-flash"), 1000),
        )
        .await
        .unwrap();
    store
        .put(
            Bytes::from_static(b"PNG2"),
            make_meta("image/png", None, Some("gemini-pro"), 2000),
        )
        .await
        .unwrap();

    let port = free_port();
    let srv = spawn_server(port, store).await;

    let resp = reqwest::get(format!(
        "http://127.0.0.1:{port}/gallery?model=gemini-flash"
    ))
    .await
    .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(body["total"].as_u64().unwrap(), 1);
    assert_eq!(data[0]["model"].as_str().unwrap(), "gemini-flash");

    srv.abort();
}

/// Filter by date_from and date_to (inclusive boundaries).
#[tokio::test]
async fn gallery_filter_by_date_range() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    store
        .put(
            Bytes::from_static(PNG),
            make_meta("image/png", Some("early"), None, 1000),
        )
        .await
        .unwrap();
    store
        .put(
            Bytes::from_static(b"PNG2"),
            make_meta("image/png", Some("mid"), None, 2000),
        )
        .await
        .unwrap();
    store
        .put(
            Bytes::from_static(b"PNG3"),
            make_meta("image/png", Some("late"), None, 3000),
        )
        .await
        .unwrap();

    let port = free_port();
    let srv = spawn_server(port, store).await;

    // Include items in [1000, 2000].
    let resp = reqwest::get(format!(
        "http://127.0.0.1:{port}/gallery?date_from=1000&date_to=2000"
    ))
    .await
    .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["total"].as_u64().unwrap(), 2);

    // date_from > date_to → 400.
    let resp = reqwest::get(format!(
        "http://127.0.0.1:{port}/gallery?date_from=3000&date_to=1000"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    srv.abort();
}

/// Pagination: offset + limit.
#[tokio::test]
async fn gallery_pagination() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    for i in 0..5u64 {
        store
            .put(
                Bytes::from(format!("PNG{i}")),
                make_meta("image/png", None, None, i as i64 * 100),
            )
            .await
            .unwrap();
    }

    let port = free_port();
    let srv = spawn_server(port, store).await;

    // First page of 2 → total should still be 5.
    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery?limit=2&offset=0"))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 2);
    assert_eq!(body["total"].as_u64().unwrap(), 5);
    assert_eq!(body["limit"].as_u64().unwrap(), 2);
    assert_eq!(body["offset"].as_u64().unwrap(), 0);

    // Limit > 200 → 400.
    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery?limit=201"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Limit 0 → 400.
    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery?limit=0"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    srv.abort();
}

/// Download returns exact bytes and MIME type.
#[tokio::test]
async fn gallery_download_returns_bytes_and_mime() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let id = store
        .put(
            Bytes::from_static(PNG),
            make_meta("image/png", None, None, 1000),
        )
        .await
        .unwrap();

    let port = free_port();
    let srv = spawn_server(port, store).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery/{id}/download"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/png");
    assert_eq!(resp.bytes().await.unwrap(), Bytes::from_static(PNG));

    srv.abort();
}

/// Download of unknown ID returns 404.
#[tokio::test]
async fn gallery_download_unknown_id_returns_404() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let port = free_port();
    let srv = spawn_server(port, store).await;

    let resp = reqwest::get(format!(
        "http://127.0.0.1:{port}/gallery/nonexistent-id/download"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    srv.abort();
}

/// Delete removes an item; subsequent download returns 404.
#[tokio::test]
async fn gallery_delete_then_download_returns_404() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let id = store
        .put(
            Bytes::from_static(PNG),
            make_meta("image/png", None, None, 1000),
        )
        .await
        .unwrap();

    let port = free_port();
    let srv = spawn_server(port, store).await;

    let client = reqwest::Client::new();
    let del = client
        .delete(format!("http://127.0.0.1:{port}/gallery/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), reqwest::StatusCode::OK);

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery/{id}/download"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    srv.abort();
}

/// Delete of unknown ID returns 404.
#[tokio::test]
async fn gallery_delete_unknown_id_returns_404() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let port = free_port();
    let srv = spawn_server(port, store).await;

    let client = reqwest::Client::new();
    let resp = client
        .delete(format!("http://127.0.0.1:{port}/gallery/no-such-id"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    srv.abort();
}

/// format=html returns text/html with gallery UI controls.
#[tokio::test]
async fn gallery_html_format_returns_html() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let port = free_port();
    let srv = spawn_server(port, store).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery?format=html"))
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let content_type = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(content_type.starts_with("text/html"));
    let html = resp.text().await.unwrap();
    // Must contain recognisable gallery UI tokens.
    assert!(html.contains("gallery") || html.contains("Gallery"));
    // Must not reference external hosts.
    assert!(!html.contains("https://cdn.") && !html.contains("https://fonts.google"));

    let invalid = reqwest::get(format!("http://127.0.0.1:{port}/gallery?format=xml"))
        .await
        .unwrap();
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    srv.abort();
}

/// URL field in listing must be a same-origin path.
#[tokio::test]
async fn gallery_url_field_is_same_origin() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let id = store
        .put(
            Bytes::from_static(PNG),
            make_meta("image/png", None, None, 1000),
        )
        .await
        .unwrap();

    let port = free_port();
    let srv = spawn_server(port, store).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery"))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let data = body["data"].as_array().unwrap();
    let url = data[0]["url"].as_str().unwrap();
    // Must start with /gallery/ — no scheme, no external host.
    assert!(url.starts_with("/gallery/"), "url={url}");
    assert!(url.contains(&id));

    srv.abort();
}
