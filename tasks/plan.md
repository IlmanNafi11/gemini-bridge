# Implementation Plan: Gemini Bridge

**Status:** Approved — implementation may proceed per task order  
**Date:** 2026-09-30  
**Source of truth:** `SPEC.md` and `PRD-gemini-bridge.md`  
**Tracker:** `tasks/todo.md`  
**Scope:** Requirements and engineering work for Fase 0–3; no staging/production deployment execution.

## Overview

Deliver a self-hosted Rust service that exposes Gemini Web through the specified OpenAI-compatible API. Work is organized as end-to-end slices: chat and transport; streaming, files, image generation, and operations; persistence, tool calling, and gallery; then metadata, plugin lifecycle/reload, a second adapter, observability, and release packaging. Video remains a reserved route/model contract only: no production generator is shipped.

The application workspace and implementation now exist. Paths in `tasks/todo.md` identify the current implementation or verification surface; completed markers describe deterministic repository evidence only, while checkpoints retain live, KPI, soak, browser, and platform gates.

## Architecture and dependency graph

The graph below follows the explicit `Depends On` column in `SPEC.md`. Endpoint integrations add runtime composition edges: image generation uses the HTTP server; conversation and tool routes extend the existing chat API; operational middleware wraps inbound routes.

```text
plugin-context ──────────────┬─> llm-service ─> openai-compat ─> http-server
                             │                                  │
                             └─> middleware ────────────────────┤
config ─> transport ─> identity ─> gemini-adapter ──────────────┘
  │          │            │              │
  │          │            └─> upload ─────┴─> image-gen <─ media-store
  ├─> media-store                              │
  └─> conversation-store <─────────────────────┘ (chat integration)

http-server + media-store ─> gallery
openai-compat + llm-service ─> tool-calling (chat integration)
gemini-adapter ─> code-exec-surface
llm-service / provider contract ─> secondary adapter proof
plugin-context ─> plugin lifecycle/reload; http-server + identity expose operations
Reserved video route/models (production enablement rejected; no adapter runtime)
```

**Build order:**
1. Approve/align root and module contracts; write module specs in dependency order.
2. Fase 0: workspace/plugin context and config; transport; identity; neutral LLM contract and Gemini adapter; OpenAI-compatible non-streaming chat API.
3. Fase 1: streaming and media storage can proceed independently after Fase 0 contracts; upload depends on transport/identity/media storage; image generation depends on upload and adapter; operations and middleware integrate with the server.
4. Fase 2: persistence precedes continuity/branching; tool calling extends the chat contract; gallery builds on media storage and HTTP routing; full middleware policy applies across routes.
5. Fase 3: metadata, the disabled video route contract, reload, provider-neutrality proof, metrics, and release packaging build on established interfaces. Production video generation is not part of the shipped phase.

## Work plan

Task acceptance criteria, verification commands, dependency IDs, and expected files are itemized in `tasks/todo.md`.

### Preparation — specification traceability
- **P.1:** Resolve the root spec's internal count mismatch (it states 17 modules but lists 18), align the diagram/phase labels and named crate/module paths, and record any remaining design decisions. Do not silently alter API or product scope.
- **P.2–P.7:** Write the 18 module specs in six dependency-ordered batches of three, each mapped to the approved capability IDs. Resolve public contracts and per-module acceptance/tests before coding.

### Fase 0 — Bootstrap: non-stream chat
- **0.1–0.2:** Cargo workspace/static plugin context and TOML/environment configuration.
- **0.3:** Configurable transport with browser-style HTTP header presets and HTTP/SOCKS5 proxy support. Presets do not impersonate JA3 or the TLS ClientHello.
- **0.4:** Session bootstrap, secure local cookie import/storage, and `auth login` / `doctor` CLI path. `auth login` validates a candidate with `/app` before atomically installing it; failure preserves the prior file. Live upstream compatibility remains a separate credentialed gate.
- **0.5–0.6:** Provider-neutral LLM contract and Gemini Web non-stream protocol/parser, with upstream schema data and fixtures.
- **0.7:** OpenAI-compatible non-streaming chat and models endpoint.
- **Checkpoint 0:** Full chat request through local REST endpoint to mock upstream; live upstream smoke only with locally supplied credentials; unit/integration, lint, formatting, and contract checks pass.

