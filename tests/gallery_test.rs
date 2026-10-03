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

/// Spawn an in-process server with no API key configured.
async fn spawn_server(port: u16, store: LocalMediaStore) -> tokio::task::JoinHandle<()> {
    spawn_server_with_key(port, store, None).await
}

/// Same as `spawn_server` but with a configured API key for auth-matrix tests.
async fn spawn_server_with_key(
    port: u16,
    store: LocalMediaStore,
    api_key: Option<String>,
) -> tokio::task::JoinHandle<()> {
    let gallery_service: Arc<dyn GalleryService> =
        Arc::new(DefaultGalleryService::new(Arc::new(store)));
    let state = AppState {
        adapter: Arc::new(NoopAdapter),
        upload_service: None,
        image_service: None,
        video_service: None,
        health_admin: gemini_bridge_http_server::build_health_admin(None),
        identity_service: None,
        conversation_store: None,
        tool_engine: gemini_bridge_http_server::build_tool_engine(),
        gallery_service: Some(gallery_service),
        media_purge: None,
    };
    let config = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key,
        require_key_for_admin: false,
        cors_enabled: false,
        rate_limit: None,
        metrics_enabled: false,
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

/// format=html returns the embedded UI with its script/style hashes and leaves
/// the default CSP unchanged on JSON/API responses.
#[tokio::test]
async fn gallery_html_csp_allows_only_embedded_inline_assets() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let port = free_port();
    let srv = spawn_server(port, store).await;

    let client = reqwest::Client::new();
    let html = client
        .get(format!("http://127.0.0.1:{port}/gallery?format=html"))
        .send()
        .await
        .unwrap();
    assert_eq!(html.status(), reqwest::StatusCode::OK);
    let csp = html.headers()["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("style-src 'sha256-"));
    assert!(csp.contains("script-src 'sha256-"));
    assert!(!csp.contains("unsafe-inline") && !csp.contains("*"));

    let json = client
        .get(format!("http://127.0.0.1:{port}/gallery"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        json.headers()["content-security-policy"],
        "default-src 'none'"
    );
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

// ── Auth matrix ───────────────────────────────────────────────────────────────

/// Gallery GET routes honor the configured API key: rejected without or with a
/// wrong Bearer token, allowed with the correct one.
#[tokio::test]
async fn gallery_list_and_download_require_api_key_when_configured() {
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
    let srv = spawn_server_with_key(port, store, Some("secret".to_string())).await;
    let base = format!("http://127.0.0.1:{port}");

    let no_key_list = reqwest::get(format!("{base}/gallery")).await.unwrap();
    assert_eq!(no_key_list.status(), reqwest::StatusCode::UNAUTHORIZED);
    let no_key_download = reqwest::get(format!("{base}/gallery/{id}/download"))
        .await
        .unwrap();
    assert_eq!(no_key_download.status(), reqwest::StatusCode::UNAUTHORIZED);

    let client = reqwest::Client::new();
    let wrong_key = client
        .get(format!("{base}/gallery"))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_key.status(), reqwest::StatusCode::UNAUTHORIZED);

    let ok_list = client
        .get(format!("{base}/gallery"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(ok_list.status(), reqwest::StatusCode::OK);
    let ok_download = client
        .get(format!("{base}/gallery/{id}/download"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(ok_download.status(), reqwest::StatusCode::OK);

    srv.abort();
}

/// Deletion of a specific gallery item honors the API key when configured.
#[tokio::test]
async fn gallery_delete_requires_api_key_when_configured() {
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
    let srv = spawn_server_with_key(port, store, Some("secret".to_string())).await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let no_key = client
        .delete(format!("{base}/gallery/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(no_key.status(), reqwest::StatusCode::UNAUTHORIZED);

    let wrong_key = client
        .delete(format!("{base}/gallery/{id}"))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_key.status(), reqwest::StatusCode::UNAUTHORIZED);

    let ok = client
        .delete(format!("{base}/gallery/{id}"))
        .bearer_auth("secret")
        .header("origin", "https://evil.example")
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), reqwest::StatusCode::OK);

    srv.abort();
}

/// Unauthenticated loopback routes remain usable by native clients, but a
/// browser-originated cross-site delete is rejected.
#[tokio::test]
async fn gallery_delete_rejects_hostile_browser_origin_but_allows_native_client() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let first_id = store
        .put(
            Bytes::from_static(PNG),
            make_meta("image/png", None, None, 1000),
        )
        .await
        .unwrap();
    let second_id = store
        .put(
            Bytes::from_static(PNG),
            make_meta("image/png", None, None, 1001),
        )
        .await
        .unwrap();
    let port = free_port();
    let srv = spawn_server(port, store).await;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let denied = client
        .delete(format!("{base}/gallery/{first_id}"))
        .header("origin", "https://evil.example")
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
    let body: serde_json::Value = denied.json().await.unwrap();
    assert_eq!(body["error"]["code"], "cross_origin_request");

    let allowed = client
        .delete(format!("{base}/gallery/{second_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), reqwest::StatusCode::OK);
    assert_eq!(
        allowed.json::<serde_json::Value>().await.unwrap()["deleted"],
        second_id
    );
    srv.abort();
}

// ── Accessible UI and hostile content rendering ───────────────────────────────

/// The served gallery page carries the accessible markup and injection-safe
/// rendering guarantees.
#[tokio::test]
async fn gallery_html_has_accessible_landmarks_and_labels() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let port = free_port();
    let srv = spawn_server(port, store).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/gallery?format=html"))
        .await
        .unwrap();
    let html = resp.text().await.unwrap();

    assert_eq!(html.matches("<main").count(), 1);
    assert!(html.contains("role=\"status\" aria-live=\"polite\""));
    for id in ["f-prompt", "f-model", "f-from", "f-to"] {
        assert!(
            html.contains(&format!("for=\"{id}\"")),
            "missing label for {id}"
        );
        assert!(html.contains(&format!("id=\"{id}\"")), "missing input {id}");
    }
    assert!(html.contains(":focus-visible"));
    assert!(html.contains("Confirm delete"));
    // Rendering path is DOM-based only; no markup-string sinks ship to the page.
    assert!(!html.contains("innerHTML"));
    assert!(!html.contains("insertAdjacentHTML"));

    srv.abort();
}

/// Hostile prompt/model/id strings round-trip as plain JSON text and are never
/// reflected into the served document as markup.
#[tokio::test]
async fn gallery_hostile_prompt_model_remain_plain_text() {
    let temp = TempDir::new().unwrap();
    let store = LocalMediaStore::new(temp.path());
    let hostile_prompt = "<img src=x onerror=alert(1)><script>alert(2)</script>";
    let hostile_model = "\" onmouseover=alert(3) data-x=\"";
    store
        .put(
            Bytes::from_static(PNG),
            make_meta("image/png", Some(hostile_prompt), Some(hostile_model), 1000),
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
    assert_eq!(data[0]["prompt"].as_str().unwrap(), hostile_prompt);
    assert_eq!(data[0]["model"].as_str().unwrap(), hostile_model);

    // The page itself is static: hostile content never becomes part of it, and
    // rendering happens via DOM text nodes, not HTML string sinks.
    let html = reqwest::get(format!("http://127.0.0.1:{port}/gallery?format=html"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!html.contains("<script>alert(2)</script>"));
    assert!(!html.contains("onerror=alert"));
    assert!(!html.contains("innerHTML"));

    srv.abort();
}
