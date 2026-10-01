# Todo — Gemini Bridge

**Status:** Approved — work in progress.  
**Plan:** `tasks/plan.md` · **Specification:** `SPEC.md`  
**Scope:** Full Fase 0–3 roadmap.  

> Paths below are planned paths, because the repository currently has no application source. Each implementation task must also pass its focused tests and repository quality gates before it is marked complete.

## Preparation — Spec traceability and module contracts

- [x] **P.1: Reconcile root SPEC module count and dependency labels**
  - Acceptance: Root spec's stated module count matches its 18 listed capability rows; graph/module names and phase labels are consistent; any remaining architecture choice is recorded, not silently assumed.
  - Verify: Manually compare `SPEC.md` capability table, graph, project structure, and PRD roadmap.
  - Files: `SPEC.md`

- [x] **P.2: Specify foundation modules** (`plugin-context`, `config`, `transport`)
  - Acceptance: Each module has objective, contract/dependencies, behavior, acceptance criteria, tests, and boundaries consistent with `SPEC.md`.
  - Verify: Check all three specs against the capability map and root six-area requirements.
  - Files: `docs/specs/SPEC-plugin-context.md`, `docs/specs/SPEC-config.md`, `docs/specs/SPEC-transport.md`
  - Depends on: P.1

- [x] **P.3: Specify upstream chat foundation** (`identity`, `gemini-adapter`, `llm-service`)
  - Acceptance: Specs define session/security lifecycle, adapter wire boundaries and parser behavior, and provider-neutral service contract without circular dependencies.
  - Verify: Review contracts against PRD §3.1 and SPEC dependency edges.
  - Files: `docs/specs/SPEC-identity.md`, `docs/specs/SPEC-gemini-adapter.md`, `docs/specs/SPEC-llm-service.md`
  - Depends on: P.2

- [x] **P.4: Specify API serving path** (`openai-compat`, `http-server`, `middleware`)
  - Acceptance: Specs define request/response and error mapping, routes/auth/SSE boundary, middleware order, and request ID/redaction contract.
  - Verify: Trace US-1 and US-8 API criteria to these three module specs.
  - Files: `docs/specs/SPEC-openai-compat.md`, `docs/specs/SPEC-http-server.md`, `docs/specs/SPEC-middleware.md`
  - Depends on: P.3

- [x] **P.5: Specify file and image path** (`media-store`, `upload`, `image-gen`)
  - Acceptance: Specs define content addressing, upload/reference security, image extraction/cache/response contract and test strategy.
  - Verify: Trace US-2 and US-3 acceptance criteria to all three specs.
  - Files: `docs/specs/SPEC-media-store.md`, `docs/specs/SPEC-upload.md`, `docs/specs/SPEC-image-gen.md`
  - Depends on: P.4

- [x] **P.6: Specify continuity and operational path** (`conversation-store`, `health-admin`, `gallery`)
  - Acceptance: Specs define durable conversation/branch behavior, health/admin/session states, and gallery operations against their upstream module contracts.
  - Verify: Trace US-4, US-6, and US-8 acceptance criteria to these specs.
  - Files: `docs/specs/SPEC-conversation-store.md`, `docs/specs/SPEC-health-admin.md`, `docs/specs/SPEC-gallery.md`
  - Depends on: P.5

- [x] **P.7: Specify advanced adapter capabilities** (`tool-calling`, `code-exec-surface`, `video-adapter`)
  - Acceptance: Specs define tool parsing/fallback, optional metadata, and off-by-default video/501 behavior with explicit phase boundaries.
  - Verify: Trace US-5, US-7, and US-9 acceptance criteria to these specs; ensure no audio/TTS scope is introduced.
  - Files: `docs/specs/SPEC-tool-calling.md`, `docs/specs/SPEC-code-exec-surface.md`, `docs/specs/SPEC-video-adapter.md`
  - Depends on: P.6

### Checkpoint P: Contract review
- [ ] All 18 capability IDs have a module spec with dependency interfaces and verifiable acceptance criteria.
- [ ] Human review approves module specs and any blocking technical decisions before implementation.

## Fase 0 — Bootstrap: non-streaming chat

- [x] **Task 0.1: Establish Cargo workspace and plugin context**
  - Acceptance: Edition-2024 workspace and static registry build; typed service provide/inject and event dispatch modes have deterministic behavior and lifecycle disposers.
  - Verify: `cargo test -p gemini-bridge-plugin-context`; workspace clippy.
  - Files: `Cargo.toml`, `crates/plugin-context/Cargo.toml`, `crates/plugin-context/src/lib.rs`, `crates/plugin-context/src/event_bus.rs`, `crates/plugin-context/tests/di_test.rs`
  - Depends on: P.2

