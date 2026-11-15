#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

trace_seeds="${CLOUDFLARE_TRACE_SEEDS:-0..4}"
sessions="${CLOUDFLARE_SESSIONS:-3}"
turns="${CLOUDFLARE_TURNS:-4}"
mutation_sessions="${CLOUDFLARE_MUTATION_SESSIONS:-8}"
mutation_turns="${CLOUDFLARE_MUTATION_TURNS:-6}"
jobs="${CLOUDFLARE_JOBS:-3}"
seed="${CLOUDFLARE_SEED:-0}"
examples="${CLOUDFLARE_EXAMPLES:-$(echo examples/*.hl)}"
hl=target/release/hl
oracle=oracle/cloudflare
scratch=output/cloudflare-drill
mkdir -p "$scratch"

cargo build --release --locked -j1 > /dev/null 2>&1 || cargo build --release --locked -j1
npm_prefix=output/cloudflare-npm
workerd="$npm_prefix/node_modules/.bin/workerd"
if [ ! -x "$workerd" ] || ! cmp -s "$oracle/package-lock.json" "$npm_prefix/package-lock.json"; then
  mkdir -p "$npm_prefix"
  cp "$oracle/package.json" "$oracle/package-lock.json" "$npm_prefix/"
  (cd "$npm_prefix" && npm ci --no-audit --no-fund > /dev/null)
fi
echo "cloudflare drill: $("$workerd" --version)"
loader_log="$scratch/loader.log"
if ! node "$oracle/loader.mjs" --dir "$scratch/loader" > "$loader_log" 2>&1; then
  cat "$loader_log" >&2
  exit 1
fi
grep -E '^(fact|FAIL|loader facts)' "$loader_log" | cut -c1-160
storage_log="$scratch/storage.log"
if ! node "$oracle/storage.mjs" --dir "$scratch/storage" > "$storage_log" 2>&1; then
  cat "$storage_log" >&2
  exit 1
fi
grep -E '^(fact|note|storage facts)' "$storage_log" | cut -c1-160
cited=$(grep -oE 'storage\.mjs [a-z]+(_[a-z]+)+(, [a-z]+(_[a-z]+)+)*' src/target/mod.rs | cut -d' ' -f2- | tr -d ',' | tr ' ' '\n' | sort -u)
if [ -z "$cited" ]; then
  echo "cloudflare drill: src/target/mod.rs cites no fact of oracle/cloudflare/storage.mjs" >&2
  exit 1
fi
for fact in $cited; do
  if ! grep -q "^fact $fact:" "$storage_log"; then
    echo "cloudflare drill: src/target/mod.rs cites $fact, which oracle/cloudflare/storage.mjs did not establish" >&2
    exit 1
  fi
done
echo "cloudflare drill: $(echo "$cited" | wc -l) facts src/target/mod.rs cites were measured"
loader_cited=$(grep -oE 'loader\.mjs [a-z]+(_[a-z]+)+(, [a-z]+(_[a-z]+)+)*' src/target/mod.rs | cut -d' ' -f2- | tr -d ',' | tr ' ' '\n' | sort -u)
if [ -z "$loader_cited" ]; then
  echo "cloudflare drill: src/target/mod.rs cites no fact of oracle/cloudflare/loader.mjs" >&2
  exit 1
fi
for fact in $loader_cited; do
  if ! grep -q "^fact $fact:" "$loader_log"; then
    echo "cloudflare drill: src/target/mod.rs cites $fact, which oracle/cloudflare/loader.mjs did not establish" >&2
    exit 1
  fi
done
echo "cloudflare drill: $(echo "$loader_cited" | wc -l) cell facts src/target/mod.rs cites were measured"

drill() {
  node "$oracle/drill.mjs" "$@"
}

tools_of() {
  grep -oE '^tool [a-z_]+' "$1" | cut -d' ' -f2 | paste -sd, -
}

module_of() {
  local name=$1 plant=$2
  local bundle="$scratch/bundle-$name-$plant"
  rm -rf "$bundle"
  "$hl" bundle --target cloudflare --plant "$plant" --out "$bundle" "examples/$name.hl" > /dev/null
  echo --module "$bundle/worker.mjs" --wasm "$bundle/program.wasm"
}

