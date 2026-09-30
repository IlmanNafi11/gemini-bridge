# Implementation Plan: Gemini Bridge

**Status:** Approved — implementation may proceed per task order  
**Date:** 2026-09-30  
**Source of truth:** `SPEC.md` and `PRD-gemini-bridge.md`  
**Tracker:** `tasks/todo.md`  
**Scope:** Requirements and engineering work for Fase 0–3; no staging/production deployment execution.

## Overview

Deliver a self-hosted Rust service that exposes Gemini Web through the specified OpenAI-compatible API. Work is organized as end-to-end slices: first establish the project and a non-streaming chat path; then deliver streaming, files, image generation, and operations; then persistence, tool calling, and gallery; finally metadata extensions, experimental video, plugin lifecycle/reload, a second adapter, observability, and release packaging. Each task leaves a reviewable, testable result and is gated by its dependencies and phase checkpoint.

This repository currently contains project documents but no application source. Therefore source paths listed in `tasks/todo.md` are planned paths derived from the approved architecture, not existing conventions. Confirm crate naming and public interfaces in module specs before implementation.

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
Gemini adapter contract ─> experimental video capability
```

**Build order:**
1. Approve/align root and module contracts; write module specs in dependency order.
2. Fase 0: workspace/plugin context and config; transport; identity; neutral LLM contract and Gemini adapter; OpenAI-compatible non-streaming chat API.
3. Fase 1: streaming and media storage can proceed independently after Fase 0 contracts; upload depends on transport/identity/media storage; image generation depends on upload and adapter; operations and middleware integrate with the server.
4. Fase 2: persistence precedes continuity/branching; tool calling extends the chat contract; gallery builds on media storage and HTTP routing; full middleware policy applies across routes.
5. Fase 3: metadata and experimental video use established adapter contracts; reload builds on plugin lifecycle and live streams; secondary adapter proves provider neutrality; then metrics and release packaging.

## Work plan

Task acceptance criteria, verification commands, dependency IDs, and expected files are itemized in `tasks/todo.md`.

### Preparation — specification traceability
- **P.1:** Resolve the root spec's internal count mismatch (it states 17 modules but lists 18), align the diagram/phase labels and named crate/module paths, and record any remaining design decisions. Do not silently alter API or product scope.
- **P.2–P.7:** Write the 18 module specs in six dependency-ordered batches of three, each mapped to the approved capability IDs. Resolve public contracts and per-module acceptance/tests before coding.

### Fase 0 — Bootstrap: non-stream chat
- **0.1–0.2:** Cargo workspace/static plugin context and TOML/environment configuration.
- **0.3:** Configurable transport with the Phase 0 TLS fingerprint profile and HTTP/SOCKS5 proxy support.
- **0.4:** Session bootstrap, secure local cookie import/storage, and `auth login` / `doctor` CLI path.
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
- **3.2:** Off-by-default experimental video capability and explicit 501 behavior when unavailable.
- **3.3:** Runtime plugin reload with active stream preservation and measured <2-second reload, respecting the selected static-registry-first strategy.
- **3.4:** Second provider/mock adapter proving the neutral contract.
- **3.5:** Profiles/bundles/patch overlays, opt-in metrics, and lightweight status dashboard.
- **3.6:** Static binary and optional Docker/service artifacts plus required risk/ToS documentation; no environment deployment.
- **Checkpoint 3:** Phase 3 acceptance, binary-size/cold-start KPIs, plugin reload KPI, full quality gates, and documented release artifacts.

## Verification approach

- Run focused deterministic unit/integration tests for each slice; mock upstream protocol and failures with `wiremock`; use `insta` snapshots for upstream parser fixtures.
- Exercise user-visible REST/SSE routes end-to-end against a mock upstream at each endpoint milestone. Use live Gemini checks only for explicitly designated acceptance runs with credentials kept outside the repository.
- Run repository quality gates after each meaningful implementation slice: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and workspace unit/integration tests. Use `cargo nextest run --workspace` where installed.
- Phase gates additionally verify PRD-specific live image samples, resilience drills, latency/RSS, continuous-run success, binary size, and reload behavior where applicable. These operational KPIs require dedicated test environment and should not be represented as proven by unit tests.
- Maintain runnable service state after each vertical slice; do not mark a task or checkpoint complete based only on compilation.

## Risks and mitigations

| Risk / uncertainty | Impact | Mitigation / gate |
|---|---|---|
| Gemini `f.req`, response tree, and TLS fingerprint behavior change or library support is unsuitable | High | Prove transport and parser assumptions early in Fase 0 with fixtures and a live smoke; keep schema mapping external and isolate adaptation behind interfaces. |
| Root SPEC lists 18 modules but claims 17; module/crate names and dependency diagram are not fully aligned | Medium | P.1 resolves traceability and records any remaining decision before module specs or code. |
| Account restrictions, expiry, or IP flagging | High | Local-only credentials, `doctor`, status states, bounded refresh/retry, proxy configuration; no secrets in tests/logs. |
| SSRF, DNS rebinding, malicious/oversized uploads, sensitive cached media | High | Module-level threat contract; enforce size/MIME checks and DNS-pinned address validation; test boundary cases; document local storage and purge. |
| Latency/memory/static-binary KPIs conflict with TLS, database, and image dependencies | Medium | Measure at each relevant gate on target build; avoid claiming KPI success from local unit tests. |
| Dynamic library loading conflicts with static, zero-unsafe initial registry | High | Keep Fase 0–2 built-in registry. Before Fase 3, define whether reload means in-process replacement of built-in plugins or dynamic library loading; resolve ABI/unsafe/security implications before implementation. |
| Real upstream acceptance tests depend on account availability and upstream stability | Medium | Separate deterministic mock suite from opt-in live acceptance runs; report live acceptance prerequisites explicitly. |

## Open decisions / prerequisites

1. Reconcile the module count and implementation naming in `SPEC.md` before coding (P.1).
2. Specify the exact Fase 3 reload mechanism: built-in plugin instance replacement or dynamic library loading. The current approved material contains both static registry first and dynamic loading later.
3. Validate the selected TLS/JA3 implementation and actual profile coverage in a Fase 0 spike; the approved spec identifies the capability but not a settled Rust crate/API.
4. Confirm the source of upstream fixtures and how live tests are provisioned without committing account identifiers or credentials.
5. Choose the definitive config crate and confirm crypto primitive/key derivation in module specs; current SPEC offers alternatives / target behavior rather than settled details.
6. Deployment to staging/production, Dokploy setup, and CI deployment execution remain outside this plan's scope; build artifacts/configuration are included only where required by the PRD.

## Review gate

This document and `tasks/todo.md` are proposed for human review. Implementation starts only after the plan, module boundaries, unresolved decisions that block a task, and task tracker are accepted.