- [x] **Task 0.2: Implement TOML configuration and overrides**
  - Acceptance: Bind/config/storage/proxy/auth settings load from TOML; environment overrides take precedence; documented defaults and profile composition work.
  - Verify: `cargo test -p gemini-bridge-config` with precedence and invalid-config boundary cases.
  - Files: `crates/config/Cargo.toml`, `crates/config/src/lib.rs`, `crates/config/src/model.rs`, `bridge.example.toml`
  - Depends on: P.2

- [x] **Task 0.3: Implement outbound transport and TLS profile**
  - Acceptance: Configured HTTP/SOCKS5 proxy and selected TLS fingerprint profile are applied by outbound client; implementation choice is validated against an actual Gemini Web request before closing task.
  - Verify: `cargo test -p gemini-bridge-transport`; mocked proxy/client integration plus live `doctor` probe with local credentials.
  - Files: `crates/transport/Cargo.toml`, `crates/transport/src/lib.rs`, `crates/transport/src/client.rs`, `crates/transport/src/tls.rs`
  - Depends on: Task 0.2

- [x] **Task 0.4: Implement identity bootstrap and local auth commands**
  - Acceptance: `/app` bootstrap extracts required session tokens; cookie import and validation work; persisted credentials use 0600 permissions and optional configured encryption; secret values are not logged. `auth login` and `doctor` provide actionable outcomes.
  - Verify: `cargo test -p gemini-bridge-identity`; CLI tests with fixtures; live `doctor` is opt-in and uses credentials outside git.
  - Files: `crates/identity/Cargo.toml`, `crates/identity/src/lib.rs`, `crates/identity/src/session.rs`, `src/main.rs`, `src/cli.rs`
  - Depends on: Tasks 0.2, 0.3

- [x] **Task 0.5: Implement provider-neutral LLM contract and Gemini non-stream adapter**
  - Acceptance: `llm-service` can carry normalized requests/results without Gemini-specific fields; Gemini adapter builds configured `f.req` and parses recorded response fixtures using schema map; parser failures are explicit and actionable.
  - Verify: `cargo test -p gemini-bridge-adapter-gemini`; `cargo insta test` against checked-in sanitized fixtures.
  - Files: `crates/llm-service/src/lib.rs`, `crates/gemini-adapter/Cargo.toml`, `crates/gemini-adapter/src/lib.rs`, `crates/gemini-adapter/src/f_req.rs`, `crates/gemini-adapter/src/parser.rs`
  - Depends on: Tasks 0.1, 0.4

- [x] **Task 0.6: Add externalized upstream schema and parser self-check**
  - Acceptance: Positional/schema indices live in `schema/gemini-web.toml`; a representative response passes self-check; unknown/malformed schema moves service to specified degraded/error state rather than panic/500.
  - Verify: Adapter tests cover valid and invalid schema fixtures.
  - Files: `schema/gemini-web.toml`, `crates/gemini-adapter/src/schema.rs`, `crates/gemini-adapter/src/self_check.rs`
  - Depends on: Task 0.5

- [x] **Task 0.7: Deliver OpenAI-compatible non-stream chat endpoint**
  - Acceptance: `POST /v1/chat/completions` supports required request fields/model aliases and returns OpenAI-shaped completion, usage and finish reason; `GET /v1/models` lists virtual models; configured bind/auth policy is enforced.
  - Verify: `cargo test -p gemini-bridge-http-server`; `cargo test --test e2e_chat_test` with mock upstream and an opt-in live smoke.
  - Files: `crates/openai-compat/src/lib.rs`, `crates/openai-compat/src/models.rs`, `crates/http-server/src/lib.rs`, `crates/http-server/src/handlers/chat.rs`, `tests/e2e_chat_test.rs`
  - Depends on: Tasks 0.2, 0.5, 0.6

### Checkpoint 0 — Bootstrap gate
- [ ] Mocked end-to-end non-streaming OpenAI chat request passes through local HTTP server and Gemini adapter.
- [ ] Live smoke succeeds when local user-provided credentials and upstream availability permit.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and workspace tests pass.
- [ ] CLI `doctor` reports connectivity/session state and handles invalid/expired credentials without crash loops.

## Fase 1 — MVP: streaming, images, files, resilience

