# Operations

Gemini Bridge is a single-operator, self-hosted service. It is not a multi-tenant security boundary and must not be exposed directly to untrusted networks.

## Unofficial automation and account risk

Gemini Bridge drives the unofficial Gemini Web interface rather than a supported public API. Google may change the page, wire protocol, cookies, models, or access policy at any time. Automated access may violate applicable terms of service, trigger challenges or rate limits, flag an IP address, or suspend the account.

Use a dedicated account whose loss is acceptable. Do not use an organization-critical or personal primary account. Review the current terms yourself before operating the service. Mocked tests prove local contracts only; they are not evidence that a live Gemini account, upstream schema, image path, or seven-day run currently works. Browser-named transport profiles select HTTP headers only and do not impersonate JA3 or a TLS ClientHello.

## Runtime prerequisites

- Build from source: Rust 1.97.1 with edition-2024 support and a C toolchain.
- Static release build: `x86_64-unknown-linux-musl` target and `musl-tools` (or equivalent `musl-gcc`).
- CLI credential import and `doctor`: an interactive terminal, a dedicated Gemini account, and a current complete browser `Cookie` header.
- Live chat/image/tool checks and load runs: explicit operator-provided credentials, acceptable account-risk posture, permitted outbound HTTPS, and a non-production API key.
- Container checks: Docker daemon. Unit verification: systemd 249 or newer is recommended for the hardening directives.

The repository contains no cookies, API keys, IP addresses, or live-test results. Do not treat CI mock success as external evidence.

## Network boundary

The default bind is `127.0.0.1:8090`. Keep it unless a trusted reverse proxy or private network provides the external boundary. A non-loopback bind requires a configured API key.

- Use a long random API key delivered through `BRIDGE_SERVER_API_KEY` or a root-readable environment file.
- Never place Google cookies or `BRIDGE_SECRET` in TOML, unit files, images, command lines, logs, or source control.
- Terminate TLS at a trusted reverse proxy whenever traffic leaves the host.
- `/admin/*` and optional `/metrics` require the bearer API key.
- Keep CORS disabled unless explicitly configured origins are required.

## Unsupported video configuration

The shipped binary does not include a production Gemini video adapter. `POST /v1/videos/generations` and `GET /v1/videos/{id}` are registered under the normal `/v1/*` API-key policy only as JSON HTTP 501 `disabled` contracts. Keep `[video].enabled = false`; setting it to `true` is rejected during config loading. Route/model types and crate-level deterministic mocks are not evidence that upstream video generation or retrieval is wired.

## Credentials and CLI checks

Run login as the same operating-system account that runs the service. Paste the complete browser `Cookie` header through standard input; do not use `--cookie` or `GEMINI_COOKIE` for a real credential because those may leak through shell history or process environment inspection. `auth login` validates the candidate through a live `/app` bootstrap before atomically installing it. A failed probe preserves the prior `cookies.json`, or leaves it absent on first login.

```bash
sudo -u gemini-bridge -H /usr/local/bin/gemini-bridge \
  --config /etc/gemini-bridge/bridge.toml auth login
sudo -u gemini-bridge -H /usr/local/bin/gemini-bridge \
  --config /etc/gemini-bridge/bridge.toml doctor
curl --fail http://127.0.0.1:8090/healthz
curl http://127.0.0.1:8090/readyz
```

After successful validation, the service stores `cookies.json` under the configured data directory with mode `0600` on Unix. Set `BRIDGE_SECRET` before importing credentials to encrypt cookies at rest. The same secret is required for later starts; losing it makes the file unreadable. Rotate credentials through `auth login`; do not delete durable conversations or media unnecessarily. File permissions and encryption do not protect credentials from a compromised process running as the service user.

When the standalone conversation store creates a new data directory, the
directory is mode `0700` on Unix. The SQLite database and any `-journal`, `-wal`,
or `-shm` sidecars are mode `0600`; opening a database in an existing
caller-owned directory does not alter that directory's permissions.

`/healthz` proves only that the process is serving. `/readyz` can return `503` for unconfigured, stale, flagged, or reauthentication-required sessions. A successful live `doctor` is account/upstream evidence only for the time and environment where it was observed.

## Data layout and retention

The default data root is `$XDG_DATA_HOME/gemini-bridge`, falling back to `~/.local/share/gemini-bridge`:

