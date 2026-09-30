# Module Specification: `middleware`

**Module ID:** `middleware`  
**Crate:** `gemini-bridge-middleware` (`crates/middleware`)  
**Phase:** Fase 1 (request IDs/redaction/rate limiting), Fase 2 (audit), Fase 3 (metrics)  
**Depends On:** `plugin-context`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-8 (structured logs/request ID), §4.1, §4.7  
**Status:** Approved Draft — enriched for P.4

---

## 1. Objective & Responsibility

Implement framework-neutral operational policy applied to inbound requests and emitted diagnostics: secret redaction, token-bucket rate limiting, structured audit records, and request tracing. Integrate through the `plugin-context` typed event bus without owning Axum routes or mutating normalized LLM request payloads.

The module's primary security invariant is that secrets are redacted **before** any log or trace sink receives them. Request bodies remain immutable after normalization; middleware operates on request metadata and owned diagnostic text only.

**In scope:**
- Redacting Gemini session cookies, bearer tokens, and known authentication headers
- Per-client token-bucket admission decisions
- Structured audit entries containing metadata, never request/response bodies
- Request lifecycle event types and waterfall/serial handler registration
- Standard rate-limit decision carrying HTTP 429 and `Retry-After` information

**Out of scope:**
- Generating or propagating HTTP request IDs (→ `http-server`); this module consumes the established ID
- Axum layer ordering and route auth enforcement (→ `http-server`)
- Credential storage and refresh (→ `identity`)
- Metrics endpoint and Prometheus encoding (→ Fase 3 `http-server` integration)

---

## 2. Public API & Interfaces

### 2.1 Request Metadata

```rust
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Immutable metadata derived from the incoming HTTP request.
/// No request body, cookie value, authorization value, or payload is retained.
#[derive(Debug, Clone)]
pub struct RequestMetadata {
    pub request_id: Arc<str>,
    pub method: Arc<str>,
    pub path: Arc<str>,
    pub client_id: Arc<str>,
    pub started_at: SystemTime,
    pub model: Option<Arc<str>>,
}

/// Result of middleware admission before handler execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    Allow,
    Reject {
        status: u16,              // always 429 for rate limiting
        retry_after: Duration,
        error_type: &'static str, // "rate_limit_exceeded"
        error_code: &'static str, // "rate_limit_exceeded"
    },
}
```

### 2.2 Redaction

```rust
#[derive(Debug, Clone, Default)]
pub struct RedactionFilter;

impl RedactionFilter {
    /// Returns owned text with every recognized secret value replaced by
    /// the literal `***REDACTED***`.
    pub fn redact_str(input: &str) -> String;

    /// Redacts string-valued fields recursively before structured output.
    pub fn redact_json(value: &serde_json::Value) -> serde_json::Value;
}
```

Recognized patterns include:
- Cookie names/values: `__Secure-1PSID`, `__Secure-1PSIDTS`, `__Secure-1PSIDCC`, `SAPISID`
- HTTP authorization: `Authorization: Bearer <token>` and `Proxy-Authorization`
- `SAPISIDHASH` header values
- Config-style secret fields: `api_key`, `token`, `password`, `secret`, `cookie`

### 2.3 Rate Limiting

```rust
#[derive(Debug, Clone, Copy)]
pub struct TokenBucketConfig {
    pub capacity: u32,
    pub refill_tokens: u32,
    pub refill_interval: Duration,
}

pub struct TokenBucketLimiter { /* internal concurrent buckets by client_id */ }

impl TokenBucketLimiter {
    pub fn new(config: TokenBucketConfig) -> Self;
    pub fn try_acquire(&self, client_id: &str, now: SystemTime) -> AdmissionDecision;
    pub fn remove_idle(&self, older_than: SystemTime);
}
```

### 2.4 Audit

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditLogEntry {
    pub request_id: String,
    pub timestamp: i64,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: u64,
    pub model: Option<String>,
}

pub trait AuditSink: Send + Sync {
    fn record(&self, entry: AuditLogEntry);
}
```

`AuditLogEntry` deliberately excludes headers, query-string values, request body, response body, cookie values, and IP address.

### 2.5 Plugin Event Contracts

```rust
/// Dispatched as a bail event before the route handler.
pub struct BeforeRequest {
    pub metadata: RequestMetadata,
}

/// Dispatched serially after a response is available.
pub struct AfterResponse {
    pub metadata: RequestMetadata,
    pub status: u16,
    pub duration: Duration,
}

/// Passed through a waterfall before any diagnostic record is emitted.
pub struct BeforeLog {
    pub request_id: Option<Arc<str>>,
    pub level: tracing::Level,
    pub fields: serde_json::Value,
}
```

Dispatch semantics:
- `BeforeRequest`: **Bail** — handlers execute in registration order and stop on the first `AdmissionDecision::Reject`.
- `AfterResponse`: **Serial** — audit handlers observe immutable metadata in registration order.
- `BeforeLog`: **Waterfall** — handlers own and transform the diagnostic record; redaction is registered first and returns a sanitized record.

---

## 3. Behavior & Invariants

### 3.1 HTTP Integration Order

The `http-server` applies cross-cutting concerns in this order:

```text
Incoming request
  1. Set / preserve x-request-id        (http-server)
  2. Propagate x-request-id to response (http-server)
  3. CORS policy                        (http-server, if enabled)
  4. Bearer authentication              (http-server)
  5. Rate-limit admission               (middleware: BeforeRequest/Bail)
  6. Handler / SSE stream start         (feature crate)
  7. Audit outcome                      (middleware: AfterResponse/Serial)
  8. Redact diagnostic fields           (middleware: BeforeLog/Waterfall)
  9. Emit structured JSON log           (tracing sink)
