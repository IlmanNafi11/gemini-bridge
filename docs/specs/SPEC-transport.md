# Module Specification: `transport`

**Module ID:** `transport`  
**Crate:** `gemini-bridge-transport` (`crates/transport`)  
**Phase:** Fase 0  
**Depends On:** `config`  
**Parent Spec:** `SPEC.md` §2.1  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

The `transport` module provides all outbound HTTP network I/O for Gemini Web, content upload, and media retrieval. It encapsulates browser-style HTTP header presets, proxy routing, timeout policies, and transport-level retry while preventing provider protocol details from leaking into the network layer. It uses the standard `reqwest`/`rustls` TLS stack and does not impersonate a browser ClientHello or JA3 fingerprint.

---

## 2. Public API & Interfaces

```rust
use bytes::Bytes;
use reqwest::{Method, StatusCode};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsProfile {
    Chrome,
    Firefox,
    Safari,
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("Request timed out after {0:?}")]
    Timeout(Duration),
    #[error("TLS handshake failed")]
    TlsFailure,
    #[error("Proxy connection failed")]
    ProxyFailure,
    #[error("Network error: {0}")]
    Network(String),
}

pub struct TransportRequest {
    pub method: Method,
    pub url: url::Url,
    pub headers: http::HeaderMap,
    pub body: Option<Bytes>,
    pub idempotency: Idempotency,
}

pub struct TransportResponse {
    pub status: StatusCode,
    pub headers: http::HeaderMap,
    pub body: Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idempotency {
    SafeToRetry,
    NeverRetry,
}

#[async_trait::async_trait]
pub trait TransportService: Send + Sync {
    async fn execute(&self, request: TransportRequest) -> Result<TransportResponse, TransportError>;
}
```

---

## 3. Behavior & Invariants

1. **Transport Profiles:** The selected browser-named profile applies a coherent HTTP header preset; the default is Chrome. Profiles do not select cipher suites, extension ordering, JA3, or any other TLS ClientHello fingerprint.
2. **Proxy Support:** HTTP and SOCKS5 proxy URLs are supported when configured; absent proxy connects directly.
3. **Retry Separation:** Only transport errors explicitly marked transient may be retried. Provider HTTP status retries (`405`, `429`, etc.) remain outside this crate except when policy delegates idempotent status retry explicitly.
4. **Idempotency:** Requests marked `NeverRetry` must never be repeated automatically.
5. **Bounded Time:** Connect, request, and idle streaming timeouts are configured and finite.
6. **No Secret Logging:** Cookie, Authorization, and upload token headers are redacted before traces are emitted.
7. **Live Compatibility Boundary:** Deterministic tests establish profile headers and transport behavior only. Current Gemini Web compatibility requires an opt-in credentialed `doctor` or live request and remains external evidence; it is not required to describe the implemented profile contract truthfully.

---

## 4. Testing Strategy

- Mock HTTP server tests for headers, body preservation, timeout, retry count, and non-idempotent no-retry.
- Proxy integration test proving requests route through configured HTTP/SOCKS5 proxy.
- Header-profile tests assert exact preset headers and confirm that no unsupported TLS-fingerprint behavior is claimed; an opt-in Gemini Web `doctor` probe records separate live compatibility evidence.
- Log capture test asserting that secret headers never appear.

---

## 5. Boundaries

- **Always:** Preserve request bytes exactly; bound retries and timeouts; redact sensitive headers.
- **Ask First:** Adding native system dependencies or changing the default header profile.
- **Never:** Claim JA3/ClientHello impersonation, retry `NeverRetry` requests, disable certificate verification, or embed provider-specific `f.req` logic.