- `cookies.json` — sensitive Gemini session credentials.
- `bridge.sqlite` — conversation and upstream identifier history.
- `media/` — content-addressed uploaded/generated bytes.
- `media/meta/` — metadata, prompts, model names, and upstream references.

Treat the whole directory as sensitive. Restrict it to the service account and include it in host backup, retention, and privacy policies. New media records receive the configured expiry timestamp once the runtime passes `storage.media_ttl_days` into the media store. TTL marks a record as eligible for deletion; it does not schedule or perform cleanup. Invoke the authenticated `POST /admin/purge` operation according to your retention policy. Stop the service before manual filesystem edits so database, metadata, and content do not diverge.

## Backup and restore

Back up `bridge.sqlite`, `media/`, and `media/meta/` together. Back up `cookies.json` only when the backup has equivalent access controls and the encryption secret is managed separately.

1. Stop the service: `sudo systemctl stop gemini-bridge`.
2. Copy `/var/lib/gemini-bridge` while preserving owner, mode, timestamps, and extended attributes.
3. Start it: `sudo systemctl start gemini-bridge`.
4. Verify both `/healthz` and `/readyz`.

Restore to the same data path and service user. If credentials are stale, replace `cookies.json` through `auth login`; do not discard conversation/media data.

## Verify and install a release

For version `v0.1.0`, the canonical Linux filename is:

```text
gemini-bridge-v0.1.0-x86_64-unknown-linux-musl.tar.gz
```

Download the archive and the canonical `SHA256SUMS` from the same GitHub release, then verify from their directory:

```bash
cd /path/to/downloads
sha256sum -c --ignore-missing SHA256SUMS
tar -xzf gemini-bridge-v0.1.0-x86_64-unknown-linux-musl.tar.gz
```

`SHA256SUMS` also includes `sbom.cdx.json` and `sbom.spdx.json`; `--ignore-missing` verifies only files downloaded locally. Alternatively, use the fail-closed installer (replace the owner and repository):

```bash
./scripts/release/install.sh v0.1.0 \
  https://github.com/OWNER/gemini-bridge/releases/download/v0.1.0
```

Published releases include CycloneDX 1.6 and SPDX 2.3 SBOMs and GitHub artifact/SBOM attestations. With GitHub CLI authenticated, verify provenance:

```bash
gh attestation verify gemini-bridge-v0.1.0-x86_64-unknown-linux-musl.tar.gz \
  --repo OWNER/gemini-bridge
```

## systemd installation

The archive includes `bridge.example.toml` and `deploy/systemd/gemini-bridge.service`.

```bash
sudo useradd --system --home-dir /var/lib/gemini-bridge --shell /usr/sbin/nologin gemini-bridge || true
sudo install -d -m 0750 -o root -g gemini-bridge /etc/gemini-bridge
sudo install -d -m 0700 -o gemini-bridge -g gemini-bridge /var/lib/gemini-bridge
sudo install -m 0755 gemini-bridge-v0.1.0-x86_64-unknown-linux-musl/gemini-bridge /usr/local/bin/gemini-bridge
sudo install -m 0640 -o root -g gemini-bridge \
  gemini-bridge-v0.1.0-x86_64-unknown-linux-musl/bridge.example.toml \
  /etc/gemini-bridge/bridge.toml
sudo install -m 0644 \
  gemini-bridge-v0.1.0-x86_64-unknown-linux-musl/deploy/systemd/gemini-bridge.service \
  /etc/systemd/system/gemini-bridge.service
sudo systemd-analyze verify /etc/systemd/system/gemini-bridge.service
sudo systemctl daemon-reload
sudo systemctl enable --now gemini-bridge
```

Create `/etc/gemini-bridge/environment` as root with mode `0600` for `BRIDGE_SECRET` and `BRIDGE_SERVER_API_KEY`; systemd environment files do not use shell quoting. The unit explicitly sets `BRIDGE_STORAGE_DATA_DIR=/var/lib/gemini-bridge` and creates that writable state directory while keeping the rest of the filesystem read-only.

## Container helper

The `Dockerfile` pins its Rust and Debian bases by immutable multi-platform digest, creates numeric uid/gid `10001`, defines `/healthz` as the image healthcheck, and keeps durable data at `/var/lib/gemini-bridge`.

