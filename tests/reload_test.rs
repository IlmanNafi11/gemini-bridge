//! Tests for Task 3.3: Plugin reload without dropping active streams.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::HeaderMap;
use futures::{StreamExt, stream};
use gemini_bridge_health_admin::{
    DefaultHealthAdminService, HealthAdminError, PluginReloader, ReloadCommit,
};
use gemini_bridge_http_server::{AppState, ServerConfig, build_router};
use gemini_bridge_identity::{
    IdentityError, IdentityService, SessionBootstrap, SessionSnapshot, SessionStatus,
};
use gemini_bridge_llm_service::{
    Completion, CompletionSummary, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
    ModelSelector, ReloadableAdapter,
};
use gemini_bridge_plugin_context::ReloadableSlot;
use gemini_bridge_tool_calling::DefaultToolEngine;
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

// ── Mock Generational Adapter ───────────────────────────────────────────────

struct GenerationalAdapter {
    generation: usize,
    /// Channel to coordinate stream chunk delivery for deterministic pause/resume.
    chunk_rx_factory: Option<Arc<dyn Fn() -> mpsc::Receiver<String> + Send + Sync>>,
}

impl GenerationalAdapter {
    fn new(generation: usize) -> Self {
        Self {
            generation,
            chunk_rx_factory: None,
        }
    }

    fn with_stream_control(
        generation: usize,
        factory: Arc<dyn Fn() -> mpsc::Receiver<String> + Send + Sync>,
    ) -> Self {
        Self {
            generation,
            chunk_rx_factory: Some(factory),
        }
    }
}

#[async_trait]
impl LlmAdapter for GenerationalAdapter {
    fn provider_id(&self) -> &'static str {
        "generational-mock"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        Ok(Completion {
            text: format!("Response from generation {}", self.generation),
            finish_reason: "stop".to_string(),
            usage: None,
            metadata: None,
        })
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        let generation = self.generation;
        if let Some(factory) = &self.chunk_rx_factory {
            let rx = factory();
            let text_events = stream::unfold(rx, move |mut rx| async move {
                rx.recv().await.map(|chunk| {
                    (
                        Ok(LlmEvent::TextDelta(format!("[gen{generation}:{chunk}]"))),
                        rx,
                    )
                })
            });
            let completed = stream::once(async {
                Ok(LlmEvent::Completed(CompletionSummary {
                    finish_reason: "stop".to_string(),
                    usage: None,
                    metadata: None,
                }))
            });
            Ok(Box::pin(text_events.chain(completed)) as LlmEventStream)
        } else {
            let events = vec![
                Ok(LlmEvent::TextDelta(format!("gen{generation}-chunk1"))),
                Ok(LlmEvent::Completed(CompletionSummary {
                    finish_reason: "stop".to_string(),
                    usage: None,
                    metadata: None,
                })),
            ];
            Ok(Box::pin(stream::iter(events)) as LlmEventStream)
        }
    }
}

// ── Mock Identity ───────────────────────────────────────────────────────────

struct MockIdentity;

#[async_trait]
impl IdentityService for MockIdentity {
    async fn bootstrap(&self) -> Result<SessionBootstrap, IdentityError> {
        Ok(SessionBootstrap {
            bl: "test-bl".to_string(),
            snlm0e: "snlm0e".to_string(),
            fsid: "fsid".to_string(),
        })
    }

    async fn refresh_1psidts(&self) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            status: SessionStatus::Valid,
            build_label: Some("test-bl".to_string()),
            cookie_age: Some(Duration::from_secs(100)),
            checked_at: std::time::SystemTime::now(),
        }
    }

    fn apply_auth_headers(&self, _headers: &mut HeaderMap) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn import_credentials(&self, _raw: &str) -> Result<(), IdentityError> {
        Ok(())
    }
}

// ── Test Server Harness ─────────────────────────────────────────────────────

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

// ── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_reloadable_slot_basic_swap() {
    let slot = ReloadableSlot::new(Arc::new(10u32));
    assert_eq!(*slot.get().await, 10);

    slot.publish(Arc::new(20u32));
    assert_eq!(*slot.get().await, 20);
}

struct DelayedReloader {
    slot: ReloadableSlot<dyn LlmAdapter>,
}

