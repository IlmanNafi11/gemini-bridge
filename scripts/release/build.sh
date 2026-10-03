#!/usr/bin/env bash
set -euo pipefail

# Build the static musl release binary and smoke-test it.
# Usage: scripts/release/build.sh [VERSION]
# VERSION defaults to the current Git tag when present, otherwise "manual".

if [[ -n "${1:-}" ]]; then
  version=$1
elif [[ "${GITHUB_EVENT_NAME:-}" == push ]]; then
  version=$GITHUB_REF_NAME
else
  version=$(git describe --tags --exact-match 2>/dev/null || echo manual)
fi
target=x86_64-unknown-linux-musl
binary="target/$target/release/gemini-bridge"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required; install Rust at https://rustup.rs" >&2
  exit 69
fi
if ! rustup target list --installed 2>/dev/null | grep -qx "$target"; then
  echo "error: missing installed Rust target $target; run: rustup target add $target" >&2
  exit 69
fi
if ! command -v musl-gcc >/dev/null 2>&1; then
  echo "error: musl-gcc is required; install musl-tools (Debian/Ubuntu) or equivalent" >&2
  exit 69
fi

cargo build --locked --release --target "$target" --bin gemini-bridge

file_output=$(file "$binary")
echo "$file_output"
grep -Eq 'statically linked|static-pie linked' <<<"$file_output" || {
  echo "error: release binary is not statically linked" >&2
  exit 70
}
size=$(stat -c '%s' "$binary")
echo "binary: $binary ($size bytes)"
[[ $size -le 26214400 ]] || {
  echo "error: binary exceeds the 25 MiB release gate" >&2
  exit 70
}

GEMINI_BRIDGE_SMOKE_BINARY="$binary" \
  cargo test --locked --release --test release_smoke_test -- --nocapture

./scripts/release/package.sh "$version" "$binary"
archive="release/gemini-bridge-${version}-${target}.tar.gz"
./scripts/release/verify-archive.sh "$archive" release/SHA256SUMS

extract_dir=$(mktemp -d)
trap 'rm -rf "$extract_dir"' EXIT
tar --no-same-owner --no-same-permissions -C "$extract_dir" -xzf "$archive"
packaged_binary="$extract_dir/gemini-bridge-${version}-${target}/gemini-bridge"
GEMINI_BRIDGE_SMOKE_BINARY="$packaged_binary" \
  cargo test --locked --release --test release_smoke_test -- --nocapture