```bash
docker build --pull -t gemini-bridge:local .
docker volume create gemini-bridge-data
docker run --rm --read-only \
  --tmpfs /tmp:rw,noexec,nosuid,size=16m \
  --cap-drop=ALL --security-opt=no-new-privileges:true \
  -p 127.0.0.1:8090:8090 \
  -v "$PWD/bridge.toml:/etc/gemini-bridge/bridge.toml:ro" \
  -v gemini-bridge-data:/var/lib/gemini-bridge \
  -e BRIDGE_SERVER_BIND_ADDR=0.0.0.0 \
  -e BRIDGE_SERVER_API_KEY \
  -e BRIDGE_SECRET \
  gemini-bridge:local
```

The named volume is initialized through the image's non-root ownership. For a bind mount, create and chown the host directory to uid/gid `10001` first. The runtime container uses glibc; the published archive is the separately verified musl-static binary.

## Exact upgrade and rollback

Never run two versions against the same SQLite/media directory.

```bash
# Before upgrade: preserve the currently installed executable and data.
sudo systemctl stop gemini-bridge
sudo install -m 0755 /usr/local/bin/gemini-bridge /usr/local/bin/gemini-bridge.previous
sudo cp -a /var/lib/gemini-bridge /var/lib/gemini-bridge.rollback

# Install the already checksum-verified new binary atomically.
sudo install -m 0755 gemini-bridge-v0.1.0-x86_64-unknown-linux-musl/gemini-bridge \
  /usr/local/bin/gemini-bridge.new
sudo mv /usr/local/bin/gemini-bridge.new /usr/local/bin/gemini-bridge
sudo systemctl start gemini-bridge
curl --fail http://127.0.0.1:8090/healthz
curl http://127.0.0.1:8090/readyz
```

If validation fails:

```bash
sudo systemctl stop gemini-bridge
sudo install -m 0755 /usr/local/bin/gemini-bridge.previous /usr/local/bin/gemini-bridge.new
sudo mv /usr/local/bin/gemini-bridge.new /usr/local/bin/gemini-bridge
# Restore data only when release notes identify an incompatible migration.
# sudo rm -rf /var/lib/gemini-bridge
# sudo mv /var/lib/gemini-bridge.rollback /var/lib/gemini-bridge
sudo systemctl start gemini-bridge
curl --fail http://127.0.0.1:8090/healthz
```

Do not overwrite data with the rollback copy after a normal binary-only failure; doing so discards requests completed after the backup.

## Live acceptance, KPI, load, and soak evidence

These commands measure an operator-configured service and retain results; they do not imply a pass before actually run. Live Gemini traffic can expose account credentials to risk, incur usage, be throttled, or violate applicable terms. Use a dedicated, expendable account and non-production API key. Never commit cookies, keys, case files, reference images, or result reports.

### Credential-gated live chat/image/tool acceptance

Create a JSON case file **outside the repository** with `base_url`, `model`, and at least 50 `chat`, 20 `image`, and 10 `tool` cases. Chat cases specify `prompt` and may set `stream:false` (otherwise SSE is tested); tool cases specify `prompt`, an OpenAI `tools` array, and optionally `expected_tool`; image cases specify distinct prompts and at least 5 cases with absolute `reference_files` paths to image files outside the repository. A tool suite checks that the result contains valid JSON arguments and declared function names; it does not execute tools. Image checks require recognizable PNG/JPEG/GIF/WebP signature and non-zero dimensions. For images the acceptance threshold is 100%; chat and tool thresholds are 95%; aggregate minimum is 95%. Reference uploads go through the target bridge and Google receives those images.

The exact opt-in and API key are mandatory. The config has no API key field, and must contain enough cases for each selected suite. Use an output path outside the repository and retain that JSON with the commit/tag and test context:

```bash
install -d -m 0700 "$HOME/gemini-bridge-evidence"
cp /secure/live-cases.json "$HOME/gemini-bridge-evidence/live-cases.json"
read -rsp 'Non-production bridge API key: ' LIVE_BRIDGE_API_KEY; export LIVE_BRIDGE_API_KEY; echo
GEMINI_BRIDGE_LIVE=I_ACCEPT_ACCOUNT_RISK \
  ./scripts/live/acceptance.sh "$HOME/gemini-bridge-evidence/live-cases.json" \
  "$HOME/gemini-bridge-evidence/live-results.json" --suite all
unset LIVE_BRIDGE_API_KEY
```

