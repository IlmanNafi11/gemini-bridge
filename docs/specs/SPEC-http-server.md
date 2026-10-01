# Module Specification: `http-server`

**Module ID:** `http-server`  
**Crate:** `gemini-bridge-http-server` (`crates/http-server`)  
**Phase:** Fase 0 (core routes), expanded through Fase 2  
**Depends On:** `openai-compat`, `config`  
**Parent Spec:** `SPEC.md` §2.1; PRD §2.2 US-1, US-8, §4.4, §4.5  
**Status:** Approved Draft — enriched for P.4

---

## 1. Objective & Responsibility

Run the Axum 0.8 / Hyper 1.x HTTP server on the configured bind address, expose all API routes, enforce bearer token authentication, handle SSE streaming pipelines and back-pressure, apply CORS, and propagate request IDs.

Acts as the thin routing and transport layer: it coordinates handler execution, request validation, and response serialization, delegating domain logic to provider and feature crates.

**In scope:**
- Route registration and dispatch (Fase 0: chat, models; later: files, images, conversations, gallery, health/admin, metrics)
- Bearer token authentication middleware
- Request ID injection (`x-request-id` UUID v4) and response header propagation
- SSE stream orchestration (`axum::response::Sse`, keep-alive, terminal `[DONE]`)
- HTTP error mapping using `openai-compat` envelopes
- Server lifecycle: `build_router`, `start_server`

**Out of scope:**
- OpenAI schema shapes, serialization, model alias parsing (→ `openai-compat`)
- Gemini Web wire protocol and session management (→ `gemini-adapter`, `identity`)
- Token bucket rate limiting, log secret redaction (→ `middleware`)
- Health/readiness probe logic, admin reauth flow (→ `health-admin`)

---

## 2. Public API & Interfaces

### 2.1 Configuration & Types

```rust
use std::net::SocketAddr;
use std::sync::Arc;
use axum::Router;
use thiserror::Error;

use gemini_bridge_llm_service::LlmAdapter;

/// Server configuration at the HTTP layer.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_addr: SocketAddr,
    pub api_key: Option<String>,
    pub require_key_for_admin: bool,
    pub cors_enabled: bool,
}

/// Shared application state threaded through all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub adapter: Arc<dyn LlmAdapter>,
    // Extended in later phases:
    // pub media_store: Option<Arc<dyn MediaStore>>,
    // pub identity_service: Option<Arc<dyn IdentityService>>,
    // pub conversation_store: Option<Arc<dyn ConversationStore>>,
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("bind failed: {0}")]
    Bind(String),
    #[error("serve error: {0}")]
    Serve(String),
}
```

### 2.2 Entrypoints

```rust
/// Build the Axum router with all routes, auth middleware, and request ID layers.
/// Does not bind a socket.
pub fn build_router(config: ServerConfig, state: AppState) -> Router;

/// Bind the configured address and run the Axum server until shutdown signal.
pub async fn start_server(
    config: ServerConfig,
    state: AppState,
) -> Result<(), ServerError>;

/// Return type for [`build_reloadable_gemini_adapter`].
pub type ReloadableGeminiAdapter = (Arc<dyn LlmAdapter>, Arc<dyn PluginReloader>);

/// Construct the production Gemini generation slot and matching built-in reloader.
pub fn build_reloadable_gemini_adapter(
    config: Arc<BridgeConfig>,
    identity: Arc<DefaultIdentityService>,
) -> Result<ReloadableGeminiAdapter, ServerError>;
```

### 2.3 Route Table Across Phases

| Method | Path | Auth | Phase | Handling Crate |
|---|---|---|---|---|
| `POST` | `/v1/chat/completions` | Optional (if `api_key` set) | Fase 0 | `http-server` (handlers::chat) |
| `GET` | `/v1/models` | Optional (if `api_key` set) | Fase 0 | `http-server` (handlers::models) |
| `POST` | `/v1/files` | Enforced | Fase 1 | `upload` / `http-server` |
| `GET` | `/v1/files/{id}` | Optional | Fase 1 | `upload` / `media-store` |
| `POST` | `/v1/images/generations` | Optional | Fase 1 | `image-gen` |
| `GET` | `/v1/images/{id}` | Optional | Fase 1 | `image-gen` / `media-store` |
| `GET` | `/healthz` | Public (no auth) | Fase 1 | `health-admin` |
| `GET` | `/readyz` | Public (no auth) | Fase 1 | `health-admin` |
| `GET` | `/admin/status` | Always requires key | Fase 1 | `health-admin` |
| `POST` | `/admin/reauth` | Always requires key | Fase 1 | `health-admin` |
| `GET` | `/v1/conversations` | Optional | Fase 2 | `conversation-store` |
| `GET` | `/v1/conversations/{id}/messages` | Optional | Fase 2 | `conversation-store` |
| `POST` | `/v1/conversations/{id}/branch` | Optional | Fase 2 | `conversation-store` |
| `GET` | `/gallery` | Optional | Fase 2 | `gallery` |
| `POST` | `/admin/reload-plugin` | Always requires key | Fase 3 | `health-admin` / `plugin-context` |
| `GET` | `/metrics` | Protected / opt-in | Fase 3 | `http-server` (metrics) |

---

## 3. Behavior & Invariants

1. **Middleware Ordering:**
   Every incoming HTTP request traverses layers in this strict order:
   ```
   Request
     │
     ▼
   [SetRequestIdLayer]         ← Injects UUID v4 x-request-id if absent
     │
     ▼
   [PropagateRequestIdLayer]   ← Ensures x-request-id header returned on response
     │
     ▼
   [CORS Layer] (optional)     ← Permissive or configured origin headers
     │
     ▼
   [Auth Middleware]           ← Validates Bearer <api_key> if key is configured
     │
     ▼
   [Route Handler]             ← Dispatches to chat, models, etc.
   ```

