# Module Specification: `identity`

**Module ID:** `identity`  
**Crate:** `gemini-bridge-identity` (`crates/identity`)  
**Phase:** Fase 0 (bootstrap), expanded in Fase 1 (rotation/recovery)  
**Depends On:** `config`, `transport`  
**Parent Spec:** `SPEC.md` §2.1; PRD §3.1, §4.6, §4.7  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Manage the local Gemini Web session lifecycle: import user-provided session cookies, bootstrap Gemini Web with authenticated requests, parse required bootstrap values (`bl`, `SNlM0e`, `thykhd`, `qKIAYe`, `Ylro7b`, `f.sid` as required by the active protocol), expose readiness/expiry/flag state, and store secrets securely. Fase 1 adds scheduled/on-demand `1PSIDTS` rotation and reports `needs_reauth` when renewal fails.

This crate does not automate Google login or depend on Camofox at runtime. During local development, an operator may use available browser tooling and import cookies through the CLI. Real credentials remain local and must not appear in repository fixtures, logs, or documentation.

---

## 2. Public API & Interfaces

```rust
use std::time::{Duration, SystemTime};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("No session credentials are configured")]
    MissingCredentials,
    #[error("Session is invalid or expired")]
    NeedsReauth,
    #[error("Gemini Web session was flagged by upstream")]
    IpFlagged,
    #[error("Bootstrap response is missing required field: {0}")]
    MissingBootstrapField(&'static str),
    #[error("Credential storage failed")]
    Storage,
    #[error("Transport request failed")]
    Transport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus { Unconfigured, Valid, Stale, NeedsReauth, IpFlagged }

/// Secret fields must redact in Debug and be zeroized when practical.
pub struct SessionCredentials { /* private */ }
pub struct SessionBootstrap { /* private protocol fields */ }

pub struct SessionSnapshot {
    pub status: SessionStatus,
    pub build_label: Option<String>,
    pub cookie_age: Option<Duration>,
    pub checked_at: SystemTime,
}

#[async_trait::async_trait]
pub trait IdentityService: Send + Sync {
    async fn bootstrap(&self) -> Result<SessionBootstrap, IdentityError>;
    async fn refresh_1psidts(&self) -> Result<(), IdentityError>;
    async fn snapshot(&self) -> SessionSnapshot;
    async fn import_credentials(&self, raw_cookie_header: &str) -> Result<(), IdentityError>;
}
```

---

## 3. Behavior & Invariants

1. **Credential Import:** Credentials are accepted only through explicit local CLI/admin flow and validated by an authenticated bootstrap; raw cookie strings are never echoed.
2. **Secure Storage:** Credential files are created with owner-only (`0600`) permissions. When `BRIDGE_SECRET` encryption is enabled, incorrect/missing decryption key fails closed; it must not silently rewrite encrypted data as plaintext.
3. **Bootstrap:** Authenticated `GET /app` extracts current required protocol values; HTML changes produce a typed missing-field error and readiness degradation, not a panic.
4. **Refresh:** Fase 1 rotation is bounded and single-flight so simultaneous requests do not trigger uncontrolled parallel refreshes. Refresh failure transitions to `NeedsReauth` and is visible via readiness/admin status.
5. **Flag Detection:** A redirect to the upstream `sorry/index` path transitions to `IpFlagged`; do not retry blindly.
6. **Secret Handling:** Secret wrappers implement redacted `Debug`; cookies, auth hashes, and tokens are excluded from spans/errors. Local session files and values are not test fixtures.
7. **Development Boundary:** Camofox may be used manually during development only. There is no shipped Camofox integration or runtime process dependency.

---

## 4. Testing Strategy

- Parser unit tests against sanitized `/app` fixtures for valid, missing, changed, and malformed bootstrap fields.
- Cookie-file permission tests and optional encryption round-trip/wrong-key tests in temporary directories.
- Mock transport tests for expired session, rotation success/failure, `sorry/index` redirect, and concurrent refresh single-flight.
- CLI tests for actionable `auth login` and `doctor` results without logging imported values.
- Live session smoke is opt-in and uses operator-provided local credentials only; it is not a deterministic CI test.

---

## 5. Boundaries

- **Always:** Keep credentials local, redact all secret representations, set restrictive file permissions, expose explicit session states, bound refresh attempts.
- **Ask First:** Changing credential format/encryption behavior, accepting credentials through a new public route, or adding browser automation/runtime dependencies.
- **Never:** Commit cookies/tokens, log cookie/header values, run unbounded refresh loops, or claim a valid session without a successful upstream check.
