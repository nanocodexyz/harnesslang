#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

hl="${DEPLOY_DRILL_HL:-target/release/hl}"
fake_latency_ms=0
plant="${DEPLOY_DRILL_PLANT:-none}"
targets="${DEPLOY_DRILL_TARGETS:-cloudflare vercel native-jsonl}"
scratch="${DEPLOY_DRILL_SCRATCH:-output/deploy-drill/$plant}"
specs=output/deploy/specs
cloudflare_spec_sha256=214602bc44128f3b441db94e5a77a1da8c47d6c5d9da9c26230dfff2650cdebf
vercel_spec_sha256=f96d1077e4152c4b51b1c821a7525f9bbb0ceea45ddd3a6141138041a26893cd
token=drill-token
first_file=examples/minimal.hl
second_file=examples/agent.hl

fail() {
  echo "deploy drill: $*" >&2
  exit 1
}

[ -x "$hl" ] || cargo build --release --frozen --offline
rm -rf "$scratch"
mkdir -p "$scratch" "$specs"

read_spec() {
  local name=$1 sha=$2 file="$specs/$1.json" temporary
  temporary=$(mktemp "$file.XXXXXX") || fail "cannot stage vendored $name spec"
  gzip -dc "vendor/specs/$name.json.gz" > "$temporary" || { rm -f "$temporary"; fail "cannot unpack vendored $name spec"; }
  [ "$(sha256sum "$temporary" | cut -d' ' -f1)" = "$sha" ] || { rm -f "$temporary"; fail "vendored $name spec does not hash to $sha"; }
  mv -f "$temporary" "$file" || { rm -f "$temporary"; fail "cannot publish vendored $name spec"; }
}
read_spec cloudflare "$cloudflare_spec_sha256"
read_spec vercel "$vercel_spec_sha256"

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
  node --experimental-vm-modules --disable-warning=ExperimentalWarning oracle/deploy/fake.mjs --cloudflare-spec "$specs/cloudflare.json" --vercel-spec "$specs/vercel.json" \
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
  export HL_DEPLOY_POLL_MS=1 HL_DEPLOY_NOW_MS=1794616200000
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

auth_check() {
  local target=$1 name=$2 out=$3 probe=$scratch/auth-$1 token expected got entry
  [ -s "$out/auth-token" ] || { echo "the first deploy wrote no auth token" >> "$scratch/$target.log"; return 1; }
  token=$(tr -d '\n' < "$out/auth-token")
  expected=$(printf %s "$token" | sha256sum | cut -d' ' -f1)
  [ "$(fake_state "$target" "$name" auth_sha256)" = "$expected" ] || { echo "the provisioned HL_AUTH_TOKEN is not the token in $out/auth-token" >> "$scratch/$target.log"; return 1; }
  rm -rf "$probe"
  if [ "$target" = cloudflare ]; then
    mkdir -p "$probe"
    cp "$out/files/hl-auth.mjs" "$probe/entry.mjs"
    printf 'export default { fetch() { return new Response("served"); } };\nexport class HarnessObject {}\n' > "$probe/worker.mjs"
    entry=$probe/entry.mjs
  else
    entry="$probe/app/sessions/[session]/[kind]/route.mjs"
    mkdir -p "$(dirname "$entry")" "$probe/workflows" "$probe/node_modules/workflow"
    cp "$out/files/app/sessions/[session]/[kind]/route.mjs" "$entry"
    printf 'export const harnessSession = () => null;\n' > "$probe/workflows/session.mjs"
    printf '{"name": "workflow", "type": "module", "exports": {"./api": "./api.mjs"}}\n' > "$probe/node_modules/workflow/package.json"
    printf 'export async function start() { return { runId: "run-1" }; }\nexport async function resumeHook() {}\n' > "$probe/node_modules/workflow/api.mjs"
  fi
  got=$(node --input-type=module -e '
    import { pathToFileURL } from "node:url";
    const [entry, token, target] = process.argv.slice(1);
    const loaded = await import(pathToFileURL(entry));
    const call = async (header, secret) => {
      const request = new Request("http://harness.invalid/sessions/s1/start", { method: "POST", headers: header ? { authorization: header } : {}, body: JSON.stringify({ turnsMax: 1 }) });
      if (target === "cloudflare") return (await loaded.default.fetch(request, secret ? { HL_AUTH_TOKEN: secret } : {}, {})).status;
      if (secret) process.env.HL_AUTH_TOKEN = secret;
      else delete process.env.HL_AUTH_TOKEN;
      return (await loaded.POST(request, { params: Promise.resolve({ session: "s1", kind: "start" }) })).status;
    };
    console.log([await call(null, token), await call("Bearer wrong", token), await call("Bearer " + token, token), await call("Bearer " + token, null)].join(" "));
  ' "$entry" "$token" "$target" 2>&1)
  [ "$got" = "401 401 200 503" ] || { echo "an unauthenticated request was served: no token, a wrong token, the token, no secret answered $got, not 401 401 200 503" >> "$scratch/$target.log"; return 1; }
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
  if [ "$(provider_of "$target")" != local ]; then
    auth_check "$target" "$name" "$out" || return 1
  fi
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
    curl -sSf -X POST --data "turn 3 failed" "http://127.0.0.1:$port/__fake/$target/$name/invoke?level=error" > /dev/null
    run "$log" monitor "${common[@]}" || return 1
    grep -q "3 events, 1 errors (33.3% failed)" "$log" || { echo "the monitor does not count the failed turn" >> "$log"; return 1; }
  else
    run "$log" logs "${common[@]}" || return 1
    run "$log" monitor "${common[@]}" || return 1
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
  runs=${DEPLOY_BENCH_RUNS:-1}
  start_fake
  for target in cloudflare vercel; do
    for step in cold:examples/minimal.hl unchanged:examples/minimal.hl changed:examples/agent.hl; do
      best=
      for run in $(seq 1 "$runs"); do
        log=$scratch/$target-${step%%:*}-$run.log
        TIMEFORMAT='%3U %3S'
        started=$(date +%s%N)
        { time "$hl" deploy --target "$target" --name "bench-$target-$run" --out "$scratch/$target-$run" "${step#*:}" > "$log" 2>&1; } 2> "$log.cpu" || { cat "$log" >&2; fail "bench deploy failed"; }
        ended=$(date +%s%N)
        counted=$(curl -sSf "http://127.0.0.1:$port/__fake/traffic")
        wall=$(( (ended - started) / 1000000 ))
        cpu=$(awk '{ printf "%d", ($1 + $2) * 1000 }' "$log.cpu")
        if [ -z "$best" ] || [ "$wall" -lt "${best%% *}" ]; then
          best="$wall ms (hl cpu $cpu ms), $(field requests <<< "$counted") requests in $(field rounds <<< "$counted") round trips"
        fi
      done
      echo "deploy bench: $target ${step%%:*} $best (latency $fake_latency_ms ms, best of $runs, load $(cut -d' ' -f1 /proc/loadavg))"
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
  show hl monitor "${common[@]}"
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
