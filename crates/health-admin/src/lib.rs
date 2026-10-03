//! Health, readiness, admin status, and session re-authentication service.

pub mod dashboard;
pub mod purge;
pub mod readiness;
pub mod reload_handler;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;

use async_trait::async_trait;
use gemini_bridge_identity::{IdentityService, SessionStatus};

pub use dashboard::render_dashboard;
pub use purge::{DefaultMediaPurgeAdminService, MediaPurgeAdminService, PurgeResult};
pub use readiness::{
    AdminStatusResponse, HealthAdminError, HealthResponse, ReadinessResponse, ReauthResponse,
};

/// Public trait for health, readiness, and administration operations.
#[async_trait]
pub trait HealthAdminService: Send + Sync {
    /// Return process liveness status.
    async fn health(&self) -> HealthResponse;

    /// Return session readiness state derived from identity snapshot.
    async fn readiness(&self) -> ReadinessResponse;

    /// Return full diagnostic admin status snapshot.
    async fn admin_status(&self) -> AdminStatusResponse;

    /// Validate candidate credentials before atomically replacing persisted and live state.
    async fn reauth(&self, raw_cookie_header: &str) -> Result<ReauthResponse, HealthAdminError>;

    /// Hot-swap a named plugin instance (Fase 3).
    async fn reload_plugin(&self, plugin_name: &str) -> Result<(), HealthAdminError>;
}

/// A prepared replacement whose commit performs only the atomic publication.
pub type ReloadCommit = Box<dyn FnOnce() + Send>;

/// Trait for preparing built-in instance replacements.
#[async_trait]
pub trait PluginReloader: Send + Sync {
    /// Fully initialize and validate a replacement without changing live state.
    async fn prepare(&self, plugin_name: &str) -> Result<ReloadCommit, HealthAdminError>;
}

/// Default implementation of [`HealthAdminService`].
pub struct DefaultHealthAdminService {
    identity_service: Option<Arc<dyn IdentityService>>,
    reloader: Option<Arc<dyn PluginReloader>>,
    reload_lock: Mutex<()>,
    start_time: Instant,
    version: &'static str,
}

impl DefaultHealthAdminService {
    /// Construct a new [`DefaultHealthAdminService`].
    pub fn new(identity_service: Option<Arc<dyn IdentityService>>) -> Self {
        Self {
            identity_service,
            reloader: None,
            reload_lock: Mutex::new(()),
            start_time: Instant::now(),
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// Construct with a custom version string and start time (useful in testing).
    pub fn with_details(
        identity_service: Option<Arc<dyn IdentityService>>,
        start_time: Instant,
        version: &'static str,
    ) -> Self {
        Self {
            identity_service,
            reloader: None,
            reload_lock: Mutex::new(()),
            start_time,
            version,
        }
    }

    /// Attach the reloader implementation.
    pub fn with_reloader(mut self, reloader: Arc<dyn PluginReloader>) -> Self {
        self.reloader = Some(reloader);
        self
    }
}

pub fn session_status_to_str(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Valid => "valid",
        SessionStatus::Stale => "stale",
        SessionStatus::NeedsReauth => "needs_reauth",
        SessionStatus::IpFlagged => "ip_flagged",
        SessionStatus::Unconfigured => "unconfigured",
    }
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[async_trait]
impl HealthAdminService for DefaultHealthAdminService {
    async fn health(&self) -> HealthResponse {
        HealthResponse {
            status: "ok",
            uptime_secs: self.start_time.elapsed().as_secs(),
            version: self.version,
        }
    }

    async fn readiness(&self) -> ReadinessResponse {
        let now = current_unix_timestamp();
        if let Some(identity) = &self.identity_service {
            let snapshot = identity.snapshot().await;
            let checked_at = snapshot
                .checked_at
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(now);
            ReadinessResponse {
                session_status: session_status_to_str(snapshot.status).to_string(),
                build_label: snapshot.build_label,
                cookie_age_secs: snapshot.cookie_age.map(|d| d.as_secs()),
                checked_at,
            }
        } else {
            ReadinessResponse {
                session_status: "unconfigured".to_string(),
                build_label: None,
                cookie_age_secs: None,
                checked_at: now,
            }
        }
    }

    async fn admin_status(&self) -> AdminStatusResponse {
        let now = current_unix_timestamp();
        if let Some(identity) = &self.identity_service {
            let snapshot = identity.snapshot().await;
            let checked_at = snapshot
                .checked_at
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(now);
            let ip_flagged = snapshot.status == SessionStatus::IpFlagged;
            let last_valid_at = if snapshot.status == SessionStatus::Valid {
                Some(checked_at)
            } else {
                None
            };
            AdminStatusResponse {
                session_status: session_status_to_str(snapshot.status).to_string(),
                build_label: snapshot.build_label,
                cookie_age_secs: snapshot.cookie_age.map(|d| d.as_secs()),
                last_valid_at,
                ip_flagged,
                checked_at,
            }
        } else {
            AdminStatusResponse {
                session_status: "unconfigured".to_string(),
                build_label: None,
                cookie_age_secs: None,
                last_valid_at: None,
                ip_flagged: false,
                checked_at: now,
            }
        }
    }

    async fn reauth(&self, raw_cookie_header: &str) -> Result<ReauthResponse, HealthAdminError> {
        let identity = self.identity_service.as_ref().ok_or_else(|| {
            HealthAdminError::ReauthFailed("No identity service configured".into())
        })?;

        let bootstrap = identity
            .import_credentials_validated(raw_cookie_header)
            .await
            .map_err(|e| HealthAdminError::ReauthFailed(e.to_string()))?;

        let snapshot = identity.snapshot().await;

        Ok(ReauthResponse {
            session_status: session_status_to_str(snapshot.status).to_string(),
            build_label: Some(bootstrap.bl),
        })
    }

    async fn reload_plugin(&self, plugin_name: &str) -> Result<(), HealthAdminError> {
        let _reload_guard = self.reload_lock.try_lock().map_err(|_| {
            HealthAdminError::ReloadFailed("Another plugin reload is already in progress".into())
        })?;
        if let Some(reloader) = &self.reloader {
            reload_handler::execute_reload_with_deadline(reloader.as_ref(), plugin_name).await
        } else {
            Err(HealthAdminError::ReloadFailed(format!(
                "Reload for '{plugin_name}' is not supported in this configuration"
            )))
        }
    }
}
