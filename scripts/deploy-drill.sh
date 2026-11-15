#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

hl="${DEPLOY_DRILL_HL:-target/release/hl}"
fake_latency_ms=0
plant="${DEPLOY_DRILL_PLANT:-none}"
targets="${DEPLOY_DRILL_TARGETS:-cloudflare vercel native-jsonl}"
scratch="${DEPLOY_DRILL_SCRATCH:-output/deploy-drill/$plant}"
specs=output/deploy/specs
cloudflare_spec_url=https://raw.githubusercontent.com/cloudflare/api-schemas/8b1a451819bc546d410e41cca71a86bcc2779808/openapi.json
cloudflare_spec_sha256=214602bc44128f3b441db94e5a77a1da8c47d6c5d9da9c26230dfff2650cdebf
vercel_spec_url=https://raw.githubusercontent.com/vercel/sdk/fe01b47433aa130b15c68a738a8773d9dd38b190/vercel-spec.json
vercel_spec_sha256=f96d1077e4152c4b51b1c821a7525f9bbb0ceea45ddd3a6141138041a26893cd
token=drill-token
first_file=examples/minimal.hl
second_file=examples/agent.hl

fail() {
  echo "deploy drill: $*" >&2
  exit 1
}

[ -x "$hl" ] || cargo build --release --locked
rm -rf "$scratch"
mkdir -p "$scratch" "$specs"

fetch_spec() {
  local file=$1 url=$2 sha=$3
  if [ -f "$file" ] && [ "$(sha256sum "$file" | cut -d' ' -f1)" = "$sha" ]; then
    return
  fi
  local download="$file.download.$$"
  curl -sSfL --retry 3 -o "$download" "$url" || fail "cannot fetch $url"
  [ "$(sha256sum "$download" | cut -d' ' -f1)" = "$sha" ] || fail "$url does not hash to $sha"
  mv "$download" "$file"
}
fetch_spec "$specs/cloudflare.json" "$cloudflare_spec_url" "$cloudflare_spec_sha256"
fetch_spec "$specs/vercel.json" "$vercel_spec_url" "$vercel_spec_sha256"

fake_pid=
stop_fake() {
  if [ -n "$fake_pid" ]; then
    kill -TERM "$fake_pid" 2> /dev/null || true
    wait "$fake_pid" 2> /dev/null || true
    fake_pid=
  fi
}
trap stop_fake EXIT

start_fake() {
  rm -f "$scratch/port" "$scratch/report.json"
  node oracle/deploy/fake.mjs --cloudflare-spec "$specs/cloudflare.json" --vercel-spec "$specs/vercel.json" \
    --port-file "$scratch/port" --token "$token" --report "$scratch/report.json" --latency-ms "$fake_latency_ms" > "$scratch/fake.log" 2>&1 &
  fake_pid=$!
  local waited
  for waited in $(seq 1 100); do
    [ -s "$scratch/port" ] && break
    kill -0 "$fake_pid" 2> /dev/null || { cat "$scratch/fake.log" >&2; fail "the fake exited"; }
    sleep 0.1
  done
  [ -s "$scratch/port" ] || fail "the fake wrote no port in $waited tenths of a second"
  port=$(cat "$scratch/port")
  export CLOUDFLARE_API_TOKEN=$token CLOUDFLARE_ACCOUNT_ID=drillaccount VERCEL_TOKEN=$token
  export HL_CLOUDFLARE_API=http://127.0.0.1:$port/cloudflare/client/v4 HL_VERCEL_API=http://127.0.0.1:$port/vercel
  export HL_DEPLOY_POLL_MS=1 HL_DEPLOY_NOW_MS=1794614400000
}

field() {
  node -e 'const value = JSON.parse(require("fs").readFileSync(0, "utf8")); const out = process.argv[1].split(".").reduce((v, k) => (v == null ? v : v[k]), value); console.log(Array.isArray(out) ? out.join(",") : out ?? "")' "$1"
}

fake_state() {
  curl -sSf "http://127.0.0.1:$port/__fake/$1/$2" | field "$3"
}

provider_of() {
  case $1 in
    cloudflare | vercel) echo "$1" ;;
    *) echo local ;;
  esac
}

