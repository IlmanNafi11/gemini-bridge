#!/usr/bin/env bash
set -euo pipefail

# Verify the complete checksum manifest, then enforce the exact canonical archive
# layout before any caller is allowed to extract it.
# Usage: scripts/release/verify-archive.sh ARCHIVE [SHA256SUMS]

[[ $# -ge 1 && $# -le 2 ]] || {
  echo "Usage: scripts/release/verify-archive.sh ARCHIVE [SHA256SUMS]" >&2
  exit 64
}

archive=$1
manifest=${2:-}
[[ -f "$archive" ]] || { echo "error: archive not found: $archive" >&2; exit 66; }
archive_name=$(basename -- "$archive")
root=${archive_name%.tar.gz}
[[ "$archive_name" == "$root.tar.gz" && "$root" =~ ^gemini-bridge-[0-9A-Za-z][0-9A-Za-z._+-]*-x86_64-unknown-linux-musl$ ]] || {
  echo "error: archive name is not canonical: $archive_name" >&2
  exit 70
}

if [[ -n "$manifest" ]]; then
  [[ -f "$manifest" ]] || { echo "error: checksum manifest not found: $manifest" >&2; exit 66; }
  archive_dir=$(cd "$(dirname -- "$archive")" && pwd -P)
  manifest_dir=$(cd "$(dirname -- "$manifest")" && pwd -P)
  manifest_absolute="$manifest_dir/$(basename -- "$manifest")"
  [[ -f "$manifest_absolute" ]] || { echo "error: checksum manifest not found: $manifest" >&2; exit 66; }
  [[ "$archive_dir" == "$manifest_dir" ]] || {
    echo "error: archive and checksum manifest must be in the same directory" >&2
    exit 64
  }
  manifest_name=$(basename -- "$manifest")
  mapfile -t manifest_lines < "$manifest_absolute"
  [[ ${#manifest_lines[@]} -eq 1 || ${#manifest_lines[@]} -eq 3 ]] || {
    echo "error: checksum manifest must list the archive alone or archive plus both SBOMs" >&2
    exit 70
  }
  declare -A seen_manifest_entries=()
  for line in "${manifest_lines[@]}"; do
    [[ "$line" =~ ^([0-9a-f]{64})\ \ (.+)$ ]] || {
      echo "error: malformed checksum manifest entry" >&2
      exit 70
    }
    entry_name=${BASH_REMATCH[2]}
    case "$entry_name" in
      "$archive_name"|sbom.cdx.json|sbom.spdx.json) ;;
      *) echo "error: unexpected checksum manifest path: $entry_name" >&2; exit 70 ;;
    esac
    [[ -z "${seen_manifest_entries[$entry_name]+present}" ]] || {
      echo "error: duplicate checksum manifest entry: $entry_name" >&2
      exit 70
    }
    seen_manifest_entries[$entry_name]=1
  done
  [[ -n "${seen_manifest_entries[$archive_name]+present}" ]] || {
    echo "error: checksum manifest does not include $archive_name" >&2
    exit 70
  }
  if [[ ${#manifest_lines[@]} -eq 3 ]]; then
    [[ -n "${seen_manifest_entries[sbom.cdx.json]+present}" && \
      -n "${seen_manifest_entries[sbom.spdx.json]+present}" ]] || {
      echo "error: checksum manifest must include both SBOM files" >&2
      exit 70
    }
  fi
  (
    cd "$manifest_dir"
    sha256sum --check --strict "$manifest_name"
  )
fi

members_output=$(tar -tzf "$archive")
verbose_output=$(tar -tvzf "$archive")
mapfile -t members <<<"$members_output"
mapfile -t verbose_members <<<"$verbose_output"
expected=(
  "$root/"
  "$root/CHANGELOG.md"
  "$root/README.md"
  "$root/LICENSE"
  "$root/bridge.example.toml"
  "$root/gemini-bridge"
  "$root/docs/"
  "$root/docs/operations.md"
  "$root/deploy/"
  "$root/deploy/systemd/"
  "$root/deploy/systemd/gemini-bridge.service"
)
[[ ${#members[@]} -eq ${#expected[@]} && ${#verbose_members[@]} -eq ${#expected[@]} ]] || {
  echo "error: archive must contain exactly the canonical release files and directories" >&2
  exit 70
}
for index in "${!members[@]}"; do
  member=${members[$index]}
  allowed=false
  for required in "${expected[@]}"; do
    [[ "$member" == "$required" ]] && allowed=true
  done
  [[ "$allowed" == true ]] || {
    echo "error: archive contains an unexpected or unsafe path: $member" >&2
    exit 70
  }
  mode=${verbose_members[$index]:0:1}
  case "$member" in
    */) [[ "$mode" == d ]] || { echo "error: archive directory has unsafe type: $member" >&2; exit 70; } ;;
    *) [[ "$mode" == - ]] || { echo "error: archive file has unsafe type: $member" >&2; exit 70; } ;;
  esac
done
for required in "${expected[@]}"; do
  found=0
  for member in "${members[@]}"; do [[ "$member" == "$required" ]] && found=$((found + 1)); done
  [[ "$found" -eq 1 ]] || { echo "error: archive is missing or duplicates required path: $required" >&2; exit 70; }
done

echo "verified: $archive_name ($(stat -c '%s' "$archive") bytes)"
