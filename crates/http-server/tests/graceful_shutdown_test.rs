use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{Stream, stream};
use gemini_bridge_health_admin::DefaultHealthAdminService;
use gemini_bridge_http_server::{
    AppState, ServerConfig, ServerError, ServerOptions, start_server_with_options_and_shutdown,
    start_server_with_options_and_shutdown_timeout,
};
use gemini_bridge_llm_service::{
    Completion, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
};
use gemini_bridge_tool_calling::DefaultToolEngine;
use tokio::sync::{Mutex, Notify, oneshot};
use tokio::time::timeout;

struct BlockingAdapter {
    entered: Arc<Notify>,
    release: Mutex<Option<oneshot::Receiver<()>>>,
}

#[async_trait]
impl LlmAdapter for BlockingAdapter {
    fn provider_id(&self) -> &'static str {
        "blocking-test"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        self.entered.notify_one();
        let release = self
            .release
            .lock()
            .await
            .take()
            .expect("only one request enters the blocking adapter");
        release.await.expect("test releases in-flight request");
        Ok(Completion {
            text: "drained response".to_owned(),
            finish_reason: "stop".to_owned(),
            usage: None,
            metadata: None,
        })
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        let events: Pin<Box<dyn Stream<Item = Result<LlmEvent, LlmError>> + Send>> =
            Box::pin(stream::empty());
        Ok(events)
    }
}

#[tokio::test]
async fn shutdown_drains_an_in_flight_request_before_the_serve_future_returns() {
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);

    let entered = Arc::new(Notify::new());
    let (release_tx, release_rx) = oneshot::channel();
    let adapter = Arc::new(BlockingAdapter {
        entered: entered.clone(),
        release: Mutex::new(Some(release_rx)),
    });
    let state = AppState {
        adapter,
        upload_service: None,
        image_service: None,
        video_service: None,
        health_admin: Arc::new(DefaultHealthAdminService::new(None)),
        identity_service: None,
        conversation_store: None,
        tool_engine: Arc::new(DefaultToolEngine),
        gallery_service: None,
        media_purge: None,
    };
    let config = ServerConfig {
        bind_addr: address,
        api_key: None,
        require_key_for_admin: true,
        cors_enabled: false,
        rate_limit: None,
        metrics_enabled: false,
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let mut server = tokio::spawn(start_server_with_options_and_shutdown(
        config,
        state,
        ServerOptions::default(),
        async move {
            let _ = shutdown_rx.await;
        },
    ));

    let health = format!("http://{address}/healthz");
    timeout(Duration::from_secs(5), async {
        loop {
            if reqwest::get(&health)
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("server started listening");

    let client = reqwest::Client::new();
    let endpoint = format!("http://{address}/v1/chat/completions");
    let request = tokio::spawn(async move {
        client
            .post(endpoint)
            .json(&serde_json::json!({
                "model": "gemini-web-flash",
                "messages": [{"role": "user", "content": "wait for release"}]
            }))
            .send()
            .await
    });

    timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("request reached the blocking adapter");
    shutdown_tx.send(()).unwrap();

    timeout(Duration::from_secs(5), async {
        loop {
            match tokio::net::TcpStream::connect(address).await {
                Ok(stream) => drop(stream),
                Err(_) => break,
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown stopped accepting new connections");
    assert!(
        !server.is_finished(),
        "serve returned before its in-flight request completed"
    );

    release_tx.send(()).unwrap();
    let response = timeout(Duration::from_secs(5), request)
        .await
        .expect("request completed after release")
        .expect("request task joined")
        .expect("request succeeded");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "drained response");

    let result = timeout(Duration::from_secs(5), &mut server)
        .await
        .expect("serve future returned after the request drained")
        .expect("server task joined");
    assert!(result.is_ok());
}

#[tokio::test]
async fn non_loopback_bind_without_api_key_fails_before_listening() {
    let state = AppState {
        adapter: Arc::new(BlockingAdapter {
            entered: Arc::new(Notify::new()),
            release: Mutex::new(None),
        }),
        upload_service: None,
        image_service: None,
        video_service: None,
        health_admin: Arc::new(DefaultHealthAdminService::new(None)),
        identity_service: None,
        conversation_store: None,
        tool_engine: Arc::new(DefaultToolEngine),
        gallery_service: None,
        media_purge: None,
    };
    let error = start_server_with_options_and_shutdown(
        ServerConfig {
            bind_addr: "0.0.0.0:0".parse().unwrap(),
            api_key: None,
            require_key_for_admin: true,
            cors_enabled: false,
            rate_limit: None,
            metrics_enabled: false,
        },
        state,
        ServerOptions::default(),
        async {},
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("API key is required"));
}

#[tokio::test]
async fn stalled_request_returns_typed_error_after_drain_deadline() {
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let entered = Arc::new(Notify::new());
    let (_release_tx, release_rx) = oneshot::channel();
    let state = AppState {
        adapter: Arc::new(BlockingAdapter {
            entered: entered.clone(),
            release: Mutex::new(Some(release_rx)),
        }),
        upload_service: None,
        image_service: None,
        video_service: None,
        health_admin: Arc::new(DefaultHealthAdminService::new(None)),
        identity_service: None,
        conversation_store: None,
        tool_engine: Arc::new(DefaultToolEngine),
        gallery_service: None,
        media_purge: None,
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(start_server_with_options_and_shutdown_timeout(
        ServerConfig {
            bind_addr: address,
            api_key: None,
            require_key_for_admin: true,
            cors_enabled: false,
            rate_limit: None,
            metrics_enabled: false,
        },
        state,
        ServerOptions::default(),
        async move {
            let _ = shutdown_rx.await;
        },
        Duration::from_millis(25),
    ));

    timeout(Duration::from_secs(5), async {
        while tokio::net::TcpStream::connect(address).await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("http://{address}/v1/chat/completions"))
            .json(&serde_json::json!({
                "model": "gemini-web-flash",
                "messages": [{"role": "user", "content": "stay blocked"}]
            }))
            .send()
            .await
    });
    timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    shutdown_tx.send(()).unwrap();

    let error = timeout(Duration::from_secs(2), server)
        .await
        .expect("server honored bounded drain deadline")
        .expect("server task joined")
        .unwrap_err();
    assert!(matches!(error, ServerError::ShutdownTimeout(_)));
    request.abort();
}