serving_digest() {
  local target=$1 name=$2 out=$3
  if [ "$(provider_of "$target")" = local ]; then
    cat "$out/versions/$(cat "$out/current")/digest"
  else
    fake_state "$target" "$name" digest
  fi
}

deployed_digest() {
  local target=$1 out=$2
  if [ "$(provider_of "$target")" = local ]; then
    cat "$out/versions/$(ls "$out/versions" | sort -n | tail -n 1)/digest"
  else
    field digest < "$out/deploy.json"
  fi
}

run() {
  local log=$1
  shift
  echo "\$ hl $*" >> "$log"
  "$hl" "$@" --plant "$plant" >> "$log" 2>&1
}

flow() {
  local target=$1 name=drill-$1 out=$scratch/$1 log=$scratch/$1.log first second listed
  local common=(--target "$target" --name "$name" --out "$out")
  : > "$log"
  run "$log" deploy "${common[@]}" --var HL_MODEL_URL=http://model.invalid "$first_file" || return 1
  first=$(deployed_digest "$target" "$out")
  echo drill-secret-value | run "$log" secrets set HL_TOKEN "${common[@]}" || return 1
  run "$log" deploy "${common[@]}" "$second_file" || return 1
  second=$(deployed_digest "$target" "$out")
  [ "$first" != "$second" ] || { echo "the two deploys have one digest $first" >> "$log"; return 1; }
  [ "$(serving_digest "$target" "$name" "$out")" = "$second" ] || { echo "the second deploy does not serve" >> "$log"; return 1; }
  listed=$("$hl" secrets list "${common[@]}" --plant "$plant" 2>> "$log")
  grep -qx HL_TOKEN <<< "$listed" || { echo "HL_TOKEN is gone after the second deploy" >> "$log"; return 1; }
  run "$log" status "${common[@]}" || return 1
  if [ "$(provider_of "$target")" != local ]; then
    curl -sSf -X POST --data "turn 1 answered" "http://127.0.0.1:$port/__fake/$target/$name/invoke" > /dev/null
    curl -sSf -X POST --data "turn 2 answered" "http://127.0.0.1:$port/__fake/$target/$name/invoke" > /dev/null
    run "$log" logs "${common[@]}" || return 1
    run "$log" logs "${common[@]}" --follow --events 2 || return 1
    grep -q "turn 2 answered" "$log" || { echo "the followed logs miss the second event" >> "$log"; return 1; }
  else
    run "$log" logs "${common[@]}" || return 1
  fi
  run "$log" rollback "${common[@]}" || return 1
  [ "$(serving_digest "$target" "$name" "$out")" = "$first" ] || { echo "the rollback does not serve the first deploy's digest $first" >> "$log"; return 1; }
  run "$log" secrets delete HL_TOKEN "${common[@]}" || return 1
  listed=$("$hl" secrets list "${common[@]}" 2>> "$log")
  if grep -qx HL_TOKEN <<< "$listed"; then
    echo "HL_TOKEN is still listed after its delete" >> "$log"
    return 1
  fi
  if [ "$(provider_of "$target")" != local ]; then
    run "$log" deploy "${common[@]}" "$second_file" || return 1
    run "$log" deploy "${common[@]}" "$second_file" || return 1
    tail -n 3 "$log" | grep -q '^  unchanged: ' || { echo "a second deploy of the same program uploaded again" >> "$log"; return 1; }
    run "$log" deploy "${common[@]}" --var HL_MODEL_URL=http://changed.invalid "$second_file" || return 1
    if tail -n 3 "$log" | grep -q '^  unchanged: '; then
      echo "a changed variable deployed as unchanged" >> "$log"
      return 1
    fi
  fi
  run "$log" destroy "${common[@]}" --yes || return 1
  if "$hl" status "${common[@]}" >> "$log" 2>&1; then
    echo "status succeeds after destroy" >> "$log"
    return 1
  fi
  [ ! -e "$out" ] || { echo "destroy left $out" >> "$log"; return 1; }
}