Use `--suite chat`, `--suite image`, or `--suite tool` to run one case group; only that group's minimum case count is required (image still requires five reference cases). The preflight requires both `/healthz` and `/readyz` to return success before any generation. HTTP/API/model failures are retained per case. The JSON result stores case pass state, status, first SSE content-event and total chat latency, tool-call structure, and image dimensions, but never prompts, response content, API key, or reference bytes. Streaming times include Gemini/upstream latency; they are not bridge-only overhead.

### Bridge PID KPIs

This Linux-only measurement starts the specified release binary against an unused loopback port, measures startup to `/healthz` (five runs by default), then samples **that bridge PID's** `/proc/<pid>/status` RSS at idle and during exactly 10 real concurrent streaming chat requests. It also records first-content-event and total stream latency; these include upstream latency and are not overhead-only. The documented limits are cold start ≤150 ms, idle RSS ≤25 MiB, and ten-stream RSS ≤120 MiB. Run only against a credentialed, explicitly configured isolated test service. `--config` must be outside the repository and its bind address/port must match `--base-url`; the config must allow this loopback benchmark without an API key.

```bash
unset BRIDGE_BENCH_API_KEY
./scripts/bench/bridge-kpi.sh \
  --binary target/x86_64-unknown-linux-musl/release/gemini-bridge \
  --config "$HOME/gemini-bridge-evidence/kpi-test.toml" \
  --base-url http://127.0.0.1:8090 \
  --output "$HOME/gemini-bridge-evidence/kpi.json"
```

The KPI runner deliberately fails if the target port is occupied, the bridge exits, health/readiness do not become successful, or a prerequisite is absent. It uses the real process PID for RSS. It is Linux `/proc`-specific; cold-start values are workstation/build/environment measurements, not release-wide claims. Bridge-only p50/p95 overhead cannot be measured through the public endpoint: E3's required upstream-chunk timestamp (`t_emit - t_upstream_chunk`) needs internal instrumentation and remains open. The generic `http-kpi.sh` remains an endpoint load tool; its runner RSS is **not** bridge RSS.

### Chat load and chat-success soak

Chat load requires a live configured model and a non-production key. Health/readiness alone are not chat success. The soak driver now periodically sends a real non-stream chat completion, records health/readiness/chat statuses, response-validation success, and curl total latency; `SOAK_MIN_CHAT` defaults to 99%. Test a short run first. The seven-day run is explicitly not started by this work:

```bash
read -rsp 'Non-production bridge API key: ' BRIDGE_BENCH_API_KEY; export BRIDGE_BENCH_API_KEY; echo
export SOAK_MODEL=gemini-web-flash
SOAK_MIN_CHAT=100 ./scripts/bench/soak-driver.sh \
  http://127.0.0.1:8090 30 5 "$HOME/gemini-bridge-evidence/soak-smoke.csv"
# After reviewing a suitable isolated staging setup, the operator may run:
# SOAK_MIN_CHAT=99 nohup ./scripts/bench/soak-driver.sh \
#   http://127.0.0.1:8090 604800 60 "$HOME/gemini-bridge-evidence/soak-seven-day.csv" \
#   >"$HOME/gemini-bridge-evidence/soak-seven-day.log" 2>&1 &
unset BRIDGE_BENCH_API_KEY SOAK_MODEL
```

A credible report must retain machine-readable output/CSV, thresholds, exact commit/tag, host/container limits, start/end time, config (with secrets redacted), upstream/account context, and every interruption. A command start, mock response, health check, or short smoke is not evidence that live acceptance, KPIs, or seven days passed. The release build/package/smoke command remains `./scripts/release/build.sh v0.1.0` and requires musl prerequisites.

## Observability and troubleshooting

Logs are JSON and include request audit records. Known credential patterns are redacted, but operators must still restrict log access and retention.

- Process unhealthy: inspect `journalctl -u gemini-bridge` and bind conflicts.
- `/readyz` says `unconfigured`: import credentials as the service user.
- `/readyz` says `needs_reauth`: import a fresh browser cookie and run `doctor`.
- `/readyz` says `ip_flagged`: stop retry loops and review the configured proxy/IP path.
- Upstream schema failure: update only to a release containing a reviewed schema change.
- Disk growth: inspect `media/` and metadata; use gallery/admin deletion instead of deleting content files alone.

The release workflow builds, verifies, attests, and publishes artifacts for version tags. It does not deploy a host or container environment.
