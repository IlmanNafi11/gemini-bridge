use async_trait::async_trait;
use http::HeaderMap;
use parking_lot::Mutex;
use reqwest::Method;
use url::Url;

use gemini_bridge_config::{BridgeConfig, StorageConfig};
use gemini_bridge_transport::{Idempotency, ReqwestTransport, TransportRequest, TransportService};

use crate::error::IdentityError;
use crate::model::{SessionBootstrap, SessionCredentials, SessionSnapshot, SessionStatus};
use crate::parser;
use crate::rotation;
use crate::storage;

#[async_trait]
pub trait IdentityService: Send + Sync {
    /// Bootstrap the Gemini session by hitting `/app` and extracting tokens.
    async fn bootstrap(&self) -> Result<SessionBootstrap, IdentityError>;
    /// Attempt to refresh `__Secure-1PSIDTS` (Fase 1; no-op in Fase 0).
    async fn refresh_1psidts(&self) -> Result<(), IdentityError>;
    /// Return current session snapshot (cheap, non-blocking).
    async fn snapshot(&self) -> SessionSnapshot;
    /// Apply authenticated Gemini session headers without exposing credential values.
    fn apply_auth_headers(&self, headers: &mut HeaderMap) -> Result<(), IdentityError>;
    /// Import raw cookie header string, parse and persist credentials without
    /// probing upstream.
    ///
    /// Legacy entry point retained for providers and tests without a transport;
    /// it does not establish that the credentials are accepted upstream. The
    /// guided re-authentication flow should use
    /// [`IdentityService::import_credentials_validated`].
    async fn import_credentials(&self, raw_cookie_header: &str) -> Result<(), IdentityError>;
    async fn validate_candidate(
        &self,
        _raw_cookie_header: &str,
    ) -> Result<SessionBootstrap, IdentityError> {
        Err(IdentityError::CandidateValidationUnsupported)
    }
    /// Probe candidate credentials, then atomically replace the persisted and
    /// live credentials only after the probe succeeds. Failure leaves both the
    /// previous credential states unchanged. On success, returns bootstrap
    /// tokens extracted during the candidate probe.
    async fn import_credentials_validated(
        &self,
        _raw_cookie_header: &str,
    ) -> Result<SessionBootstrap, IdentityError> {
        Err(IdentityError::CandidateValidationUnsupported)
    }
}

/// Inner mutable state of the identity service.
struct IdentityState {
    credentials: Option<SessionCredentials>,
    last_bootstrap: Option<SessionBootstrap>,
    status: SessionStatus,
    /// Incremented after every completed rotation attempt.
    refresh_epoch: u64,
}

/// Default implementation of [`IdentityService`] backed by the local
/// filesystem cookie store and the shared transport layer.
pub struct DefaultIdentityService {
    transport: ReqwestTransport,
    storage_cfg: StorageConfig,
    base_url: Option<String>,
    state: Mutex<IdentityState>,
    rotation_lock: tokio::sync::Mutex<()>,
}

impl DefaultIdentityService {
    /// Construct from a [`BridgeConfig`].
    ///
    /// Credentials are loaded from disk on construction; if none exist the
    /// service starts in `Unconfigured` state.
    pub fn new(config: &BridgeConfig) -> Result<Self, IdentityError> {
        Self::with_base_url(config, None)
    }

    /// Construct with an optional custom upstream base URL (useful for integration tests).
    pub fn with_base_url(
        config: &BridgeConfig,
        base_url: Option<String>,
    ) -> Result<Self, IdentityError> {
        crate::crypto::validate_bridge_secret()?;
        let transport =
            ReqwestTransport::new(&config.transport).map_err(|_| IdentityError::Transport)?;
        let storage_cfg = config.storage.clone();

        // Attempt to load persisted credentials.
        let credentials = load_credentials(&storage_cfg)?;

        let status = if credentials.is_some() {
            SessionStatus::Stale
        } else {
            SessionStatus::Unconfigured
        };

        Ok(Self {
            transport,
            storage_cfg,
            base_url,
            state: Mutex::new(IdentityState {
                credentials,
                last_bootstrap: None,
                status,
                refresh_epoch: 0,
            }),
            rotation_lock: tokio::sync::Mutex::new(()),
        })
    }
    /// Persist credentials to disk, then install them into live state.
    fn persist_and_install(&self, credentials: SessionCredentials) -> Result<(), IdentityError> {
        let json = serde_json::to_string(&credentials).map_err(|_| IdentityError::Storage)?;
        storage::write_cookies(&self.storage_cfg.data_dir, &json)?;
        let mut state = self.state.lock();
        state.credentials = Some(credentials);
        state.status = SessionStatus::Stale;
        Ok(())
    }
}

