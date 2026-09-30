# Module Specification: `middleware`

**Module ID:** `middleware`  
**Crate:** `gemini-bridge-middleware` (`crates/middleware`)  
**Phase:** Fase 1 (redaction/rate limiting), Fase 2 (audit log)  
**Depends On:** `plugin-context`  
**Parent Spec:** `SPEC.md` §2.1; PRD §4.1, §4.7  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Implement the typed event bus waterfall middleware: redact secrets from traces/logs, enforce token-bucket rate limits, log request audits, and trace execution spans without modifying immutable request payloads.

---

## 2. Public API & Interfaces

```rust
use async_trait::async_trait;

pub struct RedactionFilter;
impl RedactionFilter {
    pub fn redact_str(input: &str) -> String;
}

pub struct TokenBucketLimiter {
    pub capacity: u32,
    pub refill_rate_per_sec: u32,
}

impl TokenBucketLimiter {
    pub fn try_acquire(&self, client_id: &str) -> bool;
}

pub struct AuditLogEntry {
    pub request_id: String,
    pub timestamp: i64,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: u64,
    pub model: Option<String>,
}
```

---

## 3. Behavior & Invariants

1. **Secret Redaction:** Redacts Google session cookies (`__Secure-1PSID*`, `SAPISID`), Bearer tokens, and upstream auth headers using regex/pattern scanning across all log outputs.
2. **Immutable Requests:** Middleware operates via waterfall event filters; it does not mutate frozen request bodies.
3. **Rate Limiting:** Responds with `429 Too Many Requests` and `Retry-After` header when token bucket is exhausted.
4. **Audit Trail:** Append-only structured audit logs capturing operational metrics without recording request body contents.

---

## 4. Testing Strategy

- **Redaction Unit Tests:** Ensure all variations of cookies and auth tokens are replaced with `***REDACTED***`.
- **Rate Limiter Concurrency Tests:** Verify token bucket behavior under burst traffic.
- **Audit Verification:** Confirm that request metadata is logged correctly without leaking secrets.

---

## 5. Boundaries

- **Always:** Redact secrets before any log emission; return standard 429 error envelopes.
- **Ask First:** Adding new rate-limiting tiers or changing audit log format.
- **Never:** Log plain credentials or modify request payloads in-flight.