drilled=0
skipped=0
for file in $examples; do
  name=$(basename "$file" .hl)
  if ! "$hl" bundle --target cloudflare --out "$scratch/bundle-$name-check" "$file" > /dev/null 2> "$scratch/$name.bundle.err"; then
    echo "cloudflare drill: $name is not deployed: $(cut -c1-160 "$scratch/$name.bundle.err")"
    skipped=$((skipped + 1))
    continue
  fi
  drilled=$((drilled + 1))
  read -ra module <<< "$(module_of "$name" none)"
  "$hl" sim --target cloudflare --seeds "$trace_seeds" --trace "$scratch/$name.trace.jsonl" "$file" > /dev/null
  if grep -qE '"event":"(inbox|batch|resume)"' "$scratch/$name.trace.jsonl"; then
    echo "cloudflare drill: $name runs in kill mode only: the trace drill delivers no events or resumes and cannot order a batch's concurrent dispatches as the trace does"
  else
    drill --mode trace "${module[@]}" --dir "$scratch/trace-$name" --trace "$scratch/$name.trace.jsonl" --jobs "$jobs" | tail -n 2
  fi
  drill --mode kill "${module[@]}" --dir "$scratch/kill-$name" --trace "$scratch/$name.trace.jsonl" --tools "$(tools_of "$file")" \
    --sessions "$sessions" --turns "$turns" --seed "$seed" --jobs "$jobs" | tail -n 2
done

echo "cloudflare drill: $drilled programs drilled from their deploy bundles, $skipped not deployed"

expect_caught() {
  local mutation=$1 mode=$2 profile=$3 expected=$4
  if [ -n "${5:-}" ]; then expected=$5; fi
  local log="$scratch/mutation-$mutation.log" module
  local plant=${WASM_PLANTS[$mutation]:-none}
  if [ "$plant" != none ]; then
    read -ra module <<< "$(module_of agent "$plant")"
  else
    read -ra module <<< "$(module_of agent none)"
    node "$oracle/mutate.mjs" "$mutation" "${module[1]}" "$scratch/host-$mutation.mjs"
    module[1]="$scratch/host-$mutation.mjs"
  fi
  local args=(--mode "$mode" "${module[@]}" --dir "$scratch/mutation-$mutation" --trace "$scratch/agent.trace.jsonl" --jobs "$jobs")
  if [ "$mode" = kill ]; then
    args+=(--profile "$profile" --tools "$(tools_of examples/agent.hl)" --sessions "$mutation_sessions" --turns "$mutation_turns" --seed "$seed")
  fi
  if drill "${args[@]}" > "$log" 2>&1; then
    echo "cloudflare drill: mutation $mutation survived the $mode drill" >&2
    exit 1
  fi
  if ! grep -qE "$expected" "$log"; then
    echo "cloudflare drill: mutation $mutation failed for another reason than $expected:" >&2
    cat "$log" >&2
    exit 1
  fi
  echo "cloudflare drill: mutation $mutation caught: $(grep -m1 '^FAIL' "$log" | cut -c1-200)"
}

declare -A WASM_PLANTS=([recovery_redispatches]=recovery_skipped [recovery_unreported]=recovery_unreported)

expect_caught dispatch_before_insert kill all 'FAIL invariant=external_once .*reached the tool host before its call row was committed'
expect_caught no_alarm kill all 'FAIL invariant=liveness .*with no alarm committed'
expect_caught recovery_redispatches kill all 'FAIL invariant=external_once .*was dispatched twice' 'FAIL invariant=liveness .*recovery turns every open external call unknown before the turn runs again'
expect_caught recovery_unreported kill all 'FAIL invariant=unknown_reported .*no recovery reports it unknown'
expect_caught unserialized kill all 'FAIL invariant=liveness .*one turn runs at a time in an object'
expect_caught stale_alarm_flag kill storage 'FAIL invariant=liveness .*with no alarm committed'
expect_caught no_sync trace - 'FAIL invariant=backend_equal .*journal at open differ'
echo "cloudflare drill: ok"
