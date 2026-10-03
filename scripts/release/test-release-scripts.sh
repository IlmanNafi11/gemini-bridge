#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

version=v9.8.7
archive_name="gemini-bridge-${version}-x86_64-unknown-linux-musl.tar.gz"
mkdir -p "$work/good" "$work/evil/gemini-bridge-${version}-x86_64-unknown-linux-musl"
SOURCE_DATE_EPOCH=1 "$root/scripts/release/package.sh" "$version" /bin/true "$work/good"
"$root/scripts/release/verify-archive.sh" "$work/good/$archive_name" "$work/good/SHA256SUMS"

# The full manifest is checked, not just the archive's line.
printf 'original\n' > "$work/good/sbom.cdx.json"
printf 'original\n' > "$work/good/sbom.spdx.json"
(cd "$work/good" && sha256sum "$archive_name" sbom.cdx.json sbom.spdx.json > SHA256SUMS)
printf 'tampered\n' >> "$work/good/sbom.cdx.json"
if "$root/scripts/release/verify-archive.sh" "$work/good/$archive_name" "$work/good/SHA256SUMS" >/dev/null 2>&1; then
  echo 'error: verifier accepted a modified SBOM from the manifest' >&2
  exit 1
fi

# Checksum-valid archives with traversal, absolute, or merely extra members fail.
for kind in traversal absolute extra; do
  rm -rf "$work/evil/content"
  mkdir -p "$work/evil/content/gemini-bridge-${version}-x86_64-unknown-linux-musl"
  cp "$root/README.md" "$work/evil/content/gemini-bridge-${version}-x86_64-unknown-linux-musl/README.md"
  printf unsafe > "$work/evil/content/member"
  case "$kind" in
    traversal)
      tar -czf "$work/evil/$archive_name" -C "$work/evil/content" \
        --transform='s|^member$|../escape|' member
      ;;
    absolute)
      tar -czf "$work/evil/$archive_name" -C "$work/evil/content" \
        --transform='s|^member$|/tmp/absolute|' member
      ;;
    extra)
      mv "$work/evil/content/member" "$work/evil/content/extra"
      tar -czf "$work/evil/$archive_name" -C "$work/evil/content" .
      ;;
  esac
  (cd "$work/evil" && sha256sum "$archive_name" > SHA256SUMS)
  if "$root/scripts/release/verify-archive.sh" "$work/evil/$archive_name" "$work/evil/SHA256SUMS" >/dev/null 2>&1; then
    echo "error: verifier accepted a $kind archive" >&2
    exit 1
  fi
done

# Prefix mismatch is rejected before any download or privileged install.
if "$root/scripts/release/install.sh" "$version" https://invalid.example/releases /tmp/custom-prefix >"$work/install.out" 2>&1; then
  echo 'error: installer accepted a prefix inconsistent with systemd ExecStart' >&2
  exit 1
fi
grep -q 'PREFIX must be /usr/local' "$work/install.out"

# The installer rejects a checksum-valid traversal archive before extraction or sudo.
mkdir -p "$work/payload" "$work/malicious/gemini-bridge-${version}-x86_64-unknown-linux-musl" "$work/fake-bin"
printf sbom > "$work/payload/sbom.cdx.json"
printf sbom > "$work/payload/sbom.spdx.json"
printf unsafe > "$work/malicious/member"
cp "$root/README.md" "$work/malicious/gemini-bridge-${version}-x86_64-unknown-linux-musl/README.md"
tar -czf "$work/payload/$archive_name" -C "$work/malicious" \
  --transform='s|^member$|../escape|' \
  "gemini-bridge-${version}-x86_64-unknown-linux-musl/README.md" member
(cd "$work/payload" && sha256sum "$archive_name" sbom.cdx.json sbom.spdx.json > SHA256SUMS)
cat > "$work/fake-bin/curl" <<'CURL'
#!/usr/bin/env bash
set -euo pipefail
while (($#)); do
  if [[ "$1" == -o ]]; then output=$2; shift 2; else url=$1; shift; fi
done
cp "$FIXTURE_DIR/$(basename "$url")" "$output"
CURL
cat > "$work/fake-bin/sudo" <<'SUDO'
#!/usr/bin/env bash
touch "$SUDO_MARKER"
SUDO
cat > "$work/fake-bin/tar" <<'TAR'
#!/usr/bin/env bash
for arg in "$@"; do [[ "$arg" != -xzf ]] || touch "$EXTRACT_MARKER"; done
exec /bin/tar "$@"
TAR
chmod +x "$work/fake-bin/"*
export FIXTURE_DIR="$work/payload"
export SUDO_MARKER="$work/sudo-called"
export EXTRACT_MARKER="$work/extracted"
export PATH="$work/fake-bin:$PATH"
if "$root/scripts/release/install.sh" "$version" https://fixture.invalid/releases >"$work/install-malicious.out" 2>&1; then
  echo 'error: installer accepted a checksum-valid traversal archive' >&2
  exit 1
fi
[[ ! -e "$EXTRACT_MARKER" && ! -e "$SUDO_MARKER" ]] || {
  echo 'error: installer extracted or installed an invalid archive' >&2
  exit 1
}

echo 'release script smoke tests passed'
