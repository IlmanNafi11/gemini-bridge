//! Axum HTTP server for gemini-bridge.
//!
//! Exposes all API routes, enforces authentication, propagates request IDs,
//! and wires handlers to the LLM adapter.

pub mod handlers;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing};
use gemini_bridge_conversation_store::ConversationStore;
use gemini_bridge_health_admin::{
    DefaultHealthAdminService, HealthAdminService, MediaPurgeAdminService,
};
use gemini_bridge_llm_service::LlmAdapter;
use gemini_bridge_middleware::{
    AdmissionDecision, RedactionFilter, TokenBucketConfig, TokenBucketLimiter,
};
use gemini_bridge_openai_compat::OpenAiErrorResponse;
use gemini_bridge_tool_calling::{DefaultToolEngine, ToolEngine};
use serde_json::json;
use thiserror::Error;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

const REQUEST_ID_HEADER: &str = "x-request-id";

#[derive(Clone)]
struct PublicMiddlewareState {
    api_key: Option<String>,
    rate_limiter: Option<Arc<TokenBucketLimiter>>,
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
    /// Optional per-client request rate limit for authenticated public routes.
    pub rate_limit: Option<TokenBucketConfig>,
}
/// Shared state threaded through all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub adapter: Arc<dyn LlmAdapter>,
    /// File upload/retrieval service; `None` disables `/v1/files` routes.
    pub upload_service: Option<Arc<dyn gemini_bridge_upload::UploadService>>,
    /// Image generation/retrieval service; `None` disables `/v1/images` routes.
    pub image_service: Option<Arc<dyn gemini_bridge_image_gen::ImageGenService>>,
    /// Process and session operations used by health/admin routes.
    pub health_admin: Arc<dyn HealthAdminService>,
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
}

// ── Router builder ─────────────────────────────────────────────────────────────

/// Build the Axum router. Does not start listening — the caller binds the
/// `TcpListener` and calls `axum::serve`.
pub fn build_router(config: ServerConfig, state: AppState) -> Router {
    let x_request_id = header::HeaderName::from_static("x-request-id");
    let api_key = config.api_key.clone();

    let public_routes = Router::new()
        .route(
            "/v1/chat/completions",
            routing::post(handlers::chat::chat_completions),
        )
        .route(
            "/v1/models",
            routing::get(handlers::models::list_models_handler),
        )
        .route(
            "/v1/files",
            routing::post(handlers::files::create_file)
                .layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route("/v1/files/{id}", routing::get(handlers::files::get_file))
        .route(
            "/v1/images/generations",
            routing::post(handlers::images::generate_image),
        )
        .route("/v1/images/{id}", routing::get(handlers::images::get_image))
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
            },
            public_middleware,
        ));

    let health_routes = Router::new()
        .route("/healthz", routing::get(handlers::health::healthz))
        .route("/readyz", routing::get(handlers::health::readyz));

    let mut admin_routes = Router::new()
        .route("/admin/status", routing::get(handlers::admin::admin_status))
        .route("/admin/reauth", routing::post(handlers::admin::reauth));
    if state.media_purge.is_some() {
        admin_routes =
            admin_routes.route("/admin/purge", routing::post(handlers::admin::purge_media));
    }
    let admin_routes = admin_routes.route_layer(middleware::from_fn_with_state(
        api_key,
        admin_auth_middleware,
    ));

    Router::new()
        .merge(public_routes)
        .merge(health_routes)
        .merge(admin_routes)
        .with_state(state)
        .layer(middleware::from_fn(audit_middleware))
        .layer(PropagateRequestIdLayer::new(x_request_id.clone()))
        .layer(SetRequestIdLayer::new(x_request_id, MakeRequestUuid))
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
            .is_some_and(|token| token == expected);

        if !authorized {
            let body = OpenAiErrorResponse::with_code(
                "Invalid API key",
                "authentication_error",
                "invalid_api_key",
            );
            return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
        }
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
        .is_some_and(|token| token == expected);

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

/// Create the production tool-calling engine.
pub fn build_tool_engine() -> Arc<dyn ToolEngine> {
    Arc::new(DefaultToolEngine)
}
