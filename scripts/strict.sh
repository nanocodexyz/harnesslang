#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${STRICT_SEEDS:-0..6}"
caught_seeds="${STRICT_CAUGHT_SEEDS:-0..50}"
hl=target/release/hl
scratch=output/strict
targets="native-jsonl native-sqlite server-postgres cloudflare vercel"
usage_exit=2
mkdir -p "$scratch"

[ -x "$hl" ] || cargo build --release --locked

refuse() {
  local status
  set +e
  "$hl" sim "$@" --target native-jsonl --seeds 0..1 examples/agent.hl > /dev/null 2>&1
  status=$?
  set -e
  if [ "$status" -ne "$usage_exit" ]; then
    echo "hl sim $* must print its usage and exit $usage_exit, exited $status" >&2
    exit 1
  fi
}

refuse --any-state
refuse --exhaustive --strict
refuse --exhaustive --any-state
refuse --exhaustive --strict --any-state
refuse --strict --dump-journal "$scratch/journal"
refuse --any-state --dump-journal "$scratch/journal"
echo "strict: --any-state needs --strict, and neither combines with --exhaustive or --dump-journal"

pids=()
for target in $targets; do
  (
    for file in examples/*.hl examples/sound/*.hl; do
      "$hl" sim --target "$target" --seeds "$seeds" --strict "$file"
      "$hl" sim --target "$target" --seeds "$seeds" --strict --any-state "$file"
    done
  ) > "$scratch/sim-$target.log" 2>&1 &
  pids+=($!)
done
failed=0
for index in "${!pids[@]}"; do
  if ! wait "${pids[$index]}"; then
    failed=1
  fi
done
cat "$scratch"/sim-*.log
if [ "$failed" -ne 0 ]; then
  echo "strict: a sweep failed" >&2
  exit 1
fi
echo "strict: every example and sound example passes seeds $seeds on every target, from the zero state and from any state"

expect_caught() {
  local file=$1 invariant=$2 status
  shift 2
  set +e
  "$hl" sim --target native-jsonl --seeds "$caught_seeds" "$@" "$file" > "$scratch/caught.log" 2>&1
  status=$?
  set -e
  if [ "$status" -ne 1 ] || ! grep -q "^FAIL invariant=$invariant " "$scratch/caught.log"; then
    echo "hl sim $* $file: expected FAIL invariant=$invariant in seeds $caught_seeds (exit $status):" >&2
    cat "$scratch/caught.log" >&2
    exit 1
  fi
  echo "caught $file with $*: $invariant"
}

for mode in "--strict" "--strict --any-state"; do
  read -ra flags <<< "$mode"
  expect_caught examples/invalid/and_overflow.hl no_trap "${flags[@]}"
  expect_caught examples/invalid/coefficient_overflow.hl no_trap "${flags[@]}"
  expect_caught examples/invalid/loop_sum_lower.hl no_trap "${flags[@]}"
  expect_caught examples/invalid/invariant_underflow.hl invariant_kept "${flags[@]}"
  expect_caught examples/invalid/invariant_return.hl invariant_kept "${flags[@]}"
done
expect_caught examples/invalid/invariant_initial.hl invariant_kept --strict
echo "strict: every example the prover rejects for a trap or an invariant fails under --strict"
