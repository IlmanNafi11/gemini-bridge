//! Unit tests for health-admin service logic and state transitions.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use gemini_bridge_health_admin::{
    AdminStatusResponse, DefaultHealthAdminService, HealthAdminError, HealthAdminService,
    ReadinessResponse,
};
use gemini_bridge_identity::{
    IdentityError, IdentityService, SessionBootstrap, SessionSnapshot, SessionStatus,
};
use http::HeaderMap;

struct MockIdentity {
    status: SessionStatus,
    build_label: Option<String>,
    cookie_age: Option<Duration>,
    fail_import: bool,
    fail_bootstrap: bool,
}

#[async_trait]
impl IdentityService for MockIdentity {
    async fn bootstrap(&self) -> Result<SessionBootstrap, IdentityError> {
        if self.fail_bootstrap {
            return Err(IdentityError::NeedsReauth);
        }
        Ok(SessionBootstrap {
            bl: "bootstrap_bl".to_string(),
            snlm0e: "snlm0e".to_string(),
            fsid: "fsid".to_string(),
        })
    }

    async fn refresh_1psidts(&self) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            status: self.status,
            build_label: self.build_label.clone(),
            cookie_age: self.cookie_age,
            checked_at: SystemTime::now(),
        }
    }

    fn apply_auth_headers(&self, _headers: &mut HeaderMap) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn import_credentials(&self, raw: &str) -> Result<(), IdentityError> {
        if self.fail_import || raw.is_empty() {
            return Err(IdentityError::MissingCredentials);
        }
        Ok(())
    }

    async fn import_credentials_validated(
        &self,
        raw: &str,
    ) -> Result<SessionBootstrap, IdentityError> {
        self.import_credentials(raw).await?;
        self.bootstrap().await
    }
}

#[tokio::test]
async fn health_reports_uptime_and_version() {
    let service = DefaultHealthAdminService::with_details(
        None,
        Instant::now() - Duration::from_secs(10),
        "0.1.0-test",
    );

    let res = service.health().await;
    assert_eq!(res.status, "ok");
    assert!(res.uptime_secs >= 10);
    assert_eq!(res.version, "0.1.0-test");
}

#[tokio::test]
async fn readiness_with_unconfigured_identity() {
    let service = DefaultHealthAdminService::new(None);
    let res: ReadinessResponse = service.readiness().await;

    assert_eq!(res.session_status, "unconfigured");
    assert!(res.build_label.is_none());
    assert!(res.cookie_age_secs.is_none());
}

#[tokio::test]
async fn readiness_reflects_valid_identity_state() {
    let identity = Arc::new(MockIdentity {
        status: SessionStatus::Valid,
        build_label: Some("bl_123".to_string()),
        cookie_age: Some(Duration::from_secs(300)),
        fail_import: false,
        fail_bootstrap: false,
    });
    let service = DefaultHealthAdminService::new(Some(identity));
    let res = service.readiness().await;

    assert_eq!(res.session_status, "valid");
    assert_eq!(res.build_label.as_deref(), Some("bl_123"));
    assert_eq!(res.cookie_age_secs, Some(300));
}

#[tokio::test]
async fn admin_status_returns_diagnostic_fields() {
    let identity = Arc::new(MockIdentity {
        status: SessionStatus::Valid,
        build_label: Some("bl_123".to_string()),
        cookie_age: Some(Duration::from_secs(600)),
        fail_import: false,
        fail_bootstrap: false,
    });
    let service = DefaultHealthAdminService::new(Some(identity));
    let res: AdminStatusResponse = service.admin_status().await;

    assert_eq!(res.session_status, "valid");
    assert_eq!(res.build_label.as_deref(), Some("bl_123"));
    assert_eq!(res.cookie_age_secs, Some(600));
    assert!(res.last_valid_at.is_some());
    assert!(!res.ip_flagged);
}

#[tokio::test]
async fn reauth_flow_success_and_failure() {
    let identity = Arc::new(MockIdentity {
        status: SessionStatus::Valid,
        build_label: Some("new_bl".to_string()),
        cookie_age: Some(Duration::from_secs(1)),
        fail_import: false,
        fail_bootstrap: false,
    });
    let service = DefaultHealthAdminService::new(Some(identity));
    let ok = service.reauth("valid_cookie_str").await;
    assert!(ok.is_ok());

    let failing_id = Arc::new(MockIdentity {
        status: SessionStatus::NeedsReauth,
        build_label: None,
        cookie_age: None,
        fail_import: true,
        fail_bootstrap: false,
    });
    let service_fail = DefaultHealthAdminService::new(Some(failing_id));
    let err = service_fail.reauth("bad_cookie").await;
    assert!(matches!(err, Err(HealthAdminError::ReauthFailed(_))));
}