2. **Authentication Rules:**
   - Public/API routes (`/v1/*`): Enforced **only if** `api_key` is configured in `ServerConfig` (`Some(...)`). If `api_key` is `None`, requests pass through unauthenticated.
   - Admin routes (`/admin/*`): **Always require** authentication. If `api_key` is `None` but `require_key_for_admin` is `true`, admin routes return `401 Unauthorized` with a message instructing the operator to configure `api_key`.
   - Health probes (`/healthz`, `/readyz`): Always bypass authentication.
   - Auth format: Header `Authorization: Bearer <key>`. Missing or mismatched key returns HTTP 401:
     ```json
     {
       "error": {
         "message": "Invalid API key",
         "type": "authentication_error",
         "code": "invalid_api_key"
       }
     }
     ```

3. **Binding & Network Security:**
   - Default bind host is `127.0.0.1:8090`.
   - Binding `0.0.0.0` or a non-loopback address **without an API key configured** is rejected at startup with `ServerError::Bind`, preventing accidental public exposure without auth.

4. **SSE Streaming Contract:**
   - When `stream: true`, the handler returns `axum::response::Sse` with Content-Type `text/event-stream`.
   - Keep-alive pings are enabled (`KeepAlive::default()`).
   - Every event line is `data: <JSON>\n\n`.
   - The stream terminates with `data: [DONE]\n\n`.
   - Stream back-pressure: Events are driven from an async `LlmEventStream`; slow or stalled consumers do not block server-wide threads.
   - Provider errors during active streaming are emitted as a final data event (`data: {"error": "..."}`) before closing the stream cleanly.

5. **Error Format:**
   All handler errors return HTTP status with `OpenAiErrorResponse` JSON body. Bare text error responses are strictly prohibited.

---

## 4. Acceptance Criteria

### US-1 Traceability (API Serving Path)

| US-1 AC | Covered by |
|---|---|
| `POST /v1/chat/completions` accepts request, supports streaming & non-streaming | `handlers::chat::chat_completions` |
| Non-stream response returns JSON `ChatCompletionResponse` | `handlers::chat::non_stream_response` |
| Stream response returns SSE text/event-stream with `[DONE]` | `handlers::chat::stream_response` |
| Error mapping for 401, 429, 503, 502, 400 | `handlers::chat::map_llm_error` |
| `GET /v1/models` returns virtual model list | `handlers::models::list_models_handler` |

### US-8 Traceability (Operational Routes & Health)

| US-8 AC | Covered by |
|---|---|
| `GET /healthz` → process status, uptime, version | Registered in route table (Fase 1 handler) |
| `GET /readyz` → session status, build label, cookie age | Registered in route table (Fase 1 handler) |
| `POST /admin/reauth` → guided reauthentication | Registered in route table (Fase 1 handler, auth-gated) |
| Structured JSON logging with `x-request-id` | `SetRequestIdLayer` + `PropagateRequestIdLayer` |
| Plugin reload without dropping active streams | Protected `POST /admin/reload-plugin` route + `ReloadableAdapter` generation retention |

The reload route accepts `{"plugin":"<name>"}`. It returns HTTP 200 only after the named built-in replacement is ready and published; initialization failure, unknown plugin, or deadline expiry returns a structured `reload_failed` error without replacing the active generation.

### Acceptance criteria (module-level)

1. `POST /v1/chat/completions` with valid non-stream payload returns HTTP 200 with `ChatCompletionResponse` body.
2. `POST /v1/chat/completions` with `stream: true` returns `Content-Type: text/event-stream`, emits `ChatCompletionChunk` events, and ends with `data: [DONE]`.
3. Unknown model string returns HTTP 400 with `invalid_request_error` JSON envelope.
4. When `api_key` is configured, requests without `Authorization: Bearer <key>` return HTTP 401 with `invalid_api_key`.
5. When `api_key` is `None`, public routes accept unauthenticated requests.
6. Every response carries the `x-request-id` header matching the incoming header or newly generated UUID.
7. `GET /v1/models` returns HTTP 200 with the four virtual models.
8. Server startup fails if bound to `0.0.0.0` without an `api_key` set.

---

## 5. Testing Strategy

- **Handler unit tests (`e2e_chat_test.rs`):**
  - Non-streaming chat request with mock adapter returning text.
  - Streaming chat request with mock adapter yielding chunks; collect SSE stream and verify ordering, chunk shape, and `[DONE]` termination.
  - Model alias parsing error returns 400.
  - Provider errors (Authentication → 401, RateLimited → 429, Unavailable → 503) return corresponding HTTP status codes.
- **Authentication tests:**
  - Request with valid key succeeds (200).
  - Request with missing/wrong key rejected (401).
  - Unconfigured key allows requests on public routes (200).
- **Request ID tests:**
  - Custom `x-request-id` is preserved and echoed on response.
  - Absent `x-request-id` receives a generated UUID v4 on response.
- **Models endpoint test:**
  - `GET /v1/models` returns `object: "list"` with four entries.

---

## 6. Boundaries

- **Always:** Enforce configured auth; propagate `x-request-id`; format all errors as JSON `OpenAiErrorResponse`; enable keep-alive on SSE streams.
- **Ask First:** Adding public routes; altering auth header format; changing default bind address/port.
- **Never:** Bind `0.0.0.0` without authentication; emit un-framed SSE text lines; return bare HTTP error strings; block async worker threads in handlers.
