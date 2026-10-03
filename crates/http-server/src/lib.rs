//! Axum HTTP server for gemini-bridge.
//!
//! Exposes all API routes, enforces authentication, propagates request IDs,
//! and wires handlers to the LLM adapter.

pub mod handlers;
pub mod metrics;

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::header::{self, ACCEPT, AUTHORIZATION, CONTENT_TYPE, HOST, ORIGIN};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing};
use futures::{StreamExt, stream};
use gemini_bridge_conversation_store::ConversationStore;
use gemini_bridge_health_admin::{
    DefaultHealthAdminService, HealthAdminError, HealthAdminService, MediaPurgeAdminService,
    PluginReloader, ReloadCommit,
};
use gemini_bridge_identity::DefaultIdentityService;
use gemini_bridge_llm_service::{LlmAdapter, ReloadableAdapter};
pub use gemini_bridge_middleware::TokenBucketConfig;
use gemini_bridge_middleware::{
    AdmissionDecision, HttpMetrics, RedactionFilter, TokenBucketLimiter,
};
use gemini_bridge_openai_compat::OpenAiErrorResponse;
use gemini_bridge_tool_calling::{DefaultToolEngine, ToolEngine};
use serde_json::json;
use thiserror::Error;
use tokio::sync::Semaphore;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

const REQUEST_ID_HEADER: &str = "x-request-id";
const DEFAULT_CONCURRENCY_LIMIT: usize = 4;
const DEFAULT_BODY_LIMIT_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_CSP: &str = "default-src 'none'";
// SHA-256 hashes over the exact inline contents of gallery.html's style and
// script elements. A stale hash fails closed when an embedded asset changes.
const GALLERY_CSP: &str = "default-src 'none'; base-uri 'none'; form-action 'self'; img-src 'self'; connect-src 'self'; style-src 'sha256-Ys6KdJcY1fwN/XQNXR555HpTRtB9xUU2IZOpQdkudHM='; script-src 'sha256-AWWVjmuMddm5L+S3lfwM5V674uH9CwpuABgymMKLwvY='";
const SECURITY_HEADERS: [(&str, &str); 3] = [
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
];

/// Maximum time the HTTP server waits for in-flight requests after shutdown.
pub const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Compare same-length API keys without data-dependent early exit. Token
/// length is public; byte pairs are XOR-folded into one accumulator.
fn constant_time_eq(candidate: &[u8], expected: &[u8]) -> bool {
    if candidate.len() != expected.len() {
        return false;
    }
    candidate
        .iter()
        .zip(expected)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

async fn concurrency_middleware(
    State(semaphore): State<Arc<Semaphore>>,
    request: Request,
    next: Next,
) -> Response {
    match semaphore.try_acquire_owned() {
        Ok(permit) => {
            let response = next.run(request).await;
            let (parts, body) = response.into_parts();
            let guarded = stream::unfold(
                (body.into_data_stream(), permit),
                |(mut body, permit)| async move {
                    body.next().await.map(|chunk| (chunk, (body, permit)))
                },
            );
            Response::from_parts(parts, Body::from_stream(guarded))
        }
        Err(tokio::sync::TryAcquireError::NoPermits) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(OpenAiErrorResponse::with_code(
                "Server is overloaded",
                "service_unavailable",
                "server_overloaded",
            )),
        )
            .into_response(),
        Err(tokio::sync::TryAcquireError::Closed) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(OpenAiErrorResponse::new(
                "Server is shutting down",
                "service_unavailable",
            )),
        )
            .into_response(),
    }
}

#[derive(Clone)]
struct PublicMiddlewareState {
    api_key: Option<String>,
    rate_limiter: Option<Arc<TokenBucketLimiter>>,
    unauthenticated_loopback: bool,
}
#[derive(Clone)]
struct DeleteAuthCheckState {
    is_localhost: bool,
    has_api_key: bool,
}
// ── Public types ──────────────────────────────────────────────────────────────

/// Server configuration at the HTTP layer.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_addr: SocketAddr,
    pub api_key: Option<String>,
    pub require_key_for_admin: bool,
    pub cors_enabled: bool,
    pub rate_limit: Option<TokenBucketConfig>,
    /// Enable the authenticated Prometheus metrics endpoint and HTTP instrumentation.
    pub metrics_enabled: bool,
}

