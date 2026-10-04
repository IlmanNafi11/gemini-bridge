# Todo — Gemini Bridge

**Status:** Approved — work in progress.  
**Plan:** `tasks/plan.md` · **Specification:** `SPEC.md`  
**Scope:** Full Fase 0–3 roadmap.  

> Source paths identify current implementation and verification surfaces. Keep milestone checkpoints open until their required evidence is observed; deterministic local test results do not close external live, KPI, soak, browser, or platform gates.

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
  - Acceptance: Specs define tool parsing/fallback, optional metadata, and the reserved video request/error shapes while stating that production video is unshipped, enablement is rejected, and current routes return 501.
  - Verify: Trace US-5 and US-7 to shipped behavior; record US-9 generation behavior as future scope and ensure no audio/TTS scope is introduced.
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

- [x] **Task 0.3: Implement outbound transport and header profiles**
  - Acceptance: Configured HTTP/SOCKS5 proxy and selected browser-style HTTP header preset are applied by the outbound client. Profiles do not impersonate JA3 or the TLS ClientHello.
  - Verify: `cargo test -p gemini-bridge-transport` covers deterministic header/proxy/client behavior. A credentialed live `doctor` probe remains an open external compatibility gate and is not required to substantiate TLS impersonation.
  - Files: `crates/transport/Cargo.toml`, `crates/transport/src/lib.rs`, `crates/transport/src/client.rs`, `crates/transport/src/tls.rs`
  - Depends on: Task 0.2

- [x] **Task 0.4: Implement identity bootstrap and local auth commands**
  - Acceptance: `/app` bootstrap extracts required session tokens; `auth login` probes candidate credentials before atomic persistence and leaves the existing file unchanged on failure; persisted credentials use 0600 permissions and optional configured encryption; secret values are not logged. `doctor` provides actionable outcomes.
  - Verify: `cargo test -p gemini-bridge-identity`; CLI process tests cover successful import and failed-probe no-replacement behavior; live `doctor` remains opt-in and uses credentials outside git.
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
  - Acceptance: `stream=true` produces OpenAI SSE delta chunks; clean streams end with `[DONE]`, while mid-stream failures end with a JSON error event and no `[DONE]`; newline-framed Gemini snapshots and prefix changes are parsed with defined non-prefix rewrite behavior.
  - Verify: Adapter unit/property boundary tests and focused HTTP tests assert chunks, ordering, clean terminal event, and the failed-stream no-`[DONE]` contract.
  - Files: `crates/gemini-adapter/src/stream.rs`, `crates/gemini-adapter/src/prefix_diff.rs`, `crates/http-server/src/handlers/chat.rs`, `tests/e2e_chat_test.rs`
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
  - Verify: `cargo test -p gemini-bridge-health-admin`; `cargo test --test health_admin_test`; retain live/quality gates open until rerun on the final tree.
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
  - Verify: `cargo test -p gemini-bridge-conversation-store`; retain workspace quality gates open until rerun on the final tree.
  - Files: `crates/conversation-store/Cargo.toml`, `crates/conversation-store/src/lib.rs`, `crates/conversation-store/src/db.rs`, `crates/conversation-store/src/migrations.rs`, `crates/conversation-store/src/models.rs`, `crates/conversation-store/src/error.rs`, `crates/conversation-store/tests/store_test.rs`
  - Depends on: Tasks 0.2, 0.7

- [x] **Task 2.2: Add continuity, history, branch and regenerate flows**
  - Acceptance: Chat with `conversation_id` resumes upstream IDs; history/list endpoints reflect persisted state; branch creates a separate path from selected message; regenerate uses selected prior state; rejected upstream ID replays history and sets `x-gemini-bridge-continuity: degraded`.
  - Verify: `cargo test --test e2e_conversations_test` covers multi-turn, branch isolation, regeneration and degraded fallback.
  - Files: `crates/conversation-store/src/branch.rs`, `crates/http-server/src/handlers/conversations.rs`, `crates/openai-compat/src/chat_models.rs`, `tests/e2e_conversations_test.rs`
  - Depends on: Tasks 1.1, 2.1

