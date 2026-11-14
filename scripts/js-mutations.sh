#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${JS_MUTATION_SEEDS:-0..300}"
file="${JS_MUTATION_FILE:-examples/code_agent.hl}"
hl=target/release/hl
out=output/js-mutations
mkdir -p "$out"

[ -x "$hl" ] || cargo build --release --locked

mutations=(
  "cell_recovery_redispatches_external cloudflare nested_external_once"
  "cell_recovery_reports_failed cloudflare unknown_nested_reported"
  "cell_replay_ignores_journal cloudflare nested_external_once"
  "cell_replay_ignores_journal vercel nested_external_once"
  "cell_rerun_after_finish cloudflare cell_finishes_once"
  "cell_rerun_after_finish vercel cell_finishes_once"
  "divergence_dispatches cloudflare cell_replay_equal"
  "divergence_dispatches vercel cell_replay_equal"
  "nested_ids_restart cloudflare seq_unique"
  "nested_ids_restart vercel seq_unique"
  "cell_limit_unchecked cloudflare cell_bounded"
  "cell_limit_unchecked vercel cell_bounded"
  "vercel_nested_retry_redispatches vercel nested_external_once"
  "cell_gate_releases_early cloudflare nested_external_once"
)

for target in cloudflare vercel; do
  "$hl" emit --target "$target" "$file" > "$out/$target.mjs"
  "$hl" sim --target "$target" --seeds "$seeds" --trace "$out/$target.trace" "$file" > "$out/$target.sim.log"
done

failed=0
pids=()
for entry in "${mutations[@]}"; do
  read -r mutation target expected <<< "$entry"
  (
    log="$out/$mutation-$target.log"
    if node scripts/js-sim.mjs --target "$target" --module "$out/$target.mjs" --trace "$out/$target.trace" --mutation "$mutation" > "$log" 2>&1; then
      echo "js-mutations: $mutation survived on $target" >&2
      exit 1
    fi
    line=$(grep -m1 '^FAIL' "$log" || true)
    if [[ "$line" != *"invariant=$expected "* ]]; then
      echo "js-mutations: $mutation on $target failed for another reason than $expected:" >&2
      tail -n 3 "$log" >&2
      exit 1
    fi
    echo "${line:0:200}"
  ) &
  pids+=($!)
done
for pid in "${pids[@]}"; do
  wait "$pid" || failed=1
done
if [ "$failed" -ne 0 ]; then
  echo "js-mutations: a mutation was not caught as expected" >&2
  exit 1
fi
echo "js-mutations: every cell mutation of the generated JavaScript is caught with its invariant"