#[async_trait]
impl IdentityService for DefaultIdentityService {
    async fn bootstrap(&self) -> Result<SessionBootstrap, IdentityError> {
        let credentials = {
            self.state
                .lock()
                .credentials
                .clone()
                .ok_or(IdentityError::MissingCredentials)?
        };
        let result =
            probe_credentials(&self.transport, self.base_url.as_deref(), &credentials).await;
        match result {
            Ok(bootstrap) => {
                let mut state = self.state.lock();
                state.status = SessionStatus::Valid;
                state.last_bootstrap = Some(SessionBootstrap {
                    bl: bootstrap.bl.clone(),
                    snlm0e: bootstrap.snlm0e.clone(),
                    fsid: bootstrap.fsid.clone(),
                });
                Ok(bootstrap)
            }
            Err(error) => {
                let mut state = self.state.lock();
                state.status = if matches!(error, IdentityError::IpFlagged) {
                    SessionStatus::IpFlagged
                } else {
                    SessionStatus::NeedsReauth
                };
                Err(error)
            }
        }
    }

    async fn refresh_1psidts(&self) -> Result<(), IdentityError> {
        // Single-flight: callers that waited behind a successful refresh reuse it
        // instead of issuing another `/app` request.
        let observed_epoch = self.state.lock().refresh_epoch;
        let _guard = self.rotation_lock.lock().await;
        let creds = {
            let state = self.state.lock();
            if state.refresh_epoch != observed_epoch {
                return match state.status {
                    SessionStatus::Valid => Ok(()),
                    SessionStatus::IpFlagged => Err(IdentityError::IpFlagged),
                    _ => Err(IdentityError::NeedsReauth),
                };
            }
            state
                .credentials
                .clone()
                .ok_or(IdentityError::MissingCredentials)?
        };
        match rotation::execute_rotation(&self.transport, self.base_url.as_deref(), &creds).await {
            Ok(result) => {
                let updated = SessionCredentials {
                    psid: creds.psid,
                    psidts: result.psidts,
                    sapisid: creds.sapisid,
                    imported_at: rfc3339_now(),
                };
                storage::write_cookies(
                    &self.storage_cfg.data_dir,
                    &serde_json::to_string(&updated).map_err(|_| IdentityError::Storage)?,
                )?;
                let mut state = self.state.lock();
                state.credentials = Some(updated);
                state.last_bootstrap = Some(result.bootstrap);
                state.status = SessionStatus::Valid;
                state.refresh_epoch += 1;
                Ok(())
            }
            Err(error) => {
                let mut state = self.state.lock();
                state.status = if matches!(error, IdentityError::IpFlagged) {
                    SessionStatus::IpFlagged
                } else {
                    SessionStatus::NeedsReauth
                };
                state.refresh_epoch += 1;
                Err(error)
            }
        }
    }
    fn apply_auth_headers(&self, headers: &mut HeaderMap) -> Result<(), IdentityError> {
        let (cookie_header, sapisid) = {
            let st = self.state.lock();
            let creds = st
                .credentials
                .as_ref()
                .ok_or(IdentityError::MissingCredentials)?;
            (
                format!(
                    "__Secure-1PSID={}; __Secure-1PSIDTS={}; SAPISID={}",
                    creds.psid, creds.psidts, creds.sapisid
                ),
                creds.sapisid.clone(),
            )
        };

        headers.insert(
            http::header::COOKIE,
            cookie_header
                .parse()
                .map_err(|_| IdentityError::Transport)?,
        );
        headers.insert(
            http::header::AUTHORIZATION,
            parser::build_sapisidhash(&sapisid)
                .parse()
                .map_err(|_| IdentityError::Transport)?,
        );
        headers.insert(
            http::header::USER_AGENT,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36"
                .parse()
                .map_err(|_| IdentityError::Transport)?,
        );
        Ok(())
    }

