# Module Specification: `health-admin`

**Module ID:** `health-admin`  
**Crate:** `gemini-bridge-health-admin` (`crates/health-admin`)  
**Phase:** Fase 1 (health/readiness/status/reauth/405 recovery), Fase 3 (reload)  
**Depends On:** `http-server`, `identity`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-8, §4.6  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Expose process health, Gemini session readiness, and administrative control surfaces. Drive bounded 405 build-label auto-recovery and on-demand `1PSIDTS` cookie rotation via `identity`. Accept guided re-authentication and (in Fase 3) in-memory plugin reload without dropping active streaming connections.

---

## 2. Public API & Interfaces

Routes (all admin endpoints require API key):

| Method | Path | Auth |
|--------|------|------|
| `GET` | `/healthz` | — |
| `GET` | `/readyz` | — |
| `GET` | `/admin/status` | Required |
| `POST` | `/admin/reauth` | Required |
| `POST` | `/admin/reload-plugin` | Required (Fase 3) |
| `POST` | `/admin/purge` | Required |

```rust
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub uptime_secs: u64,
    pub version: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadinessResponse {
    pub session_status: String,
    pub build_label: Option<String>,
    pub cookie_age_secs: Option<u64>,
    pub checked_at: i64,
}
```

---

## 3. Behavior & Invariants

1. **`/healthz`:** Always returns 200 as long as the process is alive. Reports uptime and version.
2. **`/readyz`:** Reflects current `IdentityService::snapshot()` session status; returns 200 for `Valid`, 503 for `NeedsReauth`/`IpFlagged`/`Stale`.
3. **405 Recovery:** On 405 detection from `gemini-adapter`, trigger `identity.bootstrap()` once and retry the failing request once. If recovery fails, surface the error to the original client, not the retried path. This recovery path is coordinated without separate admin endpoint calls.
4. **Session Rotation:** Scheduled and on-demand `POST /admin/reauth` drives a guided credential re-import workflow that is actionable without requiring browser process restart.
5. **Reload (Fase 3):** Plugin hot-swap must buffer active SSE responses, teardown old instance via disposer, re-initialize, and verify readiness before resuming. Active connections complete successfully without disconnect. Implementation contract must be finalized in a separate design decision before Task 3.3 is implemented.

---

## 4. Testing Strategy

- State transition tests for `readyz` across session status values.
- Chaos test: wiremock injects 405 mid-request; verify single silent retry succeeds and client receives no error.
- Wiremock injection of expired cookie; verify `readyz` reflects `NeedsReauth` and no retry loop.
- 429 injection test; verify 429 is forwarded accurately.

---

## 5. Boundaries

- **Always:** Never surface session credential values in health/readiness endpoints; bound retries to once per 405; require key for all `/admin/*` routes.
- **Ask First:** Adding new admin endpoints or changing status schemas.
- **Never:** Expose raw cookie/token values; retry indefinitely on 405 or expired auth.
