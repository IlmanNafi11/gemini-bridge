//! HTTP handlers for health probes and administrative session operations.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, body::Bytes};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use gemini_bridge_health_admin::HealthAdminError;

/// `GET /healthz`: process liveness, uptime, and version.
pub async fn healthz(State(state): State<AppState>) -> Response {
    let response = state.health_admin.health().await;
    (StatusCode::OK, Json(response)).into_response()
}

/// `GET /readyz`: session readiness snapshot.
pub async fn readyz(State(state): State<AppState>) -> Response {
    let response = state.health_admin.readiness().await;
    let status = if response.session_status == "valid" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(response)).into_response()
}

/// `GET /admin/status`: operator-facing session diagnostics.
pub async fn admin_status(State(state): State<AppState>) -> Response {
    let response = state.health_admin.admin_status().await;
    (StatusCode::OK, Json(response)).into_response()
}

/// `GET /admin/dashboard`: minimal HTML from local process and session snapshots.
pub async fn dashboard(State(state): State<AppState>) -> Response {
    let health = state.health_admin.health().await;
    let status = state.health_admin.admin_status().await;
    let html = gemini_bridge_health_admin::render_dashboard(&health, &status);
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

#[derive(Deserialize)]
struct ReauthRequest {
    #[serde(alias = "raw_cookie_header")]
    cookie: String,
}

/// `POST /admin/reauth`: import cookies and bootstrap the upstream session.
pub async fn reauth(State(state): State<AppState>, body: Bytes) -> Response {
    let request: ReauthRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": "Invalid reauthentication request; expected JSON containing a cookie field",
                        "type": "invalid_request_error",
                        "code": "invalid_reauth_request"
                    }
                })),
            )
                .into_response();
        }
    };

    match state.health_admin.reauth(&request.cookie).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => reauth_error(error),
    }
}

#[derive(Deserialize)]
struct ReloadPluginRequest {
    plugin: String,
}

/// `POST /admin/reload-plugin`: replace a built-in plugin instance atomically.
pub async fn reload_plugin(State(state): State<AppState>, body: Bytes) -> Response {
    let request: ReloadPluginRequest = match serde_json::from_slice::<ReloadPluginRequest>(&body) {
        Ok(request) if !request.plugin.trim().is_empty() => request,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": "Invalid reload request; expected JSON containing a non-empty plugin field",
                        "type": "invalid_request_error",
                        "code": "invalid_reload_request"
                    }
                })),
            )
                .into_response();
        }
    };

    match state.health_admin.reload_plugin(&request.plugin).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"status": "ok", "plugin": request.plugin})),
        )
            .into_response(),
        Err(error) => reauth_error(error),
    }
}

/// `POST /admin/purge`: remove expired cached media and metadata.
pub async fn purge_media(State(state): State<AppState>) -> Response {
    let Some(service) = &state.media_purge else {
        return StatusCode::NOT_FOUND.into_response();
    };

    match service.purge_expired().await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(HealthAdminError::PurgeFailed(message)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "message": message,
                    "type": "server_error",
                    "code": "media_purge_failed"
                }
            })),
        )
            .into_response(),
        Err(error) => reauth_error(error),
    }
}

fn reauth_error(error: HealthAdminError) -> Response {
    let (status, message, code) = match error {
        HealthAdminError::ReauthFailed(message) => {
            (StatusCode::BAD_GATEWAY, message, "reauth_failed")
        }
        HealthAdminError::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            "Unauthorized".to_owned(),
            "invalid_api_key",
        ),
        HealthAdminError::ReloadFailed(_message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Plugin reload failed".to_owned(),
            "reload_failed",
        ),
        HealthAdminError::PurgeFailed(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            message,
            "media_purge_failed",
        ),
    };

    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": "server_error",
                "code": code
            }
        })),
    )
        .into_response()
}
