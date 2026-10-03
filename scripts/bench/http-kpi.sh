#!/usr/bin/env bash
set -euo pipefail

# Lightweight deterministic HTTP endpoint runner. It records latency,
# throughput, and failures for a local mock or explicitly configured target.
# Usage: scripts/bench/http-kpi.sh URL [REQUESTS] [CONCURRENCY] [OUTPUT.csv]
# Requires curl and awk. Never sends credentials unless the operator explicitly
# provides KPI_AUTH_HEADER (for example, a test API key).

[[ $# -ge 1 && $# -le 4 ]] || {
  echo "Usage: scripts/bench/http-kpi.sh URL [REQUESTS=1000] [CONCURRENCY=10] [OUTPUT.csv]" >&2
  exit 64
}
url=$1
requests=${2:-1000}
concurrency=${3:-10}
output=${4:-kpi-results.csv}

for tool in curl awk; do
  command -v "$tool" >/dev/null 2>&1 || { echo "error: $tool is required" >&2; exit 69; }
done
[[ "$requests" =~ ^[1-9][0-9]*$ && "$concurrency" =~ ^[1-9][0-9]*$ ]] || {
  echo "error: REQUESTS and CONCURRENCY must be positive integers" >&2
  exit 64
}
if [[ -n "${KPI_AUTH_HEADER:-}" ]]; then
  case "$KPI_AUTH_HEADER" in *$'\n'*|*$'\r'*) echo "error: KPI_AUTH_HEADER must be one line" >&2; exit 64;; esac
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export KPI_URL="$url" KPI_AUTH_HEADER="${KPI_AUTH_HEADER:-}"
worker() {
  index=$1
  header_args=()
  [[ -z "$KPI_AUTH_HEADER" ]] || header_args=(-H "$KPI_AUTH_HEADER")
  curl --silent --show-error --output /dev/null --write-out '%{time_total},%{http_code}\n' \
    "${header_args[@]}" "$KPI_URL" 2>/dev/null || printf '0,000\n'
}
export -f worker

start=$(date +%s)
next=0
active=()
while (( next < requests || ${#active[@]} > 0 )); do
  while (( next < requests && ${#active[@]} < concurrency )); do
    worker "$next" > "$work/$next.csv" &
    active+=("$!")
    ((next+=1))
  done
  pid=${active[0]}
  wait "$pid" || true
  active=("${active[@]:1}")
done

printf 'latency_seconds,http_status\n' > "$output"
cat "$work"/*.csv >> "$output"
end=$(date +%s)
elapsed=$((end - start))
(( elapsed > 0 )) || elapsed=1
awk -F, -v elapsed="$elapsed" '
  NR > 1 { n++; lat[n]=$1; if ($2 ~ /^2/) ok++; else fail++ }
  END {
    if (!n) { print "error: no request results collected" > "/dev/stderr"; exit 1 }
    for (i=1; i<=n; i++) for (j=i+1; j<=n; j++) if (lat[j] < lat[i]) { t=lat[i]; lat[i]=lat[j]; lat[j]=t }
    p50=lat[int((n-1)*0.50)+1]; p95=lat[int((n-1)*0.95)+1]; p99=lat[int((n-1)*0.99)+1]
    printf "requests=%d success=%d failed=%d throughput_rps=%.2f p50_s=%.4f p95_s=%.4f p99_s=%.4f wall_s=%d\n", n, ok, fail, n/elapsed, p50, p95, p99, elapsed
    if (fail > 0) exit 2
  }
' "$output"
echo "csv=$output"
