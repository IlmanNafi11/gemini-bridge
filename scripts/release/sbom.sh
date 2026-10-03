#!/usr/bin/env bash
set -euo pipefail

# Create release CycloneDX and SPDX SBOMs and the canonical SHA256SUMS.
# Usage: scripts/release/sbom.sh OUTPUT_DIR ARCHIVE
# Requires cargo-sbom; pin and install it with:
#   cargo install --locked --version 0.10.0 cargo-sbom

[[ $# -eq 2 ]] || {
  echo "Usage: scripts/release/sbom.sh OUTPUT_DIR ARCHIVE" >&2
  exit 64
}
output_dir=$1
archive=$2
[[ -d "$output_dir" ]] || { echo "error: output directory not found: $output_dir" >&2; exit 66; }
[[ -f "$archive" ]] || { echo "error: release archive not found: $archive" >&2; exit 66; }
if ! cargo sbom --version >/dev/null 2>&1; then
  echo "error: cargo-sbom is required; install with: cargo install --locked --version 0.10.0 cargo-sbom" >&2
  exit 69
fi

cargo sbom --output-format cyclone_dx_json_1_6 > "$output_dir/sbom.cdx.json"
cargo sbom --output-format spdx_json_2_3 > "$output_dir/sbom.spdx.json"
[[ -s "$output_dir/sbom.cdx.json" && -s "$output_dir/sbom.spdx.json" ]] || {
  echo "error: cargo-sbom produced an empty SBOM" >&2
  exit 70
}

archive_name=$(basename "$archive")
(
  cd "$output_dir"
  LC_ALL=C sha256sum "$archive_name" sbom.cdx.json sbom.spdx.json | LC_ALL=C sort -k2 > SHA256SUMS
)
echo "wrote $output_dir/sbom.cdx.json, $output_dir/sbom.spdx.json, and $output_dir/SHA256SUMS"
echo "note: cargo-sbom includes its generated timestamp/UUID; SBOM bytes are run-specific" >&2
