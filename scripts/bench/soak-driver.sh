#!/usr/bin/env bash
set -euo pipefail

# Live soak probe exercises chat, health, and readiness; no credential is stored.
# Usage: scripts/bench/soak-driver.sh BASE_URL DURATION_SECONDS SAMPLE_SECONDS CSV
[[ $# -eq 4 ]] || {
  echo "Usage: scripts/bench/soak-driver.sh BASE_URL DURATION_SECONDS SAMPLE_SECONDS CSV" >&2
  exit 64
}
base_url=${1%/}
duration=$2
sample=$3
csv=$4
: "${BRIDGE_BENCH_API_KEY:?error: BRIDGE_BENCH_API_KEY must contain a non-production test API key}"
: "${SOAK_MODEL:?error: SOAK_MODEL must name the configured bridge model}"
for tool in curl awk date jq; do
  command -v "$tool" >/dev/null 2>&1 || { echo "error: $tool is required" >&2; exit 69; }
done
[[ "$duration" =~ ^[0-9]+$ && "$sample" =~ ^[1-9][0-9]*$ ]] || {
  echo "error: DURATION_SECONDS must be a non-negative integer and SAMPLE_SECONDS positive" >&2
  exit 64
}

if [[ "$base_url" != http://* && "$base_url" != https://* ]]; then
  echo "error: BASE_URL must be an http(s) URL" >&2; exit 64
fi
if ! [[ "${SOAK_MIN_CHAT:-99}" =~ ^[0-9]+([.][0-9]+)?$ ]] || \
  ! awk -v p="${SOAK_MIN_CHAT:-99}" 'BEGIN { exit !(p >= 0 && p <= 100) }'; then
  echo "error: SOAK_MIN_CHAT must be a percentage from 0 to 100" >&2; exit 64
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for endpoint in healthz readyz; do
  code=$(curl --silent --show-error --max-time 10 -o "$work/preflight" -w '%{http_code}' \
    -H "Authorization: Bearer $BRIDGE_BENCH_API_KEY" "$base_url/$endpoint" 2>/dev/null || true)
  [[ "$code" == 2* ]] || { echo "error: target $endpoint unavailable (HTTP ${code:-000}); soak not started" >&2; exit 69; }
done
payload=$(jq -cn --arg model "$SOAK_MODEL" '{model:$model,messages:[{role:"user",content:"Reply with the single word OK."}],stream:false}')
printf 'timestamp,health_status,readiness_status,chat_status,chat_success,chat_latency_seconds,uptime_seconds\n' > "$csv"
chat_success=0
samples=0
deadline=$(( $(date +%s) + duration ))
while (( $(date +%s) < deadline )); do
  ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  health_code=$(curl --silent --show-error --max-time 10 -o "$work/health.json" -w '%{http_code}' \
    -H "Authorization: Bearer $BRIDGE_BENCH_API_KEY" "$base_url/healthz" 2>/dev/null || echo 000)
  readiness_code=$(curl --silent --show-error --max-time 10 -o "$work/readiness.json" -w '%{http_code}' \
    -H "Authorization: Bearer $BRIDGE_BENCH_API_KEY" "$base_url/readyz" 2>/dev/null || echo 000)
  chat_result=$(curl --silent --show-error --max-time "${SOAK_CHAT_TIMEOUT_SECONDS:-120}" \
    --output "$work/chat.json" --write-out '%{time_total},%{http_code}' \
    -H "Authorization: Bearer $BRIDGE_BENCH_API_KEY" \
    -H 'Content-Type: application/json' --data "$payload" "$base_url/v1/chat/completions" 2>/dev/null || printf '0,000')
  IFS=, read -r chat_latency chat_code <<< "$chat_result"
  chat_code=${chat_code:-000}
  chat_latency=${chat_latency:-0}
  # A real request response must contain a non-empty assistant message.
  if [[ "$chat_code" == 2* ]] && jq -e '.choices[0].message.content | strings | select(length > 0)' "$work/chat.json" >/dev/null 2>&1; then
    success=1
    ((chat_success+=1))
  else
    success=0
  fi
  uptime=$(jq -r '.uptime_secs // 0' "$work/health.json" 2>/dev/null || echo 0)
  printf '%s,%s,%s,%s,%s,%s,%s\n' "$ts" "${health_code:-000}" "${readiness_code:-000}" "${chat_code:-000}" "$success" "$chat_latency" "$uptime" >> "$csv"
  ((samples+=1))
  sleep "$sample"
done
(( samples > 0 )) || { echo "error: no soak samples collected; duration must exceed zero" >&2; exit 1; }
awk -F, -v min_chat="${SOAK_MIN_CHAT:-99}" '
  NR>1 { n++; if ($5 == 1) ok++; if ($2 !~ /^2/ || $3 !~ /^2/) unhealthy++ }
  END {
    if (!n) { print "error: no soak samples collected" > "/dev/stderr"; exit 1 }
    cp=ok*100/n
    printf "samples=%d chat_success=%d chat_success_percent=%.3f%% health_or_readiness_failures=%d min_chat=%.3f%%\n", n, ok, cp, unhealthy, min_chat
    if (cp < min_chat || unhealthy > 0) {
      print "error: soak threshold or health/readiness requirement not met" > "/dev/stderr"
      exit 2
    }
  }' "$csv"
echo "csv=$csv"