/// Runtime HTTP hardening options. Kept separate from [`ServerConfig`] so
/// existing embedders remain source-compatible while config-owned callers can
/// opt into explicit origins and resource limits.
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    pub cors_origins: Vec<String>,
    pub concurrency_limit: usize,
    pub body_limit_bytes: u64,
}

impl ServerOptions {
    fn cors_layer(&self, enabled: bool) -> Option<CorsLayer> {
        if !enabled || self.cors_origins.is_empty() {
            return None;
        }
        let origins = self
            .cors_origins
            .iter()
            .filter_map(|origin| origin.parse::<HeaderValue>().ok())
            .collect::<Vec<_>>();
        if origins.is_empty() {
            return None;
        }
        Some(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::PATCH,
                    Method::DELETE,
                ])
                .allow_headers([
                    ACCEPT,
                    AUTHORIZATION,
                    CONTENT_TYPE,
                    HeaderName::from_static("x-request-id"),
                ])
                .max_age(Duration::from_secs(600)),
        )
    }
}

/// Shared state threaded through all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub adapter: Arc<dyn LlmAdapter>,
    /// File upload/retrieval service; `None` disables `/v1/files` routes.
    pub upload_service: Option<Arc<dyn gemini_bridge_upload::UploadService>>,
    /// Image generation/retrieval service; `None` disables `/v1/images` routes.
    pub image_service: Option<Arc<dyn gemini_bridge_image_gen::ImageGenService>>,
    /// Video generation/retrieval service; `None` returns explicit HTTP 501.
    pub video_service: Option<Arc<dyn gemini_bridge_adapter_video::VideoService>>,
    /// Process and session operations used by health/admin routes.
    pub health_admin: Arc<dyn HealthAdminService>,
    /// Identity service carried for future observability and audit contexts.
    /// Rotation itself is adapter-owned; `None` is acceptable.
    pub identity_service: Option<Arc<dyn gemini_bridge_identity::IdentityService>>,
    /// Conversation persistence; `None` disables `/v1/conversations` routes.
    pub conversation_store: Option<Arc<dyn ConversationStore>>,
    /// Tool-calling translation, validation, and continuation engine.
    pub tool_engine: Arc<dyn ToolEngine>,
    /// Gallery listing, download, and deletion service; `None` disables `/gallery` routes.
    pub gallery_service: Option<Arc<dyn gemini_bridge_gallery::GalleryService>>,
    /// Expired-media purge administration; `None` disables `POST /admin/purge`.
    pub media_purge: Option<Arc<dyn MediaPurgeAdminService>>,
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("bind failed: {0}")]
    Bind(String),
    #[error("serve error: {0}")]
    Serve(String),
    #[error("graceful shutdown timed out after {0:?}")]
    ShutdownTimeout(Duration),
    #[error("adapter initialization failed: {0}")]
    AdapterInitialization(String),
}

// ── Router builder ─────────────────────────────────────────────────────────────

/// Build the Axum router using default resource limits and no configured CORS origins.
/// Prefer [`build_router_with_options`] for production startup.
pub fn build_router(config: ServerConfig, state: AppState) -> Router {
    build_router_with_options(config, state, ServerOptions::default())
}

