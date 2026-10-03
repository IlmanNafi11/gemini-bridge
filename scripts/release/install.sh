#!/usr/bin/env bash
set -euo pipefail

# Download, verify, and install a release archive. Usage:
#   scripts/release/install.sh VERSION BASE_URL [PREFIX]
# BASE_URL is a release download directory such as
# https://github.com/IlmanNafi11/gemini-bridge/releases/download/v0.1.0
# PREFIX is accepted for explicitness but must be /usr/local because the bundled
# hardened systemd unit intentionally uses /usr/local/bin/gemini-bridge.

[[ $# -ge 2 && $# -le 3 ]] || {
  echo "Usage: scripts/release/install.sh VERSION BASE_URL [PREFIX]" >&2
  exit 64
}
version=$1
base_url=${2%/}
prefix=${3:-/usr/local}
target=x86_64-unknown-linux-musl
package="gemini-bridge-${version}-${target}"
archive="${package}.tar.gz"
script_dir=$(cd "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)

[[ "$version" =~ ^v?[0-9A-Za-z][0-9A-Za-z._+-]*$ ]] || {
  echo "error: VERSION contains unsupported characters: $version" >&2
  exit 64
}
[[ "$prefix" == /usr/local ]] || {
  echo "error: PREFIX must be /usr/local to match the bundled systemd unit" >&2
  exit 64
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for tool in curl sha256sum tar sudo; do
  command -v "$tool" >/dev/null 2>&1 || { echo "error: $tool is required" >&2; exit 69; }
done

for payload in SHA256SUMS "$archive" sbom.cdx.json sbom.spdx.json; do
  curl --fail --location --silent --show-error "$base_url/$payload" -o "$tmp/$payload"
done
(
  cd "$tmp"
  sha256sum --check --strict SHA256SUMS
)
"$script_dir/verify-archive.sh" "$tmp/$archive" "$tmp/SHA256SUMS"
tar --no-same-owner --no-same-permissions -C "$tmp" -xzf "$tmp/$archive"

sudo install -d -m 0755 "$prefix/bin"
sudo install -d -m 0755 "$prefix/share/gemini-bridge"
sudo install -d -m 0755 "$prefix/lib/systemd/system"
sudo install -m 0644 "$tmp/$package/CHANGELOG.md" "$prefix/share/gemini-bridge/CHANGELOG.md"
sudo install -m 0644 "$tmp/$package/bridge.example.toml" "$prefix/share/gemini-bridge/bridge.example.toml"
sudo install -m 0755 "$tmp/$package/gemini-bridge" "$prefix/bin/gemini-bridge"
sudo install -m 0644 "$tmp/$package/deploy/systemd/gemini-bridge.service" \
  "$prefix/lib/systemd/system/gemini-bridge.service"
echo "installed gemini-bridge from $archive into $prefix"