```

Authentication runs before rate limiting so unauthenticated requests cannot consume authenticated client buckets. Audit observes both successful and rejected responses. Redaction is the final transformation before every sink, and no bypass logging path is permitted.

### 3.2 Request ID Contract

- `http-server` owns creation/preservation of UUID v4 `x-request-id`.
- `middleware` receives it as `RequestMetadata.request_id` and copies it into every audit/log record.
- The request ID is opaque: middleware never rewrites or regenerates it.
- Missing request ID at the middleware boundary is an integration error; callers must establish one before dispatching `BeforeRequest`.

### 3.3 Secret Redaction Contract

1. Redaction occurs before serialization/emission to any log, tracing subscriber, audit diagnostic, or error diagnostic.
2. The replacement is exactly `***REDACTED***`; secret length or prefix is not preserved.
3. Matching is case-insensitive for header and structured field names.
4. `redact_json` recursively processes objects and arrays without changing non-secret scalar values or field names.
5. Redaction is idempotent: applying it twice produces the same output as once.
6. Request and response bodies are excluded from audit logs; redaction is defense-in-depth, not permission to log bodies.

### 3.4 Rate-Limit Contract

1. Each `client_id` has an independent token bucket.
2. Successful acquisition consumes exactly one token.
3. Empty buckets return `AdmissionDecision::Reject` with status 429 and a positive `Retry-After` duration.
4. Refill is monotonic and capped at configured capacity; elapsed time must never overfill the bucket.
5. Concurrent attempts cannot admit more requests than available tokens.
6. The HTTP integration maps rejection to `OpenAiErrorResponse` with `type` and `code` set to `rate_limit_exceeded` and includes integer `Retry-After` seconds.

### 3.5 Audit Contract

- Audit records are structured JSON containing only: request ID, timestamp, method, normalized path template, response status, duration, optional model alias.
- Query strings are not included in `path`.
- Request/response bodies and authentication headers are never recorded.
- Audit sink failure must not panic or replace a successful API response; failure is reported through an already-redacted internal diagnostic.

### 3.6 Immutability Contract

Normalized `LlmRequest` values are shared as immutable `Arc<LlmRequest>` instances. Middleware receives metadata projections and cannot mutate model input, messages, tools, or provider metadata. Waterfall mutation is restricted to owned `BeforeLog` diagnostic values.

---

## 4. Acceptance Criteria

### US-8 Traceability (Structured Logging)

| US-8 AC | Covered by |
|---|---|
| Structured JSON logs | `AuditLogEntry` + tracing JSON sink integration |
| Every log has request ID | `RequestMetadata.request_id` copied to audit and `BeforeLog` |
| Session cookies/tokens are not leaked | `RedactionFilter` before sink + excluded body/header fields |

Remaining US-8 criteria belong to `health-admin`, `identity`, and Fase 3 reload contracts; this module neither evaluates session readiness nor reloads plugins.

### Acceptance criteria (module-level)

1. Every request that reaches middleware has a non-empty `request_id`; the same ID appears in `AuditLogEntry` and response headers.
2. `RedactionFilter` replaces every recognized cookie/header/config secret pattern with `***REDACTED***`, including mixed-case field names and nested JSON values.
3. Redaction preserves non-secret text and is idempotent.
4. A token bucket with capacity N permits exactly N immediate acquisitions and rejects the next with HTTP 429 mapping and positive `Retry-After`.
5. Concurrent acquisition cannot oversubscribe the bucket; refill never exceeds capacity.
6. Audit output includes method/path/status/duration/model but contains no header values, query string, request body, or response body.
7. Middleware receives immutable request metadata; no handler can modify the frozen `LlmRequest` through this API.

---

## 5. Testing Strategy

- **Redaction unit tests:**
  - Cookie string variants for `__Secure-1PSID*` and `SAPISID`.
  - `Authorization: Bearer`, `Proxy-Authorization`, and `SAPISIDHASH`.
  - Nested structured fields (`api_key`, `token`, `password`, `secret`, `cookie`) with mixed casing.
  - Idempotence and preservation of ordinary text.
  - Captured tracing output assertion: known sentinel secrets are absent from final emitted JSON.
- **Rate limiter tests:**
  - Capacity boundary N/N+1.
  - Deterministic refill using supplied `SystemTime`, including exact interval edge.
  - Independent client buckets.
  - Concurrent burst (more contenders than capacity) admits exactly capacity requests.
  - Idle bucket cleanup does not remove recently used buckets.
- **Audit tests:**
  - Complete metadata serializes as JSON.
  - Request ID propagation.
  - Assert prohibited values (headers, body, query string, sentinel secret) are absent.
- **Integration tests (`http-server`):**
  - Authenticated request flows through rate limit, handler, audit, redaction in declared order.
  - Rejected request returns OpenAI 429 envelope and `Retry-After` header.
  - Frozen `Arc<LlmRequest>` remains unchanged before/after middleware execution.

---

## 6. Boundaries

- **Always:** Redact before every diagnostic sink; carry the established request ID; use immutable metadata projections; return standard 429 error envelope with `Retry-After`; avoid logging bodies and headers.
- **Ask First:** Adding rate-limit tiers; changing audit field schema; retaining client identifiers; introducing persistent audit storage.
- **Never:** Log plain credentials; mutate normalized request payloads; use wall-clock sleeps in rate-limit tests; let audit failures alter successful API responses; expose raw query strings in audit records.
