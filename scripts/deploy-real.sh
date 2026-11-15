#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

hl=target/release/hl
scratch=output/deploy-real
providers=()
if [ -z "${CLOUDFLARE_API_TOKEN:-}" ] && [ -n "${CLOUDFLARE_ACCOUNT_ID:-}" ] && command -v wrangler > /dev/null; then
  CLOUDFLARE_API_TOKEN=$(wrangler auth token 2> /dev/null | tail -n 1 || true)
  export CLOUDFLARE_API_TOKEN
  [ -z "$CLOUDFLARE_API_TOKEN" ] || echo "deploy real: cloudflare token from wrangler auth token"
fi
if [ -n "${CLOUDFLARE_API_TOKEN:-}" ] && [ -n "${CLOUDFLARE_ACCOUNT_ID:-}" ]; then
  providers+=(cloudflare)
else
  echo "deploy real: cloudflare skipped: CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID are not both set"
fi
if [ -n "${VERCEL_TOKEN:-}" ]; then
  providers+=(vercel)
else
  echo "deploy real: vercel skipped: VERCEL_TOKEN is not set"
fi
[ "${#providers[@]}" -gt 0 ] || exit 0
[ -x "$hl" ] || cargo build --release --locked
mkdir -p "$scratch"
unset HL_CLOUDFLARE_API HL_VERCEL_API HL_DEPLOY_NOW_MS HL_DEPLOY_POLL_MS
suffix=$(date +%s)
deployed=()

fail() {
  echo "deploy real: $*" >&2
  exit 1
}

cleanup() {
  local target
  for target in "${deployed[@]}"; do
    "$hl" destroy --target "$target" --name "hl-drill-$target-$suffix" --out "$scratch/$target" --yes || true
  done
}
trap cleanup EXIT

answers() {
  local url=$1 code
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 30 "$url/")
  [ "$code" != 000 ] || return 1
  echo "$code"
}

for target in "${providers[@]}"; do
  name=hl-drill-$target-$suffix
  common=(--target "$target" --name "$name" --out "$scratch/$target")
  log=$scratch/$target.log
  : > "$log"
  deployed+=("$target")
  "$hl" deploy "${common[@]}" examples/minimal.hl | tee -a "$log"
  url=$(sed -n 's/^  url //p' "$log" | tail -n 1)
  code=
  for attempt in $(seq 1 30); do
    code=$(answers "$url") && break
    sleep 2
  done
  [ -n "$code" ] || fail "$target: $url does not answer after $attempt tries"
  echo "deploy real: $target: $url answers HTTP $code"
  echo drill-secret-value | "$hl" secrets set HL_TOKEN "${common[@]}" | tee -a "$log"
  "$hl" secrets list "${common[@]}" | grep -qx HL_TOKEN || fail "$target: HL_TOKEN is not listed"
  "$hl" deploy "${common[@]}" examples/agent.hl | tee -a "$log"
  "$hl" secrets list "${common[@]}" | grep -qx HL_TOKEN || fail "$target: HL_TOKEN is gone after the second deploy"
  "$hl" status "${common[@]}" | tee -a "$log"
  "$hl" logs "${common[@]}" --events 20 | tee -a "$log"
  "$hl" rollback "${common[@]}" | tee -a "$log"
  "$hl" status "${common[@]}" | tee -a "$log"
  "$hl" destroy "${common[@]}" --yes | tee -a "$log"
  deployed=("${deployed[@]/$target}")
  echo "deploy real: $target: deploy, secrets, redeploy, status, logs, rollback and destroy ok against the real API"
done
