use serde::{Deserialize, Serialize};
use std::fmt;

use crate::crypto::Redacted;

/// Parsed Google session cookies needed to drive Gemini Web.
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionCredentials {
    pub psid: String,
    pub psidts: String,
    pub sapisid: String,
    /// ISO-8601 timestamp of when credentials were imported.
    pub imported_at: String,
}

impl fmt::Debug for SessionCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionCredentials")
            .field("psid", &Redacted("<redacted>".to_string()))
            .field("psidts", &Redacted("<redacted>".to_string()))
            .field("sapisid", &Redacted("<redacted>".to_string()))
            .field("imported_at", &self.imported_at)
            .finish()
    }
}

/// Tokens extracted from the Gemini `/app` page.
pub struct SessionBootstrap {
    pub bl: String,
    pub snlm0e: String,
    pub fsid: String,
}

impl fmt::Debug for SessionBootstrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // bl and fsid are build labels / session IDs – redact from debug.
        f.debug_struct("SessionBootstrap")
            .field("bl", &Redacted(self.bl.clone()))
            .field("snlm0e", &Redacted(self.snlm0e.clone()))
            .field("fsid", &Redacted(self.fsid.clone()))
            .finish()
    }
}

/// Current state of the identity session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// No credentials have been imported yet.
    Unconfigured,
    /// Credentials are present and bootstrap succeeded.
    Valid,
    /// Credentials are present but have not been verified recently.
    Stale,
    /// Credentials are expired or rejected by upstream.
    NeedsReauth,
    /// Upstream redirected to sorry page – IP is flagged.
    IpFlagged,
}

/// A point-in-time snapshot of session health for display / health endpoints.
pub struct SessionSnapshot {
    pub status: SessionStatus,
    pub build_label: Option<String>,
    pub cookie_age: Option<std::time::Duration>,
    pub checked_at: std::time::SystemTime,
}

impl fmt::Debug for SessionSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionSnapshot")
            .field("status", &self.status)
            .field("build_label", &self.build_label)
            .field("cookie_age_secs", &self.cookie_age.map(|d| d.as_secs()))
            .field("checked_at", &self.checked_at)
            .finish()
    }
}