/// Build the Axum router with explicit-origin CORS and bounded resources.
/// This does not start listening; the caller owns the listener and lifecycle.
pub fn build_router_with_options(
    config: ServerConfig,
    state: AppState,
    options: ServerOptions,
) -> Router {
    let x_request_id = header::HeaderName::from_static("x-request-id");
    let api_key = config.api_key.clone();
    let metrics = config
        .metrics_enabled
        .then(|| Arc::new(HttpMetrics::default()));
    let public_routes = Router::new()
        .route(
            "/v1/chat/completions",
            routing::post(handlers::chat::chat_completions),
        )
        .route(
            "/v1/models",
            routing::get(handlers::models::list_models_handler),
        )
        .route("/v1/files", routing::post(handlers::files::create_file))
        .route("/v1/files/{id}", routing::get(handlers::files::get_file))
        .route(
            "/v1/images/generations",
            routing::post(handlers::images::generate_image),
        )
        .route("/v1/images/{id}", routing::get(handlers::images::get_image))
        .route(
            "/v1/videos/generations",
            routing::post(handlers::videos::generate_video),
        )
        .route("/v1/videos/{id}", routing::get(handlers::videos::get_video))
        .route(
            "/v1/conversations",
            routing::get(handlers::conversations::list_conversations),
        )
        .route(
            "/v1/conversations/{id}/messages",
            routing::get(handlers::conversations::get_messages),
        )
        .route(
            "/v1/conversations/{id}/branch",
            routing::post(handlers::conversations::branch_conversation),
        )
        .route(
            "/v1/conversations/{id}/regenerate",
            routing::post(handlers::conversations::regenerate_conversation),
        )
        .route("/gallery", routing::get(handlers::gallery::list_gallery))
        .route(
            "/gallery/{id}/download",
            routing::get(handlers::gallery::download_gallery_item),
        )
        .route(
            "/gallery/{id}",
            routing::delete(handlers::gallery::delete_gallery_item).layer(
                middleware::from_fn_with_state(
                    DeleteAuthCheckState {
                        is_localhost: config.bind_addr.ip().is_loopback(),
                        has_api_key: config.api_key.is_some(),
                    },
                    gallery_delete_auth_middleware,
                ),
            ),
        )
        .route_layer(middleware::from_fn_with_state(
            PublicMiddlewareState {
                api_key: api_key.clone(),
                rate_limiter: config.rate_limit.map(TokenBucketLimiter::new).map(Arc::new),
                unauthenticated_loopback: config.bind_addr.ip().is_loopback()
                    && config.api_key.is_none(),
            },
            public_middleware,
        ));

    let health_routes = Router::new()
        .route("/healthz", routing::get(handlers::health::healthz))
        .route("/readyz", routing::get(handlers::health::readyz));

    let mut admin_routes = Router::new()
        .route("/admin/status", routing::get(handlers::admin::admin_status))
        .route("/admin/dashboard", routing::get(handlers::admin::dashboard))
        .route("/admin/reauth", routing::post(handlers::admin::reauth))
        .route(
            "/admin/reload-plugin",
            routing::post(handlers::admin::reload_plugin),
        );
    if let Some(metrics) = metrics.clone() {
        admin_routes = admin_routes.route(
            "/metrics",
            routing::get(metrics::scrape).layer(axum::Extension(metrics)),
        );
    }
    if state.media_purge.is_some() {
        admin_routes =
            admin_routes.route("/admin/purge", routing::post(handlers::admin::purge_media));
    }
    let admin_routes = admin_routes.route_layer(middleware::from_fn_with_state(
        api_key,
        admin_auth_middleware,
    ));

    let mut router = Router::new()
        .merge(public_routes)
        .merge(health_routes)
        .merge(admin_routes)
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            metrics,
            metrics::collect_requests,
        ))
        .layer(middleware::from_fn(audit_middleware))
        .layer(PropagateRequestIdLayer::new(x_request_id.clone()))
        .layer(SetRequestIdLayer::new(x_request_id, MakeRequestUuid));

    let cors = options.cors_layer(config.cors_enabled);
    let concurrency_limit = if options.concurrency_limit == 0 {
        DEFAULT_CONCURRENCY_LIMIT
    } else {
        options.concurrency_limit
    };
    let body_limit = if options.body_limit_bytes == 0 {
        DEFAULT_BODY_LIMIT_BYTES
    } else {
        options.body_limit_bytes
    };

    router = router.layer(middleware::from_fn_with_state(
        Arc::new(Semaphore::new(concurrency_limit)),
        concurrency_middleware,
    ));
    router = router.layer(DefaultBodyLimit::max(body_limit as usize));
    if let Some(cors) = cors {
        router = router.layer(cors);
    }
    router
        .layer(middleware::from_fn(endpoint_error_middleware))
        .layer(middleware::from_fn(security_headers_middleware))
}

/// Bind the configured address and serve requests until the process is stopped.
pub async fn start_server(config: ServerConfig, state: AppState) -> Result<(), ServerError> {
    start_server_with_options(config, state, ServerOptions::default()).await
}

/// Serve with explicit-origin CORS, bounded concurrency and body limits, and a
/// graceful shutdown signal. Pass a closed/terminated signal to drain.
pub async fn start_server_with_options(
    config: ServerConfig,
    state: AppState,
    options: ServerOptions,
) -> Result<(), ServerError> {
    start_server_with_options_and_shutdown(config, state, options, futures::future::pending()).await
}