- [x] **Task 2.3: Add tool-call emulation and result continuation**
  - Acceptance: `tools[]` and `tool_choice` map to prompt schema; valid model output becomes structured tool calls; tool results continue as role `tool`; malformed output falls back to text with structured warning; parse rate ≥95% on 50 deterministic cases.
  - Verify: `cargo test -p gemini-bridge-tool-calling`; `cargo test --test tool_calling_test`; retain workspace quality gates open until rerun on the final tree.
  - Files: `crates/tool-calling/src/lib.rs`, `crates/tool-calling/src/injector.rs`, `crates/tool-calling/src/parser.rs`, `crates/openai-compat/src/tools.rs`, `tests/tool_calling_test.rs`
  - Depends on: Tasks 0.7, 1.1

- [x] **Task 2.4: Deliver gallery JSON and embedded HTML path**
  - Acceptance: `/gallery` JSON and `?format=html` show cached items; date/model/prompt filters work; delete/download actions update/use media store; HTML has no external runtime asset dependency.
  - Verify: `cargo test -p gemini-bridge-gallery`; run service and visually check rendered gallery plus filtering/deletion.
  - Files: `crates/gallery/src/lib.rs`, `crates/gallery/src/handlers.rs`, `crates/gallery/static/gallery.html`, `crates/http-server/src/handlers/gallery.rs`, `tests/gallery_test.rs`
  - Depends on: Tasks 0.7, 1.2, 1.4

- [x] **Task 2.5: Add media expiry metadata and manual purge administration**
  - Acceptance: Configured TTL marks new records with an expiry timestamp. Expired records remain until an operator invokes the protected purge endpoint, which removes eligible metadata/content consistently; no scheduler or automatic cleanup is claimed.
  - Verify: `cargo test -p gemini-bridge-media-store`; `cargo test --test media_purge_test`; retain workspace quality gates open until rerun on the final tree.
  - Files: `crates/media-store/src/cleanup.rs`, `crates/health-admin/src/purge.rs`, `crates/http-server/src/handlers/admin.rs`, `tests/media_purge_test.rs`
  - Depends on: Tasks 1.2, 1.5

### Checkpoint 2 — v1.1 gate
- [ ] Conversation continuity, list/history, branch, regenerate, and degraded replay verified end-to-end.
- [ ] Tool call parse rate ≥95% over 50 cases and tool-result continuation passes.
- [ ] Gallery filtering, retrieval and deletion pass manual/runtime verification.
- [ ] E1 ≥95%, E3 stated latency/memory KPIs verified, and quality gates pass.

## Fase 3 — v2.0: metadata, reserved video contract, plugin evolution, observability

- [x] **Task 3.1: Surface code execution and citations**
  - Acceptance: Code execution output and grounding citations appear in optional `gemini_metadata` fields and are preserved through stream/non-stream responses without changing standard OpenAI fields.
  - Verify: `cargo test -p gemini-bridge-code-exec`; recorded upstream fixtures cover present/absent/malformed metadata.
  - Files: `crates/code-exec-surface/src/lib.rs`, `crates/code-exec-surface/src/extractor.rs`, `crates/openai-compat/src/metadata.rs`, `tests/gemini_metadata_test.rs`
  - Depends on: Tasks 0.5, 1.1

- [x] **Task 3.2: Preserve the unsupported video route contract**
  - Acceptance: Production video generation is not shipped; `[video].enabled = true` fails configuration validation; the registered routes follow the configured `/v1/*` API-key policy and return JSON 501 `disabled` while no production service is wired. URL/base64 generation and upstream capability behavior remain future, unimplemented scope.
  - Verify: `rejects_enabled_video_without_production_adapter` plus focused HTTP disabled-route/auth tests. Crate-level mock generation tests prove only reusable deterministic components, not production wiring or live video support.
  - Files: `crates/video-adapter/src/lib.rs`, `crates/video-adapter/src/handler.rs`, `crates/config/src/loader.rs`, `crates/http-server/src/handlers/videos.rs`, `tests/video_test.rs`
  - Depends on: P.7, Task 1.4