    async fn snapshot(&self) -> SessionSnapshot {
        let st = self.state.lock();
        let build_label = st.last_bootstrap.as_ref().map(|b| b.bl.clone());
        let cookie_age = st.credentials.as_ref().and_then(|c| {
            // Parse the imported_at timestamp and compute age via std time.
            // Format is RFC3339; use a simple duration calculation.
            parse_age_from_rfc3339(&c.imported_at)
        });
        SessionSnapshot {
            status: st.status,
            build_label,
            cookie_age,
            checked_at: std::time::SystemTime::now(),
        }
    }

    async fn import_credentials(&self, raw_cookie_header: &str) -> Result<(), IdentityError> {
        let credentials =
            parse_cookie_header(raw_cookie_header).ok_or(IdentityError::MissingCredentials)?;
        self.persist_and_install(credentials)
    }
    async fn validate_candidate(
        &self,
        raw_cookie_header: &str,
    ) -> Result<SessionBootstrap, IdentityError> {
        let candidate =
            parse_cookie_header(raw_cookie_header).ok_or(IdentityError::MissingCredentials)?;
        probe_credentials(&self.transport, self.base_url.as_deref(), &candidate).await
    }

    async fn import_credentials_validated(
        &self,
        raw_cookie_header: &str,
    ) -> Result<SessionBootstrap, IdentityError> {
        let _guard = self.rotation_lock.lock().await;
        let candidate =
            parse_cookie_header(raw_cookie_header).ok_or(IdentityError::MissingCredentials)?;
        let bootstrap =
            probe_credentials(&self.transport, self.base_url.as_deref(), &candidate).await?;

        // Only after upstream accepts the candidate do we touch disk or live
        // state; both stay untouched when the probe fails.
        self.persist_and_install(candidate)?;
        let mut state = self.state.lock();
        state.last_bootstrap = Some(SessionBootstrap {
            bl: bootstrap.bl.clone(),
            snlm0e: bootstrap.snlm0e.clone(),
            fsid: bootstrap.fsid.clone(),
        });
        state.status = SessionStatus::Valid;
        state.refresh_epoch += 1;
        Ok(bootstrap)
    }
}
/// Probe a candidate session without mutating the service's live state.
async fn probe_credentials(
    transport: &ReqwestTransport,
    base_url: Option<&str>,
    credentials: &SessionCredentials,
) -> Result<SessionBootstrap, IdentityError> {
    let base = base_url.unwrap_or("https://gemini.google.com");
    let url = Url::parse(&format!("{base}/app")).map_err(|_| IdentityError::Transport)?;
    let mut headers = HeaderMap::new();
    insert_auth_headers(&mut headers, credentials)?;
    let response = transport
        .execute(TransportRequest {
            method: Method::GET,
            url,
            headers,
            body: None,
            idempotency: Idempotency::SafeToRetry,
        })
        .await
        .map_err(|_| IdentityError::Transport)?;

    if response.status.as_u16() == 301 || response.status.as_u16() == 302 {
        let location = response
            .headers
            .get(http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if location.contains("sorry") {
            return Err(IdentityError::IpFlagged);
        }
    }
    if !response.status.is_success() {
        return Err(IdentityError::NeedsReauth);
    }
    let html = String::from_utf8_lossy(&response.body);
    Ok(SessionBootstrap {
        bl: parser::extract_bl(&html).ok_or(IdentityError::MissingBootstrapField("bl"))?,
        snlm0e: parser::extract_snlm0e(&html)
            .ok_or(IdentityError::MissingBootstrapField("SNlM0e"))?,
        fsid: parser::extract_fsid(&html).ok_or(IdentityError::MissingBootstrapField("f.sid"))?,
    })
}

fn insert_auth_headers(
    headers: &mut HeaderMap,
    credentials: &SessionCredentials,
) -> Result<(), IdentityError> {
    headers.insert(
        http::header::COOKIE,
        format!(
            "__Secure-1PSID={}; __Secure-1PSIDTS={}; SAPISID={}",
            credentials.psid, credentials.psidts, credentials.sapisid
        )
        .parse()
        .map_err(|_| IdentityError::Transport)?,
    );
    headers.insert(
        http::header::AUTHORIZATION,
        parser::build_sapisidhash(&credentials.sapisid)
            .parse()
            .map_err(|_| IdentityError::Transport)?,
    );
    headers.insert(
        http::header::USER_AGENT,
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36"
            .parse()
            .map_err(|_| IdentityError::Transport)?,
    );
    Ok(())
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Parse the required cookies from a full `Cookie:` header string.
///
/// Accepted field names: `__Secure-1PSID`, `__Secure-1PSIDTS`, `SAPISID`.
fn parse_cookie_header(raw: &str) -> Option<SessionCredentials> {
    let mut psid = None;
    let mut psidts = None;
    let mut sapisid = None;

    for pair in raw.split(';') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            let k = k.trim();
            let v = v.trim().to_string();
            match k {
                "__Secure-1PSID" => psid = Some(v),
                "__Secure-1PSIDTS" => psidts = Some(v),
                "SAPISID" => sapisid = Some(v),
                _ => {}
            }
        }
    }

    Some(SessionCredentials {
        psid: psid?,
        psidts: psidts?,
        sapisid: sapisid?,
        imported_at: rfc3339_now(),
    })
}