census() {
  "$hl" deploy --endpoints > "$scratch/endpoints"
  node -e '
    const fs = require("fs");
    const specs = { cloudflare: JSON.parse(fs.readFileSync(process.argv[1])), vercel: JSON.parse(fs.readFileSync(process.argv[2])) };
    const problems = [];
    for (const line of fs.readFileSync(process.argv[3], "utf8").trim().split("\n")) {
      const [provider, method, path, operation] = line.split(" ");
      const found = specs[provider].paths[path]?.[method.toLowerCase()];
      if (!found) problems.push(line + ": the pinned spec has no " + method + " " + path);
      else if (found.operationId !== operation) problems.push(line + ": the pinned spec names it " + found.operationId);
    }
    if (problems.length) { console.error(problems.join("\n")); process.exit(1); }
  ' "$specs/cloudflare.json" "$specs/vercel.json" "$scratch/endpoints" || fail "hl deploy --endpoints names operations the pinned specs lack"
}

if [ "${1:-}" = --bench ]; then
  scratch=output/deploy-bench
  rm -rf "$scratch"
  mkdir -p "$scratch"
  fake_latency_ms=${DEPLOY_BENCH_LATENCY_MS:-100}
  start_fake
  for target in cloudflare vercel; do
    for step in cold:examples/minimal.hl unchanged:examples/minimal.hl changed:examples/agent.hl; do
      started=$(date +%s%N)
      "$hl" deploy --target "$target" --name "bench-$target" --out "$scratch/$target" "${step#*:}" > "$scratch/$target-${step%%:*}.log" 2>&1 || { cat "$scratch/$target-${step%%:*}.log" >&2; fail "bench deploy failed"; }
      ended=$(date +%s%N)
      counted=$(curl -sSf "http://127.0.0.1:$port/__fake/traffic")
      echo "deploy bench: $target ${step%%:*} $(( (ended - started) / 1000000 )) ms, $(field requests <<< "$counted") requests in $(field rounds <<< "$counted") round trips (latency $fake_latency_ms ms)"
    done
  done
  stop_fake
  [ -z "$(field violations < "$scratch/report.json")" ] || fail "the bench broke the pinned specs"
  exit 0
fi
if [ "${1:-}" = --demo ]; then
  scratch=output/deploy-demo
  rm -rf "$scratch"
  mkdir -p "$scratch"
  start_fake
  show() {
    echo "> $*"
    "$@" 2>&1
  }
  out=$scratch/cloudflare
  common=(--target cloudflare --name demo --out "$out")
  show hl deploy "${common[@]}" --bundle docs/snippets/deploy
  echo "> echo sk-demo | hl secrets set API_KEY ..."
  echo sk-demo | hl secrets set API_KEY "${common[@]}"
  curl -sSf -X POST --data "turn 1 answered" "http://127.0.0.1:$port/__fake/cloudflare/demo/invoke" > /dev/null
  show hl status "${common[@]}"
  show hl logs "${common[@]}" --follow --events 1
  show hl rollback "${common[@]}"
  show hl destroy "${common[@]}" --yes
  stop_fake
  [ -z "$(field violations < "$scratch/report.json")" ] || fail "the demo broke the pinned specs"
  exit 0
fi
start_fake
census
failed=
for target in $targets; do
  if flow "$target"; then
    echo "deploy drill: $target: deploy, secrets set, redeploy, status, logs, rollback, secrets delete, destroy ok ($(grep -c '^\$ hl' "$scratch/$target.log") commands)"
  else
    failed="$failed $target"
    if [ "$plant" = none ]; then
      cat "$scratch/$target.log" >&2
    fi
    echo "deploy drill: $target failed: $(tail -n 1 "$scratch/$target.log")"
  fi
done
stop_fake
violations=$(field violations < "$scratch/report.json")
if [ -n "$violations" ]; then
  node -e 'for (const line of JSON.parse(require("fs").readFileSync(0, "utf8")).violations) console.error("deploy drill: violation: " + line)' < "$scratch/report.json"
  fail "requests or fake answers broke the pinned specs"
fi
[ -z "$failed" ] || fail "failed on$failed"
if [ "$plant" = none ] && [ "$targets" = "cloudflare vercel native-jsonl" ]; then
  exercised=$(node -e 'for (const op of JSON.parse(require("fs").readFileSync(0, "utf8")).exercised) console.log(op)' < "$scratch/report.json")
  while read -r provider _ _ operation; do
    grep -qx "$provider $operation" <<< "$exercised" || fail "the drill never called $provider $operation"
  done < "$scratch/endpoints"
  echo "deploy drill: every request matched the pinned specs ($(wc -l < "$scratch/endpoints") operations, each called), no violations"
fi