- [x] **Task 3.3: Define and implement reload without dropping active streams**
  - Acceptance: Implement the approved `/admin/reload-plugin` built-in instance replacement mechanism; active SSE requests finish without interruption; reload completes in <2 seconds.
  - Verify: Focused reload integration, plugin-context lifecycle, and health-admin tests; retain full quality gates and reload KPI as open until rerun/observed on the final tree.
  - Files: `crates/plugin-context/src/reload.rs`, `crates/health-admin/src/reload_handler.rs`, `crates/http-server/src/handlers/admin.rs`, `tests/reload_test.rs`
  - Depends on: P.1, 0.1, 1.1, 1.5

- [x] **Task 3.4: Prove provider-neutral adapter contract**
  - Acceptance: Second mock/local adapter registers via the provider-neutral contract and serves a test request without Gemini-specific changes in core API/service crates.
  - Verify: `cargo test --test multi_adapter_test` proves the provider-neutral registry is registered through plugin context and routes completion and streaming requests to both implementations.
  - Files: `crates/llm-service/src/lib.rs`, `crates/llm-service/tests/mock_adapter.rs`, `crates/llm-service/tests/contract_test.rs`, `tests/multi_adapter_test.rs`
  - Depends on: 0.1, 0.5, 0.7

- [x] **Task 3.5: Add profiles/bundles/patch overlays**
  - Acceptance: Config composition supports approved profiles/bundles/patch overlays with deterministic precedence and rejects invalid combinations clearly.
  - Verify: `cargo test -p gemini-bridge-config` with profile-composition cases; retain workspace quality gates open until rerun on the final tree.
  - Files: `crates/config/src/profiles.rs`, `crates/config/src/overlay.rs`, `crates/config/tests/profile_composition.rs`
  - Depends on: 0.2

- [x] **Task 3.6: Add opt-in metrics and lightweight status dashboard**
  - Acceptance: `/metrics` is disabled by default; when enabled, it exposes Prometheus-compatible request/error/latency data; the status surface remains minimal and uses local service state.
  - Verify: `cargo test --test metrics_test`; `cargo test -p gemini-bridge-config`; `cargo test -p gemini-bridge-middleware`; retain workspace quality gates open until rerun on the final tree.
  - Files: `crates/http-server/src/metrics.rs`, `crates/middleware/src/metrics_layer.rs`, `crates/health-admin/src/dashboard.rs`, `tests/metrics_test.rs`
  - Depends on: 1.5, 1.7

- [x] **Task 3.7: Produce release artifacts and operational docs**
  - Acceptance: Static release build and optional container/service helper meet the approved packaging constraints; docs explain unofficial automation/ToS, account risk, credential handling, storage and cleanup. No deployment is executed.
  - Verify: release build for target platform; inspect binary size and run cold-start/health smoke; container build if enabled.
  - Files: `README.md`, `Dockerfile`, `deploy/systemd/gemini-bridge.service`, `docs/operations.md`, `.github/workflows/release.yml`
  - Depends on: Checkpoints 0–2 and Tasks 3.1–3.6

### Checkpoint 3 — v2.0 release gate
- [ ] Code execution and citations surfaced; production video remains unshipped, enablement is rejected, and contract-only routes return JSON 501.
- [ ] Plugin reload <2 seconds without interruption (if approved scope/implementation is viable).
- [ ] Secondary adapter works without changing core abstractions.
- [ ] Profiles, metrics, dashboard, release packaging, and risk documentation verified.
- [ ] Cold start ≤150 ms, binary ≤25 MB and remaining project KPIs measured on target build.
- [ ] Full quality gates pass; all deviations from KPI targets are documented and reviewed.

## Release remediation — 2026-10-02 (operator-approved complete pass)

> Source of truth for the remediation: `tasks/plan.md` § “Release remediation plan”. Keep every remediation item open until its own focused evidence is final. A prior or aggregate workspace run does not prove each subcriterion, and no deterministic run closes live credential, KPI/soak, browser axe, Docker, or human-review gates.