/// Load credentials from disk. Storage, decryption, parsing, and legacy
/// rewrap failures propagate so startup cannot silently use stale credentials.
fn load_credentials(cfg: &StorageConfig) -> Result<Option<SessionCredentials>, IdentityError> {
    match storage::read_cookies(&cfg.data_dir)? {
        None => Ok(None),
        Some(json) => {
            let creds: SessionCredentials =
                serde_json::from_str(&json).map_err(|_| IdentityError::Storage)?;
            Ok(Some(creds))
        }
    }
}

/// Produce a minimal RFC3339 timestamp using only stdlib.
fn rfc3339_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Format as ISO-8601 UTC: 1970-01-01T00:00:00Z
    let s = secs;
    let (y, mo, d, h, mi, sec) = unix_to_parts(s);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, sec)
}

/// Parse age from a stored RFC3339 string (the simple format we produce).
fn parse_age_from_rfc3339(s: &str) -> Option<std::time::Duration> {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Expect format: YYYY-MM-DDTHH:MM:SSZ
    let s = s.trim_end_matches('Z');
    let parts: Vec<&str> = s.splitn(2, 'T').collect();
    if parts.len() != 2 {
        return None;
    }
    let date: Vec<u32> = parts[0].split('-').filter_map(|x| x.parse().ok()).collect();
    let time: Vec<u32> = parts[1].split(':').filter_map(|x| x.parse().ok()).collect();
    if date.len() < 3 || time.len() < 3 {
        return None;
    }
    let then_secs = parts_to_unix(
        date[0] as u64,
        date[1] as u64,
        date[2] as u64,
        time[0] as u64,
        time[1] as u64,
        time[2] as u64,
    );
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now_secs
        .checked_sub(then_secs)
        .map(std::time::Duration::from_secs)
}

/// Convert unix epoch seconds to (year, month, day, hour, min, sec).
fn unix_to_parts(mut s: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = s % 60;
    s /= 60;
    let min = s % 60;
    s /= 60;
    let hour = s % 24;
    s /= 24;
    // Days since 1970-01-01
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if s < days_in_year {
            break;
        }
        s -= days_in_year;
        year += 1;
    }
    let months = [
        31u64,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for m in &months {
        if s < *m {
            break;
        }
        s -= m;
        month += 1;
    }
    (year, month, s + 1, hour, min, sec)
}

fn parts_to_unix(y: u64, mo: u64, d: u64, h: u64, mi: u64, s: u64) -> u64 {
    let mut days = 0u64;
    for yr in 1970..y {
        days += if is_leap(yr) { 366 } else { 365 };
    }
    let months = [
        31u64,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for days_in_month in months.iter().take((mo - 1) as usize) {
        days += days_in_month;
    }
    days += d - 1;
    days * 86400 + h * 3600 + mi * 60 + s
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}
