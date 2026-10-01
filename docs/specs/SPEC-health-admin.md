# Module Specification: `health-admin`

**Module ID:** `health-admin`  
**Crate:** `gemini-bridge-health-admin` (`crates/health-admin`)  
**Phase:** Fase 1 (health/readiness/status/reauth/405 recovery), Fase 3 (reload)  
**Depends On:** `http-server`, `identity`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-8, §4.6  
**Status:** Approved Draft — enriched for P.6  

---

## 1. Objective & Responsibility

The `health-admin` module exposes process health, Gemini session readiness, administrative session control, and (in Fase 3) plugin reload surfaces. It encapsulates all operational and diagnostic endpoint logic, delegating session state to `identity` and HTTP routing to `http-server`.

**In scope:**
- `/healthz` — lightweight process liveness indicator: uptime, version, status.
- `/readyz` — session readiness probe deriving state from `IdentityService::snapshot()`: reflects `SessionStatus` (Valid/Stale/NeedsReauth/IpFlagged), build label (`bl`), and `1PSIDTS` cookie age.
- `/admin/status` — authenticated snapshot of the current session state for operator diagnostics.
- `/admin/reauth` — guided re-authentication POST endpoint that triggers a fresh cookie import and re-bootstrap through `identity`, transitioning session state from `NeedsReauth` to `Valid`.
- Bounded 405 build-label auto-recovery: when `gemini-adapter` detects a 405 response, the `health-admin` layer coordinates a single `identity.bootstrap()` refresh and one upstream retry. No second retry is issued.
- (Fase 3) `/admin/reload-plugin` — hot-swap a named plugin instance without terminating active SSE streams. Implemented via built-in instance replacement wrapped in `Arc<tokio::sync::RwLock<T>>` to ensure active streams hold their older `Arc` securely.

**Out of scope:**
- HTTP routing infrastructure, SSE streaming (→ `http-server`).
- Cookie rotation scheduling, `1PSIDTS` refresh, and `IpFlagged` detection (→ `identity`).
- Media purge administration (→ `media-store`, Task 2.5).
- Rate limiting or request audit logging (→ `middleware`).
- Prometheus metrics exposition (→ Fase 3 `http-server` extension).

---

## 2. Public API & Interfaces

### 2.1 Domain Types

```rust
use serde::Serialize;

/// Response payload for `GET /healthz`.
/// Always returns HTTP 200 as long as the process is alive.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    /// Always "ok" when the process is alive.
    pub status: &'static str,
    /// Seconds since the server process started.
    pub uptime_secs: u64,
    /// Semantic version string of the bridge binary (from `env!("CARGO_PKG_VERSION")`).
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
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HealthAdminError {
    #[error("Reauth failed: {0}")]
    ReauthFailed(String),

    #[error("Reload failed: {0}")]
    ReloadFailed(String),

    #[error("Unauthorized")]
    Unauthorized,
}
```

### 2.2 Service Trait

```rust
#[async_trait::async_trait]
pub trait HealthAdminService: Send + Sync {
    /// Return process liveness status.
    async fn health(&self) -> HealthResponse;

    /// Return session readiness state from `IdentityService::snapshot()`.
    async fn readiness(&self) -> ReadinessResponse;

    /// Return full diagnostic admin status snapshot.
    async fn admin_status(&self) -> AdminStatusResponse;

    /// Attempt guided re-authentication by importing new credentials via identity bootstrap.
    async fn reauth(&self, raw_cookie_header: &str) -> Result<ReauthResponse, HealthAdminError>;

    /// (Fase 3) Request a hot reload of a named plugin via built-in instance replacement.
    /// Returns `Err(ReloadFailed)` if reload exceeds the 2-second deadline or active
    /// streams cannot safely buffer the transition.
    async fn reload_plugin(&self, plugin_name: &str) -> Result<(), HealthAdminError>;
}
```

---

## 3. Behavior & Invariants

1. **`/healthz` Always Succeeds:**
   Returns HTTP 200 with `{"status":"ok","uptime_secs":N,"version":"..."}` as long as the process is alive. Never returns non-2xx for the health check path. Uptime is measured from process start time captured at binary initialization.

2. **`/readyz` Reflects IdentityService State:**
   Calls `IdentityService::snapshot()` to read the current `SessionStatus` without network I/O on the hot path. The HTTP response code follows:
   - `SessionStatus::Valid` → HTTP 200
   - `SessionStatus::Stale`, `NeedsReauth`, `IpFlagged`, `Unconfigured` → HTTP 503

