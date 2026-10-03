#!/usr/bin/env bash
set -euo pipefail

# Opt-in only; the Python runner enforces the live-risk acknowledgement and
# external config/output paths before contacting the configured bridge.
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec python3 "$script_dir/acceptance.py" "$@"