### Fase 1 — MVP: stream, image, files, operations
- **1.1:** SSE chat with cumulative-response parsing, prefix-diff, and defined non-prefix rewrite behavior.
- **1.2:** Content-addressed media cache with metadata and TTL/purge primitives.
- **1.3:** Validated multipart upload, SSRF-safe URL references, resumable upstream upload, and file retrieval.
- **1.4:** Image generation, reference handling, URL extraction, caching, and `url` / `b64_json` retrieval.
- **1.5:** Health/readiness/admin status, session rotation, and bounded 405 refresh/retry behavior.
- **1.6:** Request IDs, secret redaction, and rate-limit/audit middleware integrated across routes.
- **Checkpoint 1:** OpenAI core endpoint acceptance; golden chat/image/file tests; 405/429/expiry drills; stated latency/memory KPIs; seven-day stability criterion. Live tests require operator-provided local credentials and must never store them in fixtures.

### Fase 2 — v1.1: continuity, tool calling, gallery
- **2.1–2.2:** Persist upstream/local conversation identifiers and message history, then expose history, branching, regeneration, and degraded continuity fallback.
- **2.3:** Tool schema handling, parse/validation, and tool-result continuation over the chat path.
- **2.4:** Gallery JSON/HTML, media retrieval/deletion/filtering, and purge/TTL integration.
- **2.5:** Complete middleware behavior and cross-feature integration checks.
- **Checkpoint 2:** Branching/continuity acceptance, 50-case tool parse suite at ≥95%, gallery behaviors, and full quality gates.

### Fase 3 — v2.0: extensions and release readiness
- **3.1:** Surface code execution output and citations as optional `gemini_metadata`.
- **3.2:** Preserve deterministic video request/error models and routes under the configured `/v1/*` API-key policy, returning 501 while production generation is unshipped; reject `video.enabled = true` during config loading.
- **3.3:** Runtime plugin reload with active stream preservation and measured <2-second reload, respecting the selected static-registry-first strategy.
- **3.4:** Second provider/mock adapter proving the neutral contract.
- **3.5:** Profiles/bundles/patch overlays, opt-in metrics, and lightweight status dashboard.
- **3.6:** Static binary and optional Docker/service artifacts plus required risk/ToS documentation; no environment deployment.
- **Checkpoint 3:** Phase 3 acceptance, binary-size/cold-start KPIs, plugin reload KPI, full quality gates, and documented release artifacts.

## Verification approach

- Run focused deterministic unit/integration tests for each slice; mock upstream protocol and failures with `wiremock`; use `insta` snapshots for upstream parser fixtures.
- Exercise user-visible REST/SSE routes end-to-end against a mock upstream at each endpoint milestone. A failed SSE emits a final JSON error event and closes without `[DONE]`; `[DONE]` is success-only.
- Use live Gemini checks only for explicitly designated acceptance runs with credentials kept outside the repository. Header-preset tests do not prove live compatibility or JA3/ClientHello impersonation.
- Phase gates additionally verify PRD-specific live image samples, resilience drills, latency/RSS, continuous-run success, binary size, and reload behavior where applicable. These operational KPIs require a dedicated environment and are never represented as proven by deterministic tests.
- Maintain runnable service state after each vertical slice; do not mark a task or checkpoint complete based only on compilation.

## Risks and mitigations

| Risk / uncertainty | Impact | Mitigation / gate |
|---|---|---|
| Gemini `f.req` or response-tree behavior changes | High | Keep schema mapping external, exercise sanitized fixtures early, and require a separate credentialed live smoke for current upstream compatibility. |
| Browser-named transport profiles could be mistaken for TLS impersonation | High | Specify and test them as HTTP header presets only; make no JA3/ClientHello claim. |
| Account restrictions, expiry, or IP flagging | High | Local-only credentials, `doctor`, status states, bounded refresh/retry, proxy configuration; no secrets in tests/logs. |
| SSRF, DNS rebinding, malicious/oversized uploads, sensitive cached media | High | Enforce size/MIME and DNS-pinned address validation; test boundary cases; document local storage and manual purge. |
| Latency/memory/static-binary KPIs conflict with TLS, database, and image dependencies | Medium | Measure at each relevant gate on target build; avoid claiming KPI success from local unit tests. |
| Dynamic library loading conflicts with static, zero-unsafe initial registry | High | Use built-in instance replacement rather than runtime dynamic library loading. |
| Real upstream acceptance tests depend on account availability and upstream stability | Medium | Separate deterministic mock suites from opt-in live acceptance runs; report live acceptance prerequisites explicitly. |