#[async_trait]
impl PluginReloader for DelayedReloader {
    async fn prepare(&self, _plugin_name: &str) -> Result<ReloadCommit, HealthAdminError> {
        tokio::time::sleep(Duration::from_millis(2100)).await;
        let slot = self.slot.clone();
        Ok(Box::new(move || {
            let _ = slot.publish(Arc::new(GenerationalAdapter::new(2)) as Arc<dyn LlmAdapter>);
        }))
    }
}

#[tokio::test]
async fn reload_prepare_timeout_keeps_old_generation_active() {
    let adapter =
        ReloadableAdapter::new(Arc::new(GenerationalAdapter::new(1)) as Arc<dyn LlmAdapter>);
    let reloader = DelayedReloader {
        slot: adapter.slot(),
    };

    let error = gemini_bridge_health_admin::reload_handler::execute_reload_with_deadline(
        &reloader,
        "gemini-adapter",
    )
    .await
    .unwrap_err();

    assert!(matches!(error, HealthAdminError::ReloadFailed(_)));
    let request = Arc::new(LlmRequest {
        model: ModelSelector {
            provider: "gemini-web".to_owned(),
            model: "gemini-web-flash".to_owned(),
            thinking_level: None,
        },
        messages: Arc::from([]),
        temperature: None,
        max_output_tokens: None,
        tools: Arc::from([]),
        metadata: BTreeMap::new(),
    });
    let completion = adapter.complete(request).await.unwrap();
    assert_eq!(completion.text, "Response from generation 1");
}

