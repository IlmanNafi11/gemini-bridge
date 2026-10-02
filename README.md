# Gemini Bridge

A self-hosted Rust service that exposes Gemini Web through an OpenAI-compatible HTTP API. It supports chat completions, SSE streaming, file references, image generation, conversations, tool-call emulation, gallery management, health/admin surfaces, and optional local metrics.

> [!IMPORTANT]
> Gemini Bridge automates the unofficial Gemini Web interface. It is not affiliated with, endorsed by, or supported by Google. Upstream behavior can change without notice. Automation may violate applicable terms of service or trigger account restrictions. Use a dedicated account whose loss you can accept; you are responsible for reviewing and complying with the terms that apply to you.

## Quick start

### Build from source

Requirements: stable Rust with edition 2024 support and a C toolchain for bundled SQLite.

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

Copy the complete `Cookie` header from an authenticated Gemini Web browser session, then provide it through standard input so it is not retained in shell history:

```bash
./target/release/gemini-bridge --config bridge.toml auth login
./target/release/gemini-bridge --config bridge.toml doctor
```

Credential files are written with mode `0600` on Unix. Set `BRIDGE_SECRET` to encrypt stored cookies at rest. Do not place credentials in `bridge.toml`, service unit files, container images, source control, or command-line arguments.

## Configuration

`bridge.example.toml` documents the base settings. Environment variables prefixed with `BRIDGE_` override TOML values, using underscores for nested fields; for example:

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

Experimental video generation is disabled by default and returns an explicit `501` response when unavailable.

## Operations and packaging

See [docs/operations.md](docs/operations.md) for data layout, credentials, cleanup, backup/restore, systemd, container usage, upgrades, and troubleshooting.

Release tags build Linux archives and publish checksums through `.github/workflows/release.yml`. The checked-in systemd and container files are optional helpers; this repository does not deploy them automatically.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace
cargo build --release
```

The architecture and acceptance contracts live in `SPEC.md` and `docs/specs/`.

## License

MIT
