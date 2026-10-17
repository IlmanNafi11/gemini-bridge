//! Handlers for the `/v1/sessions` routes (Fase 4, V.4).
//!
//! Contract (SPEC-session-vault.md §6.1):
//! - `POST /v1/sessions` (api_key only — no session token exists yet):
//!   parse cookie → probe `/app` → persist encrypted → `{id, token,
//!   session_status}`. The raw token is returned exactly once and stored
//!   only as a hash. A failed probe leaves any prior row untouched.
//! - `GET /v1/sessions/current` (api_key + `X-Session-Token`): returns only
//!   the caller's session state, never secret material.
//! - `DELETE /v1/sessions/{id}` (api_key + token matching `{id}`): purges the
//!   row; subsequent requests with that token resolve to
//!   `401 invalid_session_token`. Cross-tenant access is rejected.
//!
//! Route authentication is layered in `build_router`: these handlers sit on a
//! sub-router whose middleware enforces api_key but not a session token, while
//! every other `/v1/*` route additionally requires a resolvable token.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use gemini_bridge_identity::{IdentityError, generate_session_id};
use gemini_bridge_openai_compat::OpenAiErrorResponse;

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    pub cookie: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// `POST /v1/sessions`: create or atomically replace the caller's session.
pub async fn create_session(
    State(state): State<AppState>,
    body: Json<CreateSessionRequest>,
) -> Response {
    let Some(vault) = state.vault.as_ref() else {
        return vault_unconfigured();
    };

    let request = body.0;
    if request.cookie.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Missing cookie field",
            "invalid_session_request",
        );
    }

    // Raw token is generated here and returned exactly once; only its hash is
    // ever persisted (spec §5).
    let token = gemini_bridge_identity::generate_session_token();
    let id = generate_session_id();
    let label = request.label.as_deref().filter(|l| !l.trim().is_empty());

    match vault
        .create_validated(&id, &token, label, &request.cookie)
        .await
    {
        Ok(session) => {
            let status = gemini_bridge_health_admin::session_status_to_str(session.status);
            (
                StatusCode::OK,
                Json(json!({
                    "id": session.id,
                    "token": token,
                    "session_status": status,
                })),
            )
                .into_response()
        }
        Err(error) => create_error(error),
    }
}

/// `GET /v1/sessions/current`: caller's own session state only.
pub async fn current_session(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(vault) = state.vault.as_ref() else {
        return vault_unconfigured();
    };

    let Some(row) = resolve_caller(vault, &headers) else {
        return invalid_token();
    };

    let status = gemini_bridge_health_admin::session_status_to_str(row.status);
    (
        StatusCode::OK,
        Json(json!({
            "id": row.id,
            "label": row.label,
            "session_status": status,
            "imported_at": row.imported_at,
        })),
    )
        .into_response()
}

/// `DELETE /v1/sessions/{id}`: purge the row, only when the presented token
/// owns that id.
pub async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(vault) = state.vault.as_ref() else {
        return vault_unconfigured();
    };

    let resolved = match resolve_caller(vault, &headers) {
        Some(resolved) => resolved,
        None => return invalid_token(),
    };
    if resolved.id != id {
        // Ownership check: cross-tenant access is rejected without leaking
        // whether the target id exists.
        return error_response(
            StatusCode::FORBIDDEN,
            "Session token does not own this session",
            "invalid_session_token",
        );
    }

    match vault.store().delete(&id) {
        Ok(true) => (StatusCode::OK, Json(json!({"deleted": true, "id": id}))).into_response(),
        Ok(false) => error_response(StatusCode::NOT_FOUND, "Session not found", "session_not_found"),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Session deletion failed",
            "session_delete_failed",
        ),
    }
}

/// Resolve the caller's vault row from their `X-Session-Token` header using
/// the same hash lookup the dual-auth middleware performs.
fn resolve_caller(
    vault: &gemini_bridge_identity::VaultService,
    headers: &axum::http::HeaderMap,
) -> Option<gemini_bridge_identity::VaultSession> {
    let token = headers
        .get("x-session-token")
        .and_then(|value| value.to_str().ok())?;
    vault
        .store()
        .find_by_token_hash(&gemini_bridge_identity::hash_session_token(token))
        .ok()
        .flatten()
}

fn invalid_token() -> Response {
    error_response(
        StatusCode::UNAUTHORIZED,
        "Invalid session token",
        "invalid_session_token",
    )
}

fn vault_unconfigured() -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        "Session vault is not configured",
        "vault_unconfigured",
    )
}

fn error_response(status: StatusCode, message: &str, code: &str) -> Response {
    (
        status,
        Json(OpenAiErrorResponse::with_code(
            message,
            "invalid_request_error",
            code,
        )),
    )
        .into_response()
}

/// Map `create_validated` failures to the stable error taxonomy. Probe
/// failures surface as upstream-authentication errors; the prior session row
/// is untouched by contract.
fn create_error(error: IdentityError) -> Response {
    let (status, message, code, error_type) = match error {
        IdentityError::MissingCredentials => (
            StatusCode::BAD_REQUEST,
            "Cookie header is missing required fields (need __Secure-1PSID, __Secure-1PSIDTS, SAPISID)",
            "invalid_session_request",
            "invalid_request_error",
        ),
        IdentityError::NeedsReauth => (
            StatusCode::UNAUTHORIZED,
            "Upstream rejected the supplied credentials; supply a fresh cookie",
            "needs_reauth",
            "authentication_error",
        ),
        IdentityError::IpFlagged => (
            StatusCode::FORBIDDEN,
            "Upstream flagged this client; supply a fresh cookie from an acceptable network",
            "ip_flagged",
            "authentication_error",
        ),
        IdentityError::WeakBridgeSecret => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "BRIDGE_SECRET is not configured or too weak; cookies cannot be encrypted",
            "weak_bridge_secret",
            "server_error",
        ),
        IdentityError::Transport => (
            StatusCode::BAD_GATEWAY,
            "Could not reach the upstream to validate the credentials",
            "upstream_unavailable",
            "api_error",
        ),
        IdentityError::Storage | IdentityError::Vault(_) | IdentityError::LegacyCredentialRewrap => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Session persistence failed",
            "session_persistence_failed",
            "server_error",
        ),
        IdentityError::CandidateValidationUnsupported | IdentityError::MissingBootstrapField(_) => (
            StatusCode::BAD_GATEWAY,
            "Upstream response could not be validated",
            "upstream_invalid",
            "api_error",
        ),
    };
    (
        status,
        Json(OpenAiErrorResponse::with_code(message, error_type, code)),
    )
        .into_response()
}