/// Fully explicit variant accepting a custom shutdown future for graceful drain.
pub async fn start_server_with_options_and_shutdown(
    config: ServerConfig,
    state: AppState,
    options: ServerOptions,
    shutdown_signal: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    start_server_with_options_and_shutdown_timeout(
        config,
        state,
        options,
        shutdown_signal,
        GRACEFUL_SHUTDOWN_TIMEOUT,
    )
    .await
}

/// Variant with an explicit graceful-drain deadline, primarily for embedders
/// with a stricter process supervisor deadline.
pub async fn start_server_with_options_and_shutdown_timeout(
    config: ServerConfig,
    state: AppState,
    options: ServerOptions,
    shutdown_signal: impl Future<Output = ()> + Send + 'static,
    drain_timeout: Duration,
) -> Result<(), ServerError> {
    if !config.bind_addr.ip().is_loopback() && config.api_key.is_none() {
        return Err(ServerError::Bind(
            "an API key is required when binding outside localhost".to_string(),
        ));
    }

    let bind_addr = config.bind_addr;
    let router = build_router_with_options(config, state, options);
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .map_err(|error| ServerError::Bind(error.to_string()))?;
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel();
    let serve = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = drain_rx.await;
        })
        .into_future();
    tokio::pin!(serve);
    tokio::pin!(shutdown_signal);

    tokio::select! {
        result = &mut serve => result.map_err(|error| ServerError::Serve(error.to_string())),
        () = &mut shutdown_signal => {
            let _ = drain_tx.send(());
            tokio::time::timeout(drain_timeout, &mut serve)
                .await
                .map_err(|_| {
                    tracing::error!(
                        timeout_secs = drain_timeout.as_secs_f64(),
                        "HTTP server graceful shutdown timed out"
                    );
                    ServerError::ShutdownTimeout(drain_timeout)
                })?
                .map_err(|error| ServerError::Serve(error.to_string()))
        }
    }
}
async fn security_headers_middleware(request: Request, next: Next) -> Response {
    let requested_gallery = request.uri().path() == "/gallery";
    let mut response = next.run(request).await;
    let gallery_html = requested_gallery
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html"));
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(if gallery_html {
            GALLERY_CSP
        } else {
            DEFAULT_CSP
        }),
    );
    response
}
async fn endpoint_error_middleware(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let status = response.status();
    let (message, code) = match status {
        StatusCode::METHOD_NOT_ALLOWED => ("Method not allowed", "method_not_allowed"),
        StatusCode::PAYLOAD_TOO_LARGE => ("Request body too large", "request_too_large"),
        _ => return response,
    };
    let body = OpenAiErrorResponse::with_code(message, "invalid_request_error", code);
    (status, Json(body)).into_response()
}

// ── Authentication middleware ──────────────────────────────────────────────────

async fn public_middleware(
    State(state): State<PublicMiddlewareState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    if let Some(expected) = &state.api_key {
        let authorized = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()));

        if !authorized {
            let body = OpenAiErrorResponse::with_code(
                "Invalid API key",
                "authentication_error",
                "invalid_api_key",
            );
            return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
        }
    } else if state.unauthenticated_loopback
        && matches!(
            request.method(),
            &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
        )
        && is_cross_origin_browser_request(&headers, request.uri())
    {
        let body = OpenAiErrorResponse::with_code(
            "Cross-origin browser mutations require an API key",
            "invalid_request_error",
            "cross_origin_request",
        );
        return (StatusCode::FORBIDDEN, Json(body)).into_response();
    }

    if let Some(limiter) = &state.rate_limiter {
        let client_id = if state.api_key.is_some() {
            "authenticated"
        } else {
            "anonymous"
        };
        if let AdmissionDecision::Reject {
            retry_after,
            error_type,
            error_code,
            ..
        } = limiter.try_acquire(client_id, SystemTime::now())
        {
            let retry_after_secs = retry_after.as_secs().max(1);
            let body =
                OpenAiErrorResponse::with_code("Rate limit exceeded", error_type, error_code);
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, retry_after_secs.to_string())],
                Json(body),
            )
                .into_response();
        }
    }
    next.run(request).await
}

fn is_cross_origin_browser_request(headers: &HeaderMap, request_uri: &Uri) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|site| matches!(site, "cross-site" | "same-site"))
    {
        return true;
    }

    let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Ok(origin) = origin.parse::<Uri>() else {
        return true;
    };
    let Some(origin_authority) = origin.authority() else {
        return true;
    };
    let request_authority = request_uri
        .authority()
        .map(|authority| authority.as_str())
        .or_else(|| headers.get(HOST).and_then(|value| value.to_str().ok()));

    origin.scheme_str() != Some("http")
        || request_authority.is_none_or(|authority| origin_authority.as_str() != authority)
}