### R.1 — Harden untrusted media retrieval
- [x] **R.1.1 SSRF/resource-safe media downloader** — parse extracted media URLs, enforce HTTPS + exact host allowlist, pin resolved addresses, validate every redirect, bound time and bytes, and verify signatures for shipped image/media fetch paths. Production video is unshipped and is not part of this runtime claim.
  - Verify: focused image/media downloader tests plus relevant integration tests.
  - Files: `crates/image-gen/src/extractor.rs`, `crates/image-gen/src/lib.rs`, shared download module, related tests.
- [x] **R.1.2 Media-downloader regression tests** — host-confusion SSRF, blocked/private/IPv6/rebinding, redirect chains, chunked oversize, timeout/error, MIME mismatch, b64/url consistency.
  - Verify: focused test suites pass and demonstrate red (before) → green (after).
  - Files: `crates/upload/tests/media_fetch_test.rs`, `crates/video-adapter/tests/media_fetch_test.rs`.

### R.2 — Restore runtime contract truth
- [x] **R.2.1 Incremental streaming** — transport streams upstream bytes; adapter emits events as frames arrive; HTTP SSE starts before upstream completion; cancellation propagates; buffers bounded.
  - Verify: chunked-upstream integration test asserts first downstream event precedes upstream completion; existing SSE tests pass.
  - Files: `crates/transport/src/client.rs`, `crates/gemini-adapter/src/lib.rs`, `crates/gemini-adapter/src/stream.rs`, `crates/http-server/src/handlers/chat.rs`, tests.
- [x] **R.2.2 Video enablement truth** — production video is not shipped; `video.enabled = true` is rejected; registered video routes remain JSON 501 `disabled` contracts, so no enablement flag is silently ignored.
  - Verify: config rejection test and focused HTTP disabled-route/auth tests; crate-level supported-generation mocks are not production evidence.
  - Files: `crates/config/src/loader.rs`, `crates/http-server/src/handlers/videos.rs`, `src/main.rs`, tests.
- [x] **R.2.3 Config/API-key/CORS/limits wiring** — reject empty/weak API keys; wire explicit-origin CORS; wire configurable rate limits with bounded concurrency and body caps; expose profiles/bundles/patch CLI selectors; load external schema via canonical self-check path.
  - Verify: config validation tests, CORS integration tests, rate-limit binary smoke, composition CLI test, adapter self-check test.
  - Files: `crates/config/src/*`, `crates/http-server/src/lib.rs`, `src/main.rs`, `src/cli.rs`, tests.
- [x] **R.2.4 Startup readiness and graceful shutdown** — bounded startup credential bootstrap before readiness; SIGTERM/SIGINT drain; in-flight request/stream preserved during shutdown.
  - Verify: release smoke asserts readiness transitions and termination drains; focused integration test for drain.
  - Files: `src/main.rs`, `crates/http-server/src/lib.rs`, `crates/identity/src/session.rs`, tests.

### R.3 — Protect credentials and durable state
- [x] **R.3.1 Cookie rotation wired** — production 405 recovery rotates `1PSIDTS` via the single-flight operation, persists it, retries once, and preserves error taxonomy.
  - Verify: adapter-level integration test proves stored cookie changed and retry used it.
  - Files: `crates/gemini-adapter/src/lib.rs`, `crates/identity/src/rotation.rs`, `crates/identity/src/session.rs`, tests.
- [x] **R.3.2 Atomic reauth + KDF secrets** — validate candidate credentials before atomically replacing `cookies.json`; strong-secret validation; Argon2id-style KDF.
  - Verify: identity tests for atomic replace, wrong-key, strong-secret validation.
  - Files: `crates/identity/src/crypto.rs`, `crates/identity/src/storage.rs`, `crates/identity/src/session.rs`, `crates/health-admin/src/lib.rs`, tests.
- [x] **R.3.3 Transactional conversation turns** — complete turn + upstream IDs in one transaction, unique `(conversation_id, sequence_number)`, deterministic concurrent behavior, persistence failures surfaced.
  - Verify: conversation-store concurrency/migration tests; e2e conversation tests still pass.
  - Files: `crates/conversation-store/src/db.rs`, `crates/conversation-store/src/migrations.rs`, `crates/http-server/src/handlers/chat.rs`, tests.
