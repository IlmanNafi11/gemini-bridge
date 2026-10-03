#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/release/package.sh VERSION BINARY [OUTPUT_DIR]

Create the canonical Linux musl release archive and SHA256SUMS manifest.
VERSION is normally a tag such as v0.1.0. BINARY must be an executable file.
USAGE
}

[[ $# -ge 2 && $# -le 3 ]] || { usage; exit 64; }

version=$1
binary=$2
output_dir=${3:-release}
target=x86_64-unknown-linux-musl
package="gemini-bridge-${version}-${target}"
archive="${package}.tar.gz"

[[ "$version" =~ ^v?[0-9A-Za-z][0-9A-Za-z._+-]*$ ]] || {
  echo "error: VERSION contains unsupported characters: $version" >&2
  exit 64
}
[[ -f "$binary" && -x "$binary" ]] || {
  echo "error: release binary is missing or not executable: $binary" >&2
  exit 66
}
for required in README.md CHANGELOG.md LICENSE bridge.example.toml docs/operations.md deploy/systemd/gemini-bridge.service; do
  [[ -f "$required" ]] || { echo "error: required package input is missing: $required" >&2; exit 66; }
done

source_date_epoch=${SOURCE_DATE_EPOCH:-}
if [[ -z "$source_date_epoch" ]]; then
  source_date_epoch=$(git log -1 --format=%ct 2>/dev/null || true)
fi
[[ "$source_date_epoch" =~ ^[0-9]+$ ]] || {
  echo "error: SOURCE_DATE_EPOCH must be a non-negative integer" >&2
  exit 64
}
umask 022
mkdir -p "$output_dir"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/$package/deploy/systemd" "$tmp/$package/docs"
install -m 0755 "$binary" "$tmp/$package/gemini-bridge"
install -m 0644 README.md CHANGELOG.md LICENSE bridge.example.toml "$tmp/$package/"
install -m 0644 docs/operations.md "$tmp/$package/docs/operations.md"
install -m 0644 deploy/systemd/gemini-bridge.service \
  "$tmp/$package/deploy/systemd/gemini-bridge.service"

# Fixed ownership, ordering, and timestamps make repeated packaging byte-identical.
tar --sort=name \
  --mtime="@$source_date_epoch" \
  --owner=0 --group=0 --numeric-owner \
  -C "$tmp" -cf - "$package" \
  | gzip -n -9 > "$output_dir/$archive"
(
  cd "$output_dir"
  sha256sum "$archive" > SHA256SUMS
)