async fn gallery_delete_auth_middleware(
    State(state): State<DeleteAuthCheckState>,
    request: Request,
    next: Next,
) -> Response {
    if !state.is_localhost && !state.has_api_key {
        let body = OpenAiErrorResponse::with_code(
            "API key required for gallery deletion on non-local bind",
            "authentication_error",
            "api_key_required",
        );
        return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    }
    next.run(request).await
}

async fn audit_middleware(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("missing-request-id")
        .to_owned();
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();

    let response = next.run(request).await;
    let fields = RedactionFilter::redact_json(&json!({
        "request_id": request_id,
        "timestamp": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs(),
        "method": method,
        "path": path,
        "status": response.status().as_u16(),
        "duration_ms": started.elapsed().as_millis() as u64,
    }));
    tracing::info!(target: "gemini_bridge::audit", fields = %fields, "request completed");
    response
}

async fn admin_auth_middleware(
    State(api_key): State<Option<String>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = api_key else {
        let body = OpenAiErrorResponse::with_code(
            "Admin API key is not configured",
            "authentication_error",
            "invalid_api_key",
        );
        return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    };

    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()));

    if !authorized {
        let body = OpenAiErrorResponse::with_code(
            "Unauthorized",
            "authentication_error",
            "invalid_api_key",
        );
        return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    }

    next.run(request).await
}

/// Create a health/admin service for an optional identity provider.
pub fn build_health_admin(
    identity: Option<Arc<dyn gemini_bridge_identity::IdentityService>>,
) -> Arc<dyn HealthAdminService> {
    Arc::new(DefaultHealthAdminService::new(identity))
}

/// Return type for [`build_reloadable_gemini_adapter`].
pub type ReloadableGeminiAdapter = (Arc<dyn LlmAdapter>, Arc<dyn PluginReloader>);

/// Construct a reloadable Gemini adapter and its production built-in reloader.
///
/// The returned proxy is used for chat requests; attach the returned reloader to
/// `DefaultHealthAdminService::with_reloader` to enable the protected admin route.
pub fn build_reloadable_gemini_adapter(
    config: Arc<gemini_bridge_config::BridgeConfig>,
    identity: Arc<DefaultIdentityService>,
) -> Result<ReloadableGeminiAdapter, ServerError> {
    let initial = Arc::new(
        gemini_bridge_adapter_gemini::DefaultGeminiAdapter::new(identity.clone(), config.clone())
            .map_err(|error| ServerError::AdapterInitialization(error.to_string()))?,
    ) as Arc<dyn LlmAdapter>;
    let proxy = Arc::new(ReloadableAdapter::new(initial));
    let reloader = Arc::new(GeminiAdapterReloader {
        slot: proxy.slot(),
        config,
        identity,
    });
    Ok((proxy, reloader))
}

struct GeminiAdapterReloader {
    slot: gemini_bridge_plugin_context::ReloadableSlot<dyn LlmAdapter>,
    config: Arc<gemini_bridge_config::BridgeConfig>,
    identity: Arc<DefaultIdentityService>,
}

#[async_trait::async_trait]
impl PluginReloader for GeminiAdapterReloader {
    async fn prepare(&self, plugin_name: &str) -> Result<ReloadCommit, HealthAdminError> {
        if plugin_name != "gemini-adapter" {
            return Err(HealthAdminError::ReloadFailed(format!(
                "Unknown plugin: {plugin_name}"
            )));
        }

        let replacement = Arc::new(
            gemini_bridge_adapter_gemini::DefaultGeminiAdapter::new(
                self.identity.clone(),
                self.config.clone(),
            )
            .map_err(|error| HealthAdminError::ReloadFailed(error.to_string()))?,
        ) as Arc<dyn LlmAdapter>;
        let slot = self.slot.clone();
        Ok(Box::new(move || {
            let _old_generation = slot.publish(replacement);
        }))
    }
}

/// Create the production tool-calling engine.
pub fn build_tool_engine() -> Arc<dyn ToolEngine> {
    Arc::new(DefaultToolEngine)
}
