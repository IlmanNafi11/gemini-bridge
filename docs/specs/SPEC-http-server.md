# Module Specification: `http-server`

**Module ID:** `http-server`  
**Crate:** `gemini-bridge-http-server` (`crates/http-server`)  
**Phase:** Fase 0 (core routes), expanded through Fase 2  
**Depends On:** `openai-compat`, `config`  
**Parent Spec:** `SPEC.md` §2.1, PRD §4.4, §4.5  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

Run the Axum 0.8 / Hyper 1.x HTTP server on the configured bind address, expose all API routes, enforce authentication, handle SSE streaming back-pressure, apply CORS, and propagate request IDs. Acts as the thin routing/transport layer; business logic belongs to handler crates.

---

## 2. Public API & Interfaces

```rust
pub struct ServerConfig {
    pub bind_addr: std::net::SocketAddr,
    pub api_key: Option<String>,
    pub require_key_for_admin: bool,
    pub cors_enabled: bool,
}

pub async fn start_server(
    config: ServerConfig,
    router: axum::Router,
) -> Result<(), ServerError>;
```

Exposed routes at Fase 0:

| Method | Path | Auth |
|--------|------|------|
| `POST` | `/v1/chat/completions` | Optional |
| `GET`  | `/v1/models`           | Optional |

Later phases add: `/v1/files`, `/v1/images/*`, `/v1/conversations/*`, `/gallery`, `/healthz`, `/readyz`, `/admin/*`, `/metrics`.

---

## 3. Behavior & Invariants

1. **Binding:** Defaults to `127.0.0.1:8090`; non-localhost binding with no API key is rejected at startup, not silently accepted.
2. **Auth Enforcement:** Admin routes always require key; public routes enforce key only if `api_key` is configured.
3. **Request IDs:** Every request receives a UUID v4 `x-request-id` injected before handlers and echoed in response headers.
4. **SSE Back-Pressure:** SSE streams are connected to tokio channels with bounded capacity; slow consumers are gracefully disconnected.
5. **Error Format:** All handler errors must return OpenAI-compatible JSON error envelopes, never bare strings.

---

## 4. Testing Strategy

- Auth enforcement tests for admin routes with and without correct key.
- SSE back-pressure test: slow consumer frame-drops without panic or deadlock.
- Request ID propagation through handler chain.
- Non-localhost binding rejected without `api_key` configured.

---

## 5. Boundaries

- **Always:** Enforce configured auth; propagate request IDs; format errors as JSON.
- **Ask First:** Adding public routes or changing auth enforcement model.
- **Never:** Bind `0.0.0.0` by default; silently ignore TLS misconfiguration for non-local binding.
