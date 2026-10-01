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
        HealthAdminError::ReloadFailed(message) => {
            (StatusCode::NOT_IMPLEMENTED, message, "reload_not_supported")
        }
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
