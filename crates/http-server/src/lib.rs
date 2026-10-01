//! Axum HTTP server for gemini-bridge.
//!
//! Exposes all API routes, enforces authentication, propagates request IDs,
//! and wires handlers to the LLM adapter.

pub mod handlers;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing};
use thiserror::Error;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

use gemini_bridge_health_admin::{DefaultHealthAdminService, HealthAdminService};
use gemini_bridge_llm_service::LlmAdapter;
use gemini_bridge_openai_compat::OpenAiErrorResponse;

// ── Public types ──────────────────────────────────────────────────────────────

/// Server configuration at the HTTP layer.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_addr: SocketAddr,
    pub api_key: Option<String>,
    pub require_key_for_admin: bool,
    pub cors_enabled: bool,
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
        .route_layer(middleware::from_fn_with_state(
            api_key.clone(),
            auth_middleware,
        ));

    let health_routes = Router::new()
        .route("/healthz", routing::get(handlers::health::healthz))
        .route("/readyz", routing::get(handlers::health::readyz));

    let admin_routes = Router::new()
        .route("/admin/status", routing::get(handlers::admin::admin_status))
        .route("/admin/reauth", routing::post(handlers::admin::reauth))
        .route_layer(middleware::from_fn_with_state(
            api_key,
            admin_auth_middleware,
        ));

    Router::new()
        .merge(public_routes)
        .merge(health_routes)
        .merge(admin_routes)
        .with_state(state)
        .layer(PropagateRequestIdLayer::new(x_request_id.clone()))
        .layer(SetRequestIdLayer::new(x_request_id, MakeRequestUuid))
}

// ── Authentication middleware ──────────────────────────────────────────────────

async fn auth_middleware(
    State(api_key): State<Option<String>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    if let Some(expected) = &api_key {
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
    next.run(request).await
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