3. **405 Build-Label Auto-Recovery:**
   When `gemini-adapter` returns a 405 error indicating a stale build label:
   1. `health-admin` triggers `identity.bootstrap()` exactly once.
   2. If bootstrap succeeds, retry the original request exactly once with the refreshed build label.
   3. If bootstrap or the second request fails, propagate the error to the original client as a 502 with a structured `OpenAiErrorResponse`.
   4. This entire cycle produces at most two upstream attempts for one client request. Active SSE streams are **not** dropped by 405 recovery; in-flight streaming is isolated from bootstrap.

4. **Admin Routes Require Authentication:**
   All `/admin/*` routes must validate `Authorization: Bearer <api_key>` and return HTTP 401 `{"error":{"message":"Unauthorized","type":"authentication_error","code":"invalid_api_key"}}` for missing or invalid keys. If `api_key` is not configured in `ServerConfig`, admin routes return 401 with instructions to configure a key — they are never unauthenticated.

5. **`/admin/reauth` Guided Workflow:**
   - Accepts a `raw_cookie_header` (the `Cookie: ...` header string from the operator's authenticated browser session) as a JSON or form body parameter.
   - Delegates to `IdentityService::import_credentials(raw_cookie_header)` followed by `IdentityService::bootstrap()`.
   - Reports the resulting `SessionStatus` and `build_label` in the response. Failures are returned as structured errors, not panics.

6. **Credential Values Never Exposed:**
   `/healthz`, `/readyz`, `/admin/status`, and `/admin/reauth` responses must not include raw cookie values, token hashes, or any secret credential fields. Responses reflect state indicators (status, timestamps) only.

7. **Reload Safety (Fase 3):**
   The reloader asynchronously prepares and validates an unpublished built-in generation. Only after preparation succeeds within the two-second deadline does a synchronous atomic commit publish it. Initialization failure or timeout leaves the currently active generation unchanged. Concurrent reload requests are serialized. Active SSE requests retain their original generation and complete successfully.

---

## 4. Acceptance Criteria

### US-8 Traceability (Operational & Session Health)

| US-8 Acceptance Criterion | Covered by |
|---|---|
| `GET /healthz` → process status, uptime, version | `health()` handler; always HTTP 200 with `HealthResponse` |
| `GET /readyz` → session status (`valid`/`stale`/`needs_reauth`), build label, cookie age | `readiness()` handler deriving from `IdentityService::snapshot()` |
| Automatic `1PSIDTS` rotation; failure → `needs_reauth` + guided reauth endpoint | `identity.refresh_1psidts()`; status reflected by `readiness()`; `POST /admin/reauth` |
| Structured JSON log with request-id | Handled by `http-server` request ID layer + `middleware` redaction |
| Plugin reload without process restart or active stream interruption | `reload_plugin()` with buffering (Fase 3) |

---

## 5. Testing Strategy

1. **State Transition Tests for `/readyz`:**
   - Mock `IdentityService` returning each `SessionStatus` value; assert correct HTTP response code and body `session_status` string.
2. **405 Auto-Recovery Tests:**
   - `wiremock` serves 405 on first request and 200 on second; verify client receives successful response; count bootstrap calls (exactly 1); verify no error is returned to client.
   - `wiremock` serves 405 on first and 405 on second; verify client receives HTTP 502 error; no further retries are issued.
3. **Cookie Expiry State Tests:**
   - Mock `IdentityService::snapshot()` returning `NeedsReauth`; assert `readyz` returns 503.
   - Mock `IdentityService::snapshot()` returning `IpFlagged`; assert `readyz` returns 503.
4. **429 Forwarding Test:**
   - `gemini-adapter` returns 429; verify the handler correctly maps it without triggering 405-recovery logic.
5. **Admin Auth Boundary Tests:**
   - Call `GET /admin/status` without API key configured: expect 401 with configuration instruction.
   - Call `GET /admin/status` with wrong key: expect 401.
   - Call `GET /admin/status` with correct key: expect 200.
6. **Uptime Monotonicity Test:**
   - Two sequential `GET /healthz` calls return non-decreasing `uptime_secs`.

7. **Reload Generation Tests:**
   - A protected `POST /admin/reload-plugin` swaps a fully initialized built-in generation and maps failures to `reload_failed`.
   - A stream started before the swap completes with `[DONE]` on the old generation while a later request uses the new generation.
   - The old generation disposer runs only after the final in-flight generation handle is dropped.
   - Reload execution is bounded by a two-second timeout.

---

## 6. Boundaries

- **Always:** Return HTTP 200 from `/healthz` while the process is alive; derive session state from `IdentityService::snapshot()` without direct upstream network calls in the hot path; require API key for all `/admin/*` endpoints; bound 405 recovery to one bootstrap + one retry; exclude raw credential values from all responses.
- **Ask First:** Adding new admin endpoints or changing health or readiness response schemas.
- **Never:** Expose raw cookie values, auth tokens, or SAPISIDHASH in any response; retry 405 more than once per original request; implement unsafe dynamic library loading for reload.
