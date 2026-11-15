#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${JS_MUTATION_SEEDS:-0..1000}"
file="${JS_MUTATION_FILE:-examples/code_agent.hl}"
hl=target/release/hl
out=output/js-mutations
mkdir -p "$out"

[ -x "$hl" ] || cargo build --release --locked

mutations=(
  "cell_recovery_redispatches_external cloudflare nested_external_once"
  "cell_recovery_reports_failed cloudflare unknown_nested_reported"
  "cell_replay_ignores_journal cloudflare seq_unique"
  "cell_replay_ignores_journal vercel backend_equal"
  "cell_rerun_after_finish cloudflare cell_finishes_once"
  "cell_rerun_after_finish vercel cell_finishes_once"
  "divergence_dispatches cloudflare cell_replay_equal"
  "divergence_dispatches vercel cell_replay_equal"
  "nested_ids_restart cloudflare liveness|nested_external_once"
  "nested_ids_restart vercel backend_equal"
  "cell_limit_unchecked cloudflare cell_bounded"
  "cell_limit_unchecked vercel cell_bounded"
  "vercel_nested_retry_redispatches vercel nested_external_once"
  "cell_gate_releases_early cloudflare nested_external_once|backend_equal"
  "nested_idempotent_reported_unknown cloudflare nested_known"
  "nested_idempotent_reported_unknown vercel backend_equal|nested_known"
)

for target in cloudflare vercel; do
  "$hl" emit --format source --target "$target" "$file" > "$out/$target.mjs"
  "$hl" sim --target "$target" --seeds "$seeds" --trace "$out/$target-none.trace" "$file" > "$out/$target-none.sim.log"
done

check() {
  local mutation=$1 target=$2 expected=$3
  local mutated="$out/$mutation-$target"
  "$hl" sim --target "$target" --seeds "$seeds" --mutation "$mutation" --trace "$mutated.trace" "$file" > "$mutated.sim.log" 2>&1 || true
  if node scripts/js-sim.mjs --target "$target" --module "$out/$target.mjs" --trace "$mutated.trace" --mutation "$mutation" > "$mutated.log" 2>&1; then
    echo "js-mutations: $mutation survived on $target against the mutated interpreter" >&2
    return 1
  fi
  local line
  line=$(grep -m1 '^FAIL' "$mutated.log" || true)
  if ! [[ "$line" =~ invariant=($expected)\  ]]; then
    echo "js-mutations: $mutation on $target failed for another reason than $expected:" >&2
    tail -n 3 "$mutated.log" >&2
    return 1
  fi
  if node scripts/js-sim.mjs --target "$target" --module "$out/$target.mjs" --trace "$out/$target-none.trace" --mutation "$mutation" > "$mutated.clean.log" 2>&1; then
    echo "js-mutations: $mutation survived on $target against the clean interpreter" >&2
    return 1
  fi
  local clean
  clean=$(grep -m1 '^FAIL' "$mutated.clean.log" | cut -d: -f1 || true)
  echo "${line%%:*} | clean trace: ${clean#FAIL }"
}

failed=0
pids=()
for entry in "${mutations[@]}"; do
  read -r mutation target expected <<< "$entry"
  check "$mutation" "$target" "$expected" &
  pids+=($!)
  if [ "${#pids[@]}" -ge "${JS_MUTATION_JOBS:-2}" ]; then
    wait "${pids[0]}" || failed=1
    pids=("${pids[@]:1}")
  fi
done
for pid in "${pids[@]}"; do
  wait "$pid" || failed=1
done
if [ "$failed" -ne 0 ]; then
  echo "js-mutations: a mutation was not caught as expected" >&2
  exit 1
fi
echo "js-mutations: every cell mutation of the generated JavaScript is caught with its invariant, against the mutated and the clean interpreter"