- [x] **Task 1.1: Add streaming SSE chat path**
  - Acceptance: `stream=true` produces OpenAI SSE delta chunks and `[DONE]`; newline-framed Gemini snapshots and prefix changes are parsed; non-prefix rewrites follow the defined reset behavior.
  - Verify: Adapter unit/property boundary tests and `cargo test --test e2e_chat_test` asserting chunks, ordering, and terminal event.
  - Files: `crates/gemini-adapter/src/stream.rs`, `crates/gemini-adapter/src/prefix_diff.rs`, `crates/http-server/src/handlers/chat_stream.rs`, `tests/e2e_chat_test.rs`
  - Depends on: Task 0.7

- [x] **Task 1.2: Implement content-addressed media store**
  - Acceptance: Content has deterministic SHA-256 identity; duplicate bytes resolve to same stored object; metadata supports retrieval and TTL/purge without removing newer files.
  - Verify: `cargo test -p gemini-bridge-media-store`, including deduplication and expiry boundary tests.
  - Files: `crates/media-store/Cargo.toml`, `crates/media-store/src/lib.rs`, `crates/media-store/src/store.rs`, `crates/media-store/src/cleanup.rs`
  - Depends on: Task 0.2

- [x] **Task 1.3: Deliver upload/reference-file endpoints**
  - Acceptance: `POST /v1/files` validates multipart size/type, stores bytes, uploads through resumable push flow and returns an ID; `GET /v1/files/{id}` retrieves stored file; same-content references reuse fileRef. URL references reject forbidden addresses/schemes with DNS pinning.
  - Verify: `cargo test -p gemini-bridge-upload`; mocked resumable upload; SSRF tests cover IPv4/IPv6 private, loopback, link-local, DNS changes, and redirects.
  - Files: `crates/upload/src/lib.rs`, `crates/upload/src/push_client.rs`, `crates/upload/src/ssrf.rs`, `crates/http-server/src/handlers/files.rs`, `tests/e2e_files_test.rs`
  - Depends on: Tasks 0.3, 0.4, 1.2

- [x] **Task 1.4: Deliver image-generation and retrieval path**
  - Acceptance: `POST /v1/images/generations` accepts required inputs and optional references; extracts generated image URLs; caches image bytes and metadata; returns proxy URL or b64; `GET /v1/images/{id}` returns valid cached image.
  - Verify: `cargo test -p gemini-bridge-image-gen`; live acceptance: 5 distinct prompts and 2 with references produce decodable non-empty images.
  - Files: `crates/image-gen/src/lib.rs`, `crates/image-gen/src/extractor.rs`, `crates/http-server/src/handlers/images.rs`, `crates/media-store/src/metadata.rs`, `tests/e2e_image_test.rs`
  - Depends on: Tasks 0.7, 1.2, 1.3

- [x] **Task 1.5: Add health, readiness, admin status and reauth surfaces**
  - Acceptance: `/healthz` reports process/uptime/version; `/readyz` reports session/build-label/cookie-age states; `/admin/status` is protected; guided `/admin/reauth` moves between needs-reauth and valid states.
  - Verify: `cargo test -p gemini-bridge-health-admin` (5 tests), `cargo test --test health_admin_test` (15 tests), `cargo test --workspace` (175 passed), workspace clippy and fmt checks pass.
  - Files: `crates/health-admin/src/lib.rs`, `crates/health-admin/src/readiness.rs`, `crates/http-server/src/handlers/health.rs`, `crates/http-server/src/handlers/admin.rs`, `tests/health_admin_test.rs`
  - Depends on: Tasks 0.4, 0.7

- [x] **Task 1.6: Add cookie rotation and bounded 405 recovery**
  - Acceptance: Stale `1PSIDTS` is refreshed; 405 triggers one bootstrap refresh and one retry; failed rotation sets `needs_reauth`; no retry loop or active stream drop.
  - Verify: `cargo test -p gemini-bridge-identity`; `cargo test --test resilience_drills_test` for success, retry failure, 429, and expiry.
  - Files: `crates/identity/src/rotation.rs`, `crates/gemini-adapter/src/lib.rs`, `crates/identity/src/session.rs`, `tests/resilience_drills_test.rs`
  - Depends on: Tasks 0.4, 1.1, 1.5

- [x] **Task 1.7: Integrate redaction, request IDs, audit and rate limiting**
  - Acceptance: Request IDs propagate through logs/responses; known secret patterns are redacted; rate limiting returns mapped 429; middleware does not mutate frozen request payloads.
  - Verify: `cargo test -p gemini-bridge-middleware`; capture emitted logs and assert secrets absent; verify limit boundary.
  - Files: `crates/middleware/src/lib.rs`, `crates/middleware/src/redact.rs`, `crates/middleware/src/rate_limit.rs`, `crates/middleware/src/audit.rs`, `crates/http-server/src/middleware.rs`
  - Depends on: Tasks 0.1, 0.7

