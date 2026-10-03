# Gemini Bridge

A self-hosted Rust service that exposes Gemini Web through an OpenAI-compatible HTTP API. It supports chat completions, SSE streaming, file references, image generation, conversations, tool-call emulation, gallery management, health/admin surfaces, and optional local metrics.

> [!IMPORTANT]
> Gemini Bridge automates the unofficial Gemini Web interface. It is not affiliated with, endorsed by, or supported by Google. Upstream behavior can change without notice. Automation may violate applicable terms of service or trigger account restrictions. Use a dedicated account whose loss you can accept; you are responsible for reviewing and complying with the terms that apply to you.

## Quick start

### Build from source

Requirements: Rust 1.97.1 (edition 2024) and a C toolchain for bundled SQLite.

```bash
cargo build --release
cp bridge.example.toml bridge.toml
./target/release/gemini-bridge --config bridge.toml
```

The default listener is `127.0.0.1:8090`. Verify it locally:

```bash
curl --fail http://127.0.0.1:8090/healthz
```

### Import credentials

Copy the complete `Cookie` header from an authenticated Gemini Web browser session and paste it at the standard-input prompt so it is not retained in shell history. `auth login` probes the candidate against Gemini Web's `/app` bootstrap before installing it; if validation fails, the existing `cookies.json` is left unchanged (or absent on first login).

```bash
./target/release/gemini-bridge --config bridge.toml auth login
./target/release/gemini-bridge --config bridge.toml doctor
```

Credential files are written with mode `0600` on Unix. Set `BRIDGE_SECRET` to encrypt stored cookies at rest. Do not place credentials in `bridge.toml`, service unit files, container images, source control, or command-line arguments.

## Configuration

`bridge.example.toml` documents the base settings. Environment variables prefixed with `BRIDGE_` override TOML values using underscores for nested fields; for example:

```bash
export BRIDGE_SERVER_API_KEY='replace-with-a-long-random-value'
export BRIDGE_SERVER_METRICS_ENABLED=true
```

An API key is mandatory when binding outside localhost. All `/admin/*` routes require the configured bearer key even on localhost.

## API surface

- `POST /v1/chat/completions`
- `GET /v1/models`
- `POST /v1/files`, `GET /v1/files/{id}`
- `POST /v1/images/generations`, `GET /v1/images/{id}`
- Conversation list/history/branch/regenerate routes under `/v1/conversations`
- `GET /gallery` and `GET /gallery?format=html`
- `GET /healthz`, `GET /readyz`
- Protected `/admin/status`, `/admin/reauth`, `/admin/reload-plugin`, `/admin/purge`, and `/admin/dashboard`
- Protected `/metrics` when `server.metrics_enabled = true`

Successful SSE chat streams end with `[DONE]`. If the provider fails after streaming has started, the bridge emits a final JSON SSE error event and closes the stream **without** `[DONE]`; clients must not interpret a failed stream as a successful completion.

The shipped binary does not include a production Gemini video adapter. `POST /v1/videos/generations` and `GET /v1/videos/{id}` remain registered under the normal `/v1/*` API-key policy only to provide a JSON 501 `disabled` contract. Keep `[video].enabled = false`; setting it to `true` is rejected during configuration loading. Route/model types and crate-level mock behavior do not mean upstream generation or retrieval is wired.

Browser-named transport profiles are HTTP header presets only. Gemini Bridge uses ordinary certificate-verified `reqwest`/`rustls` negotiation and does not impersonate a browser JA3 or TLS ClientHello. Current Gemini Web compatibility requires an operator-observed, credentialed live check; deterministic mocks do not establish it.

## Releases

Tagged versions (`v*`) are built by `.github/workflows/release.yml` into a verified Linux archive and companion files:

- `gemini-bridge-<tag>-x86_64-unknown-linux-musl.tar.gz` — the static binary, `README.md`, `CHANGELOG.md`, `LICENSE`, example configuration, operations guide, and systemd unit.
- `SHA256SUMS` — the canonical checksum manifest for the archive and both SBOMs.
- `sbom.cdx.json` (CycloneDX 1.6) and `sbom.spdx.json` (SPDX 2.3) — generated from the locked dependency graph.
- GitHub artifact provenance and signed SBOM attestations, verifiable with `gh attestation verify`.

Quality gates (format, clippy, locked build, unit/integration tests, `cargo audit`, static-binary smoke, container health, and systemd syntax) run on every pull request to `staging` and again before any release is published.

## Operations and packaging

See [docs/operations.md](docs/operations.md) for data layout, credentials, cleanup, backup/restore, systemd, container usage, exact upgrade/rollback commands, and load/soak tooling. The checked-in systemd and container files are optional helpers; this repository does not deploy them automatically.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace
cargo build --release
```

The architecture and acceptance contracts live in `SPEC.md` and `docs/specs/`. Packaging and benchmark helpers live in `scripts/release/` and `scripts/bench/`.

## License

MIT — see [LICENSE](LICENSE).
