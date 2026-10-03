#!/usr/bin/env bash
set -euo pipefail

[[ $# -eq 5 ]] || {
  echo "Usage: scripts/bench/chat-load.sh BASE_URL MODEL REQUESTS CONCURRENCY OUTPUT.csv" >&2
  exit 64
}

base_url=${1%/}
model=$2
requests=$3
concurrency=$4
output=$5
[[ "$requests" =~ ^[1-9][0-9]*$ && "$concurrency" =~ ^[1-9][0-9]*$ ]] || {
  echo "error: REQUESTS and CONCURRENCY must be positive integers" >&2
  exit 64
}
if [[ "$base_url" != http://* && "$base_url" != https://* ]]; then
  echo "error: BASE_URL must be an http(s) URL" >&2; exit 64
fi
for tool in curl jq; do
  command -v "$tool" >/dev/null 2>&1 || { echo "error: $tool is required" >&2; exit 69; }
done
for endpoint in healthz readyz; do
  status=$(curl --silent --show-error --max-time 10 -o /dev/null -w '%{http_code}' \
    -H "Authorization: Bearer $BRIDGE_BENCH_API_KEY" "$base_url/$endpoint" 2>/dev/null || true)
  [[ "$status" == 2* ]] || { echo "error: target $endpoint unavailable (HTTP ${status:-000}); load not started" >&2; exit 69; }
done

payload=$(jq -cn --arg model "$model" '{model:$model,messages:[{role:"user",content:"Reply with the single word OK."}],stream:false}')
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export BRIDGE_BENCH_BASE_URL="$base_url" BRIDGE_BENCH_MODEL="$model" BRIDGE_BENCH_PAYLOAD="$payload"
worker() {
  index=$1
  result=$(curl --silent --show-error --max-time "${BRIDGE_BENCH_TIMEOUT_SECONDS:-60}" \
    --output "$work/$index.body" --write-out '%{time_total},%{http_code}' \
    -H "Authorization: Bearer $BRIDGE_BENCH_API_KEY" \
    -H 'Content-Type: application/json' \
    --data "$BRIDGE_BENCH_PAYLOAD" "$BRIDGE_BENCH_BASE_URL/v1/chat/completions" \
    2>/dev/null || printf '0,000')
  printf '%s\n' "$result" > "$work/$index.csv"
  status=${result##*,}
  if [[ "$status" == 2* ]] && jq -e '.choices[0].message.content | strings | select(length > 0)' "$work/$index.body" >/dev/null 2>&1; then
    printf '1\n' > "$work/$index.success"
  else
    printf '0\n' > "$work/$index.success"
  fi
}
export work

next=0
active=()
while (( next < requests || ${#active[@]} > 0 )); do
  while (( next < requests && ${#active[@]} < concurrency )); do
    worker "$next" &
    active+=("$!")
    ((next+=1))
  done
  pid=${active[0]}
  wait "$pid" || true
  active=("${active[@]:1}")
done

printf 'latency_seconds,http_status,chat_success\n' > "$output"
for result in "$work"/*.csv; do
  index=${result##*/}; index=${index%.csv}
  printf '%s,%s\n' "$(cat "$result")" "$(cat "$work/$index.success")" >> "$output"
done
awk -F, 'NR>1 {n++; if ($3 == 1) ok++; else fail++; lat[n]=$1} END {
  for(i=1;i<=n;i++) for(j=i+1;j<=n;j++) if(lat[j]<lat[i]){t=lat[i];lat[i]=lat[j];lat[j]=t}
  if(n) printf "requests=%d chat_success=%d failed=%d p50_s=%.4f p95_s=%.4f p99_s=%.4f\n",n,ok,fail,lat[int((n-1)*.5)+1],lat[int((n-1)*.95)+1],lat[int((n-1)*.99)+1]
  if(!n || fail > 0) exit 2
}' "$output"
echo "csv=$output"