### Checkpoint 1 — MVP gate
- [ ] Chat non-stream/stream, image generation, files, health/readiness/admin acceptance paths pass.
- [ ] E1 acceptance threshold ≥95%; file/image output validity criteria pass.
- [ ] E2 405, 429 and cookie expiry scenarios handled as specified; 405 retry is bounded.
- [ ] E3 latency and memory KPI measurements recorded; seven-day stability run achieves ≥99% success without manual restart.
- [ ] Full formatting, clippy, unit, integration quality gates pass.

## Fase 2 — v1.1: conversations, tools, gallery

- [x] **Task 2.1: Persist conversation and message state**
  - Acceptance: SQLite stores local conversation/message records and upstream identifiers transactionally; retrieval order and migrations are deterministic; stored state can reconstruct history.
  - Verify: `cargo test -p gemini-bridge-conversation-store` (5 tests), `cargo test --workspace` (200 passed), workspace clippy and fmt checks pass.
  - Files: `crates/conversation-store/Cargo.toml`, `crates/conversation-store/src/lib.rs`, `crates/conversation-store/src/db.rs`, `crates/conversation-store/src/migrations.rs`, `crates/conversation-store/src/models.rs`, `crates/conversation-store/src/error.rs`, `crates/conversation-store/tests/store_test.rs`
  - Depends on: Tasks 0.2, 0.7

- [ ] **Task 2.2: Add continuity, history, branch and regenerate flows**
  - Acceptance: Chat with `conversation_id` resumes upstream IDs; history/list endpoints reflect persisted state; branch creates a separate path from selected message; regenerate uses selected prior state; rejected upstream ID replays history and sets `x-gemini-bridge-continuity: degraded`.
  - Verify: `cargo test --test e2e_conversations_test` covers multi-turn, branch isolation, regeneration and degraded fallback.
  - Files: `crates/conversation-store/src/branch.rs`, `crates/http-server/src/handlers/conversations.rs`, `crates/openai-compat/src/chat_models.rs`, `tests/e2e_conversations_test.rs`
  - Depends on: Tasks 1.1, 2.1

- [ ] **Task 2.3: Add tool-call emulation and result continuation**
  - Acceptance: `tools[]` and `tool_choice` map to prompt schema; valid model output becomes structured tool calls; tool results continue as role `tool`; malformed output falls back to text with structured warning; parse rate ≥95% on 50 cases.
  - Verify: `cargo test -p gemini-bridge-tool-calling`; deterministic 50-case parser suite and one full chat round-trip.
  - Files: `crates/tool-calling/src/lib.rs`, `crates/tool-calling/src/injector.rs`, `crates/tool-calling/src/parser.rs`, `crates/openai-compat/src/tools.rs`, `tests/tool_calling_test.rs`
  - Depends on: Tasks 0.7, 1.1

- [ ] **Task 2.4: Deliver gallery JSON and embedded HTML path**
  - Acceptance: `/gallery` JSON and `?format=html` show cached items; date/model/prompt filters work; delete/download actions update/use media store; HTML has no external runtime asset dependency.
  - Verify: `cargo test -p gemini-bridge-gallery`; run service and visually check rendered gallery plus filtering/deletion.
  - Files: `crates/gallery/src/lib.rs`, `crates/gallery/src/handlers.rs`, `crates/gallery/static/gallery.html`, `crates/http-server/src/handlers/gallery.rs`, `tests/gallery_test.rs`
  - Depends on: Tasks 0.7, 1.2, 1.4

- [ ] **Task 2.5: Add media purge and TTL administration**
  - Acceptance: Configured TTL cleanup and protected purge endpoint remove expired or requested cached media and metadata consistently.
  - Verify: `cargo test -p gemini-bridge-media-store`; route test verifies authorization and retained non-expired media.
  - Files: `crates/media-store/src/cleanup.rs`, `crates/health-admin/src/purge.rs`, `crates/http-server/src/handlers/admin.rs`, `tests/media_purge_test.rs`
  - Depends on: Tasks 1.2, 1.5

### Checkpoint 2 — v1.1 gate
- [ ] Conversation continuity, list/history, branch, regenerate, and degraded replay verified end-to-end.
- [ ] Tool call parse rate ≥95% over 50 cases and tool-result continuation passes.
- [ ] Gallery filtering, retrieval and deletion pass manual/runtime verification.
- [ ] E1 ≥95%, E3 stated latency/memory KPIs verified, and quality gates pass.

## Fase 3 — v2.0: metadata, video, plugin evolution, observability

