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
hl=target/release/hl
oracle=oracle/cloudflare
scratch=output/cloudflare-drill
mkdir -p "$scratch"

cargo build --release --locked -j1 > /dev/null 2>&1 || cargo build --release --locked -j1
if [ ! -x "$oracle/node_modules/.bin/workerd" ]; then
  (cd "$oracle" && npm ci --no-audit --no-fund > /dev/null)
fi
echo "cloudflare drill: $("$oracle/node_modules/.bin/workerd" --version)"

drill() {
  node "$oracle/drill.mjs" "$@"
}

tools_of() {
  grep -oE '^tool [a-z_]+' "$1" | cut -d' ' -f2 | paste -sd, -
}

for file in examples/*.hl; do
  name=$(basename "$file" .hl)
  "$hl" emit --target cloudflare "$file" > "$scratch/$name.mjs"
  "$hl" sim --target cloudflare --seeds "$trace_seeds" --trace "$scratch/$name.trace.jsonl" "$file" > /dev/null
  drill --mode trace --module "$scratch/$name.mjs" --dir "$scratch/trace-$name" --trace "$scratch/$name.trace.jsonl" --jobs "$jobs" | tail -n 2
  drill --mode kill --module "$scratch/$name.mjs" --dir "$scratch/kill-$name" --trace "$scratch/$name.trace.jsonl" --tools "$(tools_of "$file")" \
    --sessions "$sessions" --turns "$turns" --seed "$seed" --jobs "$jobs" | tail -n 2
done

expect_caught() {
  local mutation=$1 mode=$2 profile=$3 expected=$4
  local module="$scratch/agent-$mutation.mjs" log="$scratch/mutation-$mutation.log"
  node "$oracle/mutate.mjs" "$mutation" "$scratch/agent.mjs" "$module"
  local args=(--mode "$mode" --module "$module" --dir "$scratch/mutation-$mutation" --trace "$scratch/agent.trace.jsonl" --jobs "$jobs")
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

expect_caught dispatch_before_insert kill all 'FAIL invariant=external_once .*reached the tool host before its call row was committed'
expect_caught no_alarm kill all 'FAIL invariant=liveness .*with no alarm committed'
expect_caught recovery_redispatches kill all 'FAIL invariant=external_once .*was dispatched twice'
expect_caught recovery_unreported kill all 'FAIL invariant=unknown_reported .*no recovery reports it unknown'
expect_caught unserialized kill all 'FAIL invariant=liveness .*one turn runs at a time in an object'
expect_caught stale_alarm_flag kill storage 'FAIL invariant=liveness .*with no alarm committed'
expect_caught no_sync trace - 'FAIL invariant=backend_equal .*journal at open differ'
echo "cloudflare drill: ok"