#[tokio::test]
async fn test_admin_reload_auth_matrix() {
    let port = free_port();
    let config =
        Arc::new(gemini_bridge_config::BridgeConfig::load_with_overrides(None, None).unwrap());
    let identity = Arc::new(gemini_bridge_identity::DefaultIdentityService::new(&config).unwrap());
    let (adapter, reloader) =
        gemini_bridge_http_server::build_reloadable_gemini_adapter(config, identity.clone())
            .unwrap();
    let health_admin =
        Arc::new(DefaultHealthAdminService::new(Some(identity)).with_reloader(reloader));

    let state = AppState {
        adapter,
        upload_service: None,
        image_service: None,
        video_service: None,
        health_admin,
        conversation_store: None,
        tool_engine: Arc::new(DefaultToolEngine),
        gallery_service: None,
        media_purge: None,
    };

    let srv_cfg = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key: Some("secret-admin-key".to_string()),
        require_key_for_admin: true,
        cors_enabled: false,
        rate_limit: None,
    };

    let router = build_router(srv_cfg, state);
    let handle = spawn_server(router, port).await;

    let client = Client::new();
    let base = format!("http://127.0.0.1:{port}");

    // 1. Missing auth header -> 401
    let resp = client
        .post(format!("{base}/admin/reload-plugin"))
        .json(&json!({"plugin": "gemini-adapter"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // 2. Wrong auth header -> 401
    let resp = client
        .post(format!("{base}/admin/reload-plugin"))
        .header("authorization", "Bearer wrong-key")
        .json(&json!({"plugin": "gemini-adapter"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // 3. Valid key, unknown plugin -> 500 / error response
    let resp = client
        .post(format!("{base}/admin/reload-plugin"))
        .header("authorization", "Bearer secret-admin-key")
        .json(&json!({"plugin": "non-existent-plugin"}))
        .send()
        .await
        .unwrap();
    assert!(!resp.status().is_success());

    // 4. Valid key, correct plugin -> 200 OK
    let resp = client
        .post(format!("{base}/admin/reload-plugin"))
        .header("authorization", "Bearer secret-admin-key")
        .json(&json!({"plugin": "gemini-adapter"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");

    handle.abort();
}

#[tokio::test]
async fn test_active_sse_stream_preservation_during_reload() {
    let port = free_port();

    // Create a channel factory so we can feed chunks to Generation 1 stream
    let (tx1, rx1) = mpsc::channel::<String>(10);
    let rx1 = Arc::new(tokio::sync::Mutex::new(Some(rx1)));

    let rx_factory = Arc::new(move || {
        rx1.try_lock()
            .unwrap()
            .take()
            .expect("should only be called once")
    });

    let swappable = Arc::new(ReloadableAdapter::new(Arc::new(
        GenerationalAdapter::with_stream_control(1, rx_factory),
    ) as Arc<dyn LlmAdapter>));
    let slot = swappable.slot();

    struct AdapterReloader {
        slot: ReloadableSlot<dyn LlmAdapter>,
    }

    #[async_trait]
    impl PluginReloader for AdapterReloader {
        async fn prepare(&self, plugin_name: &str) -> Result<ReloadCommit, HealthAdminError> {
            if plugin_name == "gemini-adapter" {
                let slot = self.slot.clone();
                Ok(Box::new(move || {
                    let _ =
                        slot.publish(Arc::new(GenerationalAdapter::new(2)) as Arc<dyn LlmAdapter>);
                }))
            } else {
                Err(HealthAdminError::ReloadFailed("unknown".into()))
            }
        }
    }

    let health_admin = Arc::new(
        DefaultHealthAdminService::new(Some(Arc::new(MockIdentity)))
            .with_reloader(Arc::new(AdapterReloader { slot: slot.clone() })),
    );

    let state = AppState {
        adapter: swappable,
        upload_service: None,
        image_service: None,
        video_service: None,
        health_admin,
        conversation_store: None,
        tool_engine: Arc::new(DefaultToolEngine),
        gallery_service: None,
        media_purge: None,
    };

    let srv_cfg = ServerConfig {
        bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
        api_key: Some("secret-key".to_string()),
        require_key_for_admin: true,
        cors_enabled: false,
        rate_limit: None,
    };

    let router = build_router(srv_cfg, state);
    let handle = spawn_server(router, port).await;

    let client = Client::new();
    let base = format!("http://127.0.0.1:{port}");

    // 1. Initiate an active SSE stream on Generation 1
    let chat_req = json!({
        "model": "gemini-web-flash",
        "messages": [{"role": "user", "content": "Hello"}],
        "stream": true
    });

    let sse_resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", "Bearer secret-key")
        .json(&chat_req)
        .send()
        .await
        .unwrap();

    assert_eq!(sse_resp.status(), 200);
    let mut byte_stream = sse_resp.bytes_stream();

    // Send first chunk to Gen 1
    tx1.send("first-word".to_string()).await.unwrap();

    // Read the chunk from SSE stream
    let first_chunk = byte_stream.next().await.unwrap().unwrap();
    let first_text = String::from_utf8_lossy(&first_chunk);
    assert!(first_text.contains("[gen1:first-word]"));

    // 2. While the SSE stream is still active, trigger /admin/reload-plugin
    let reload_start = std::time::Instant::now();
    let reload_resp = client
        .post(format!("{base}/admin/reload-plugin"))
        .header("authorization", "Bearer secret-key")
        .json(&json!({"plugin": "gemini-adapter"}))
        .send()
        .await
        .unwrap();
    let reload_duration = reload_start.elapsed();

    assert_eq!(reload_resp.status(), 200);
    // Reload must complete in < 2 seconds
    assert!(reload_duration < Duration::from_secs(2));

    // 3. New non-streaming chat request must immediately hit Generation 2
    let chat_req_gen2 = json!({
        "model": "gemini-web-flash",
        "messages": [{"role": "user", "content": "Hello Gen2"}],
        "stream": false
    });
    let non_stream_resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", "Bearer secret-key")
        .json(&chat_req_gen2)
        .send()
        .await
        .unwrap();
    assert_eq!(non_stream_resp.status(), 200);
    let body: Value = non_stream_resp.json().await.unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "Response from generation 2"
    );

    // 4. In-flight Gen 1 SSE stream must still be alive and able to receive further chunks and [DONE]
    tx1.send("second-word".to_string()).await.unwrap();
    let second_chunk = byte_stream.next().await.unwrap().unwrap();
    let second_text = String::from_utf8_lossy(&second_chunk);
    assert!(second_text.contains("[gen1:second-word]"));

    // Close Gen 1 sender so the stream completes
    drop(tx1);

    let mut remaining = String::new();
    while let Some(Ok(bytes)) = byte_stream.next().await {
        remaining.push_str(&String::from_utf8_lossy(&bytes));
    }
    assert!(remaining.contains("[DONE]"));

    handle.abort();
}
