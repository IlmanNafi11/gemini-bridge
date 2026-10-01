//! Health, readiness, and admin response models and utilities.

use serde::Serialize;
use thiserror::Error;

/// Response payload for `GET /healthz`.
/// Always returns HTTP 200 as long as the process is alive.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    /// Always "ok" when the process is alive.
    pub status: &'static str,
    /// Seconds since the server process started.
    pub uptime_secs: u64,
    /// Semantic version string of the bridge binary.
    pub version: &'static str,
}

/// Response payload for `GET /readyz`.
/// HTTP 200 if session status is Valid; HTTP 503 for Stale/NeedsReauth/IpFlagged/Unconfigured.
#[derive(Debug, Clone, Serialize)]
pub struct ReadinessResponse {
    /// Human-readable session status: "valid", "stale", "needs_reauth", "ip_flagged", "unconfigured".
    pub session_status: String,
    /// Current Gemini build label (`bl`), if a successful bootstrap has occurred.
    pub build_label: Option<String>,
    /// Seconds since the `__Secure-1PSIDTS` cookie was last refreshed. None if unknown.
    pub cookie_age_secs: Option<u64>,
    /// Unix timestamp (seconds) when this snapshot was captured.
    pub checked_at: i64,
}

/// Response payload for `GET /admin/status`.
/// Contains the full operator-facing diagnostic snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct AdminStatusResponse {
    /// Current session status.
    pub session_status: String,
    /// Current build label, if known.
    pub build_label: Option<String>,
    /// Seconds since last `1PSIDTS` refresh. None if not yet rotated.
    pub cookie_age_secs: Option<u64>,
    /// Unix timestamp when the session was last confirmed valid by an upstream check.
    pub last_valid_at: Option<i64>,
    /// Whether IP-flagging was last detected on the upstream.
    pub ip_flagged: bool,
    /// Unix timestamp when this status was captured.
    pub checked_at: i64,
}

/// Response for `POST /admin/reauth` on success.
#[derive(Debug, Clone, Serialize)]
pub struct ReauthResponse {
    /// New session status after re-authentication attempt.
    pub session_status: String,
    /// Build label obtained from re-bootstrap, if successful.
    pub build_label: Option<String>,
}

/// Error type for health-admin operations.
#[derive(Debug, Error)]
pub enum HealthAdminError {
    #[error("Reauth failed: {0}")]
    ReauthFailed(String),

    #[error("Reload failed: {0}")]
    ReloadFailed(String),

    #[error("Unauthorized")]
    Unauthorized,
}
