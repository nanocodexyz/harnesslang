#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${SEEDS:-0..2000}"
twice_seeds="${TWICE_SEEDS:-0..300}"
targets="native-jsonl native-sqlite server-postgres cloudflare vercel"
hl=target/release/hl
scratch=output/check
mkdir -p "$scratch"

cargo build --release --locked
packages=$(grep -c '^name = ' Cargo.lock)
if [ "$packages" -ne 1 ] || ! grep -q '^name = "harnesslang"' Cargo.lock; then
  echo "Cargo.lock must list harnesslang alone" >&2
  exit 1
fi
if grep -rnE '^\s*//|#\[test\]|#\[cfg\(test\)\]' src; then
  echo "src has comments or test functions" >&2
  exit 1
fi

for file in examples/*.hl; do
  "$hl" check "$file" > /dev/null
  for target in $targets; do
    if ! "$hl" check --target "$target" "$file" > /dev/null 2> "$scratch/stderr"; then
      if grep -q 'cells: unsupported' "$scratch/stderr"; then
        touch "$scratch/unsupported-$(basename "$file" .hl)-$target"
        continue
      fi
      cat "$scratch/stderr" >&2
      exit 1
    fi
    rm -f "$scratch/unsupported-$(basename "$file" .hl)-$target"
  done
done
echo "check: every example passes on every target, or is refused with cells: unsupported"

expect_error() {
  local file=$1 target=$2 expected=$3
  local args=(check)
  [ "$target" != any ] && args+=(--target "$target")
  if "$hl" "${args[@]}" "$file" > /dev/null 2> "$scratch/stderr"; then
    echo "$file on $target: expected an error, the check passed" >&2
    exit 1
  fi
  if ! grep -qF -- "$expected" "$scratch/stderr"; then
    echo "$file on $target: expected error text: $expected" >&2
    cat "$scratch/stderr" >&2
    exit 1
  fi
}

expect_error examples/invalid/missing_unknown.hl any "missing_unknown.hl:21:11: error: match on maybe<text> is missing 'unknown'"
expect_error examples/invalid/unchecked_external.hl any "unchecked_external.hl:10:3: error: match on maybe<text> is missing 'unknown'"
expect_error examples/invalid/unbounded_list.hl any "unbounded_list.hl:8:22: error: a list needs a capacity"
expect_error examples/invalid/literal_bound.hl any "literal_bound.hl:13:19: error: the bound of loop 'step' must be a named constant, found number 64"
expect_error examples/invalid/unknown_name.hl any "unknown_name.hl:13:21: error: unknown name 'inpt'"
expect_error examples/invalid/code_no_model.hl any "code_no_model.hl:14:6: error: code 'exec' needs a model declaration"
expect_error examples/invalid/code_unknown_tool.hl any "code_unknown_tool.hl:17:15: error: code 'exec' lists 'bash', which is not a declared tool"
expect_error examples/invalid/code_nested_code.hl any "code_nested_code.hl:27:15: error: code 'outer' lists code tool 'exec'"
expect_error examples/invalid/code_literal_bound.hl any "code_literal_bound.hl:18:9: error: field 'calls' of code 'exec' must be a named constant, found number 8"
expect_error examples/invalid/code_parallel_over_calls.hl any "code_parallel_over_calls.hl:18:6: error: code 'exec': parallel (cell_wide = 16) exceeds calls (cell_calls_max = 8)"
expect_error examples/invalid/code_missing_field.hl any "code_missing_field.hl:16:6: error: code 'exec' lacks 'timers'"
expect_error examples/invalid/code_no_tools.hl any "code_no_tools.hl:16:6: error: code 'exec' lists no tools"
expect_error examples/invalid/code_unknown_field.hl any "code_unknown_field.hl:24:3: error: code 'exec' has no field 'memory'"
expect_error examples/invalid/over_cloudflare.hl cloudflare "handler 'prompt': worst case 17408 outbound requests exceeds cloudflare's limit of 10000 outbound requests per invocation; loops: step below turn_steps_max (1024) x call below calls_per_step_max (16)"
for target in native-jsonl native-sqlite server-postgres vercel; do
  "$hl" check --target "$target" examples/invalid/over_cloudflare.hl > /dev/null
done
echo "check: every invalid example fails with its expected error"

pids=()
for target in $targets; do
  (
    for file in examples/*.hl; do
      if [ -e "$scratch/unsupported-$(basename "$file" .hl)-$target" ]; then
        echo "skip $file on $target: cells: unsupported"
        continue
      fi
      "$hl" sim --target "$target" --seeds "$seeds" "$file"
    done
    "$hl" sim --target "$target" --seeds "$twice_seeds" --twice examples/agent.hl
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
  echo "sim: a sweep failed" >&2
  exit 1
fi
echo "sim: every example passes seeds $seeds on every target, and agent.hl seeds $twice_seeds twice"

scripts/mutations.sh
scripts/prove-mutations.sh
scripts/docs-check.sh
scripts/js-diff.sh > "$scratch/js-diff.log" 2>&1 || { cat "$scratch/js-diff.log" >&2; exit 1; }
tail -n 1 "$scratch/js-diff.log"
scripts/js-mutations.sh
if [ "${HL_ORACLE:-0}" = 1 ]; then
  scripts/oracle.sh
fi
if [ "${HL_DRILL:-0}" = 1 ]; then
  DRILL_SESSIONS=4 scripts/rust-crash-drill.sh
  if DRILL_SESSIONS=8 DRILL_MUTATION=flush_after_dispatch scripts/rust-crash-drill.sh > "$scratch/drill-mutation.log" 2>&1; then
    echo "drill: flush_after_dispatch survived the crash drill" >&2
    exit 1
  fi
  if ! grep -qE 'external [0-9]+/[0-9]+ ran (before its call record was durable|as its recorded tool)' "$scratch/drill-mutation.log"; then
    echo "drill: flush_after_dispatch failed for another reason:" >&2
    cat "$scratch/drill-mutation.log" >&2
    exit 1
  fi
  echo "drill: flush_after_dispatch is caught on the real files"
  coder=(DRILL_FILE=examples/code_agent.hl)
  env "${coder[@]}" DRILL_SESSIONS=4 scripts/rust-crash-drill.sh
  for expected in \
    "nested_flush_after_dispatch:nested_external_once: [0-9/]+code-[0-9]+ ran before its (cell's header|call record) was durable" \
    "timeout_rides:header_before_wait: cell [0-9]+ waits on timer [0-9]+ before its header is durable" \
    "cell_recovery_reports_failed:unknown_nested_reported: [0-9/]+code-[0-9]+ is delivered unknown exactly when a recovery lists it" \
    "cell_recovery_redispatches_external:nested_external_once: [0-9/]+code-[0-9]+ dispatched [0-9]+ times" \
    "cell_ids_restart_at_checkpoint:(the tail's next cell follows every cell before its checkpoint|the journal folds: cell [0-9]+ starts before cell [0-9]+)"; do
    mutation=${expected%%:*}
    if env "${coder[@]}" DRILL_SESSIONS=8 DRILL_MUTATION="$mutation" scripts/rust-crash-drill.sh > "$scratch/drill-$mutation.log" 2>&1; then
      echo "drill: $mutation survived the crash drill" >&2
      exit 1
    fi
    if ! grep -qE "${expected#*:}" "$scratch/drill-$mutation.log"; then
      echo "drill: $mutation failed for another reason:" >&2
      cat "$scratch/drill-$mutation.log" >&2
      exit 1
    fi
    echo "drill: $mutation is caught on the real files"
  done
fi
if [ "${HL_RUST_DIFF:-0}" = 1 ]; then
  RUST_DIFF_SEEDS=0..300 RUST_DIFF_OPT=0 scripts/rust-diff.sh
  for plant in nested_flush_after_dispatch timeout_rides; do
    if RUST_DIFF_SEEDS=0..300 RUST_DIFF_OPT=0 RUST_DIFF_FILES=examples/code_agent.hl RUST_DIFF_PLANT=$plant scripts/rust-diff.sh > "$scratch/rust-diff-$plant.log" 2>&1; then
      echo "rust-diff: plant $plant matched the interpreter" >&2
      exit 1
    fi
    if ! grep -q 'FAIL invariant=backend_equal' "$scratch/rust-diff-$plant.log"; then
      echo "rust-diff: plant $plant failed for another reason:" >&2
      cat "$scratch/rust-diff-$plant.log" >&2
      exit 1
    fi
    echo "rust-diff: plant $plant is caught by the differential"
  done
fi
echo "check.sh: ok"