- [x] **R.3.4 Media expiry, permissions, indexing, reference safety** — configured TTL marks new objects for expiry; cleanup is an explicit authenticated admin purge, not a scheduler. Also require restrictive data-root permissions, indexed/bounded metadata queries, race-safe reference accounting, and a shared-hash purge test.
  - Verify: media-store expiry/permission/index tests; explicit purge shared-hash test; gallery pagination tests.
  - Files: `crates/media-store/src/store.rs`, `crates/media-store/src/cleanup.rs`, `crates/media-store/src/index.rs`, `crates/upload/src/lib.rs`, `crates/image-gen/src/lib.rs`, `crates/gallery/src/handlers.rs`, tests.

### R.4 — Complete release, UI, and operational gates
- [x] **R.4.1 Mutating-API authorization + security headers** — Require key/Origin policy on state-changing loopback endpoints; add security headers; preserve documented single-operator boundary.
  - Verify: auth-matrix integration tests; browser header check.
  - Files: `crates/http-server/src/lib.rs`, `crates/http-server/src/handlers/files.rs`, tests.
- [x] **R.4.2 Gallery a11y + XSS + auth** — contrast AA, landmarks/labels, focus/announcements; prompt XSS rendered as text; auth matrix for list/download/delete.
  - Verify: axe-core 0 violations in browser; DOM/HTTP tests for XSS and auth matrix.
  - Files: `crates/gallery/static/gallery.html`, `crates/gallery/src/handlers.rs`, `tests/gallery_test.rs`.
- [x] **R.4.3 Release pipeline hardening** — pin actions/toolchain to SHAs, least-privilege publish job, fmt/clippy/test/audit/static/archive/container/systemd gates, checksum manifest matching docs, SBOM/provenance, `LICENSE` packaged.
  - Verify: workflow syntax check; archive/checksum smoke; systemd-analyze verify; Docker build if daemon available.
  - Files: `.github/workflows/release.yml`, `.github/workflows/staging-quality.yml`, `docs/operations.md`, `LICENSE`, `Dockerfile`.
- [x] **R.4.4 Regression and coverage suites** — CLI login/doctor, migrations, conversation/media concurrency, error-map, mid-stream errors, CORS, non-loopback bind, metrics cardinality, request-ID, audit log, image `n>1`/no-image, purge reference-count, reload-error, push retry.
  - Verify: full workspace suite (existing + new) passes.
  - Files: `tests/*`, `crates/*/tests/*`.
- [ ] **R.4.5 Live/KPI external evidence** — opt-in live upstream suites (chat/image/tool) and reproducible KPI/load/soak tooling are documented. A successful operator-reported `doctor` run was received on 2026-10-03 (`Session status: Valid`; `bl`, `SNlM0e`, and `f.sid` present). This proves a live bootstrap at that time only; live chat/image/tool suites, measured target KPIs, and seven-day success remain unverified.
  - Verify: tooling runs against local mocks; live runs require operator credentials and retained results. A successful command start is not completion evidence.
  - Current blocker: An operator-supplied Cookie header was submitted to `auth login` through hidden stdin on 2026-10-04; validation failed with `Bootstrap response is missing required field: SNlM0e`, so the prior credential file remains unchanged. CamoFox reports an active Gemini tab, but its persisted state does not contain the three required session cookies. Live acceptance awaits a cookie header copied freshly from the authenticated browser's `/app` request. KPI, Docker runtime build/smoke, seven-day soak, and human review remain open.
  - Files: `scripts/live/*` (opt-in), `scripts/bench/*`, `docs/operations.md`.

### Remediation checkpoints
- [x] **Checkpoint R-A:** every R.1–R.2 item has a focused regression test and passes.
- [x] **Checkpoint R-B:** every R.3–R.4 item passes its focused tests and the complete suite.
- [x] **Checkpoint R-C:** fmt/clippy/workspace tests/locked builds/`cargo audit`/static artifact/browser axe all pass; external prerequisites are either observed or explicitly listed.