## External evidence prerequisites

1. Current Gemini Web compatibility requires operator-provided credentials and an observed live `doctor`/chat/image run; deterministic header, parser, and mock tests do not establish it.
2. Live credentials/account identifiers and the source environment for live fixtures remain external and must not be committed.
3. Latency, RSS, cold-start, binary-size, and seven-day soak claims require retained target-environment measurements.
4. Browser axe verification requires an actual browser runtime; Docker checks require an available Docker daemon. Unavailable tools remain reported blockers rather than inferred passes.
5. Deployment to staging/production, Dokploy setup, and CI deployment execution remain outside this plan's scope; build artifacts/configuration are included only where required by the PRD.

## Review gate

This document and `tasks/todo.md` are proposed for human review. Implementation starts only after the plan, module boundaries, unresolved decisions that block a task, and task tracker are accepted.

## Release remediation plan — 2026-10-02

The production-readiness review found security, contract, persistence, runtime,
accessibility, and release-pipeline gaps. The operator approved a complete
remediation pass. Work proceeds risk-first and remains on `dev`.

### R.1 — Harden untrusted media retrieval
- Parse provider-returned media URLs, enforce HTTPS and an exact allowlisted
  host boundary, resolve and pin globally routable addresses, validate every
  redirect, bound time and bytes, and require recognized media signatures.
- Apply the shared media-fetch policy to every shipped untrusted media retrieval path. The unshipped video adapter remains outside production downloader claims.
- Add regression tests for host-confusion SSRF, private addresses, redirects,
  oversized/chunked bodies, timeout/error behavior, and MIME mismatch.

### R.2 — Restore runtime contract truth
- Make Gemini streaming incremental from the upstream socket through SSE with
  cancellation/back-pressure and bounded frame/body buffers.
- Make video configuration truthful: production video is not shipped,
  `video.enabled = true` is rejected, and the registered contract-only routes
  return JSON 501 while disabled.
- Wire explicit-origin CORS, configurable request limits, strict API-key
  validation, profiles/bundles/patch selectors, external schema self-check,
  startup credential validation, and graceful shutdown.
- Add binary/HTTP tests for every production wiring branch and complete error
  mappings, including mid-stream failures and non-loopback binding.

### R.3 — Protect credentials and durable state
- Rotate `1PSIDTS` through the production 405 recovery path while preserving
  error taxonomy and retry bounds.
- Validate candidate credentials before atomically replacing the persisted
  credential file; use a password KDF and enforce strong secret input.
- Persist complete conversation turns and upstream identifiers in one
  transaction with unique sequence constraints and deterministic concurrent
  behavior; surface persistence failures.
- Apply configured media TTL as expiry metadata, retain restrictive data permissions and indexed/bounded metadata queries, and keep reference accounting race-safe. Cleanup remains an explicit authenticated admin purge; no scheduled deletion is claimed.

### R.4 — Complete release, UI, and operational gates
- Require authorization on mutating loopback APIs, preserve the documented
  single-operator boundary, add security headers, and close gallery XSS/auth,
  label, landmark, focus, contrast, and status-announcement gaps.
- Pin CI actions/toolchain/base images, separate least-privilege publication,
  add fmt/clippy/test/audit/static-artifact/container/systemd/archive gates,
  publish an authenticated checksum manifest, SBOM/provenance, and `LICENSE`.
- Add CLI, migration, concurrency, error-map, metrics-cardinality, request-ID,
  logging, image-count/error, purge-reference, reload-error, and packaging tests.
- Add opt-in live upstream suites and reproducible KPI/load/soak tooling. Live
  credentials and elapsed soak evidence remain external prerequisites and may
  not be represented as passed without an observed run.

### Remediation verification gate
- Focused regression tests pass after each slice.
- `cargo fmt --all -- --check`, workspace clippy with warnings denied, locked
  workspace build/tests, and `cargo audit` pass.
- Static-musl artifact passes size, cold-start, health/readiness/auth, archive,
  checksum, and runtime smoke checks.
- Browser gallery verification has no axe-core violations and works at required
  responsive widths with keyboard navigation.
- Docker/systemd checks and opt-in external tests are either observed passing or
  explicitly reported as external blockers; they are never inferred.