- [ ] **Task 3.1: Surface code execution and citations**
  - Acceptance: Code execution output and grounding citations appear in optional `gemini_metadata` fields and are preserved through stream/non-stream responses without changing standard OpenAI fields.
  - Verify: `cargo test -p gemini-bridge-code-exec`; recorded upstream fixtures cover present/absent/malformed metadata.
  - Files: `crates/code-exec-surface/src/lib.rs`, `crates/code-exec-surface/src/extractor.rs`, `crates/openai-compat/src/metadata.rs`, `tests/gemini_metadata_test.rs`
  - Depends on: Tasks 0.5, 1.1

- [ ] **Task 3.2: Add experimental off-by-default video capability**
  - Acceptance: Disabled by default; unavailable upstream returns clear 501 not 500; when available, output follows image URL/b64 schema.
  - Verify: `cargo test -p gemini-bridge-adapter-video` with disabled/unavailable/supported fixture cases.
  - Files: `crates/video-adapter/src/lib.rs`, `crates/video-adapter/src/handler.rs`, `crates/config/src/model.rs`, `tests/video_test.rs`
  - Depends on: P.7, Task 1.4

- [ ] **Task 3.3: Define and implement reload without dropping active streams**
  - Acceptance: Before code, decision/spec update resolves built-in instance replacement versus dynamic library loading. Implement the approved `/admin/reload-plugin` mechanism; active SSE requests finish without interruption; reload completes in <2 seconds.
  - Verify: `cargo test -p gemini-bridge-health-admin`; run a reload while an active SSE stream is observed to complete.
  - Files: `crates/plugin-context/src/reload.rs`, `crates/health-admin/src/reload_handler.rs`, `crates/http-server/src/handlers/admin.rs`, `tests/reload_test.rs`
  - Depends on: P.1, 0.1, 1.1, 1.5; blocked until reload design is approved.

- [ ] **Task 3.4: Prove provider-neutral adapter contract**
  - Acceptance: Second mock/local adapter registers via the provider-neutral contract and serves a test request without Gemini-specific changes in core API/service crates.
  - Verify: `cargo test --test multi_adapter_test` proves routing to both implementations.
  - Files: `crates/llm-service/tests/mock_adapter.rs`, `crates/plugin-context/tests/adapter_registry_test.rs`, `tests/multi_adapter_test.rs`
  - Depends on: 0.1, 0.5, 0.7

- [ ] **Task 3.5: Add profiles/bundles/patch overlays**
  - Acceptance: Config composition supports approved profiles/bundles/patch overlays with deterministic precedence and rejects invalid combinations clearly.
  - Verify: `cargo test -p gemini-bridge-config` covers precedence and invalid profile cases.
  - Files: `crates/config/src/profiles.rs`, `crates/config/src/overlay.rs`, `crates/config/tests/profile_composition.rs`
  - Depends on: 0.2

- [ ] **Task 3.6: Add opt-in metrics and lightweight status dashboard**
  - Acceptance: `/metrics` is disabled by default; when enabled, exposes Prometheus-compatible request/error/latency data; status surface remains minimal and uses local service state.
  - Verify: `cargo test -p gemini-bridge-http-server`; scrape endpoint and confirm disabled/enabled behavior.
  - Files: `crates/http-server/src/metrics.rs`, `crates/middleware/src/metrics_layer.rs`, `crates/health-admin/src/dashboard.rs`, `tests/metrics_test.rs`
  - Depends on: 1.5, 1.7

- [ ] **Task 3.7: Produce release artifacts and operational docs**
  - Acceptance: Static release build and optional container/service helper meet the approved packaging constraints; docs explain unofficial automation/ToS, account risk, credential handling, storage and cleanup. No deployment is executed.
  - Verify: release build for target platform; inspect binary size and run cold-start/health smoke; container build if enabled.
  - Files: `README.md`, `Dockerfile`, `deploy/systemd/gemini-bridge.service`, `docs/operations.md`, `.github/workflows/release.yml`
  - Depends on: Checkpoints 0–2 and Tasks 3.1–3.6

### Checkpoint 3 — v2.0 release gate
- [ ] Code execution and citations surfaced; video is off-by-default with correct 501 fallback.
- [ ] Plugin reload <2 seconds without interruption (if approved scope/implementation is viable).
- [ ] Secondary adapter works without changing core abstractions.
- [ ] Profiles, metrics, dashboard, release packaging, and risk documentation verified.
- [ ] Cold start ≤150 ms, binary ≤25 MB and remaining project KPIs measured on target build.
- [ ] Full quality gates pass; all deviations from KPI targets are documented and reviewed.
