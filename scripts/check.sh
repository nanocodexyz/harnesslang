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
expect_error examples/invalid/code_parallel_for.hl any "code_parallel_for.hl:39:7: error: a parallel for dispatches tool calls at once, but code 'exec' makes a dispatch able to run a cell"
expect_error examples/invalid/code_missing_field.hl any "code_missing_field.hl:16:6: error: code 'exec' lacks 'timers'"
expect_error examples/invalid/code_no_tools.hl any "code_no_tools.hl:16:6: error: code 'exec' lists no tools"
expect_error examples/invalid/code_unknown_field.hl any "code_unknown_field.hl:24:3: error: code 'exec' has no field 'memory'"
expect_error examples/invalid/enum_missing_value.hl any 'enum_missing_value.hl:6:3: error: match on status is missing "interrupted"'
expect_error examples/invalid/enum_unknown_value.hl any 'enum_unknown_value.hl:10:10: error: "cancelled" is not a value of status'
expect_error examples/invalid/reply_two_call_lists.hl any "reply_two_call_lists.hl:7:73: error: reply type 'split_reply' carries list<call, calls_max> in 'reads' and 'writes': exactly one variant carries the model's calls"
expect_error examples/invalid/reply_no_call_list.hl any "reply_no_call_list.hl:6:6: error: reply type 'text_reply' has no variant carrying list<call, calls_max>"
expect_error examples/invalid/reply_missing_variant.hl any "reply_missing_variant.hl:21:7: error: match on turn_reply is missing 'paused'"
expect_error examples/invalid/reply_unmatched_failure.hl any "reply_unmatched_failure.hl:20:5: error: 'answer' is not a variant of known<turn_reply>"
expect_error examples/invalid/over_cloudflare.hl cloudflare "handler 'prompt': worst case 17408 outbound requests exceeds cloudflare's limit of 10000 outbound requests per invocation; loops: step below turn_steps_max (1024) x call below calls_per_step_max (16)"
expect_error examples/invalid/fn_recursion.hl any "fn_recursion.hl:33:3: error: fn 'ask' is recursive: ask -> run_tools -> ask"
expect_error examples/invalid/fn_arity.hl any "fn_arity.hl:16:3: error: fn 'note' takes 2 argument(s), found 1"
expect_error examples/invalid/fn_hidden_name.hl any "fn_hidden_name.hl:10:14: error: fn 'remember' cannot see 'input'"
for target in native-jsonl native-sqlite server-postgres vercel; do
  "$hl" check --target "$target" examples/invalid/over_cloudflare.hl > /dev/null
done
echo "check: every invalid example fails with its expected error"

same_program() {
  local with_fn=$1 written_out=$2 target
  for target in $targets; do
    "$hl" emit --target "$target" "$written_out" > "$scratch/written_out.emit"
    if ! "$hl" emit --target "$target" "$with_fn" | cmp -s - "$scratch/written_out.emit"; then
      echo "fn: $with_fn and $written_out emit different code for $target" >&2
      return 1
    fi
  done
  "$hl" check "$written_out" | sed "s#$written_out#FILE#" > "$scratch/written_out.check"
  if ! "$hl" check "$with_fn" | sed "s#$with_fn#FILE#" | cmp -s - "$scratch/written_out.check"; then
    echo "fn: $with_fn and $written_out check and prove differently" >&2
    return 1
  fi
}
same_program examples/subagents.hl examples/inlined/subagents.hl
same_program docs/snippets/fn.hl docs/snippets/fn_inline.hl
same_program examples/code_tasks.hl examples/inlined/code_tasks.hl
sed '/push agents spawned/{N;s/\(.*\)\n\(.*\)/\2\n\1/}' examples/inlined/subagents.hl > "$scratch/planted.hl"
"$hl" check "$scratch/planted.hl" > /dev/null
if same_program examples/subagents.hl "$scratch/planted.hl" 2> /dev/null; then
  echo "fn: a written-out program with two pushes swapped still compares equal" >&2
  exit 1
fi
echo "check: every fn program emits, checks and proves exactly as its written-out copy"

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
scripts/exhaustive.sh
scripts/strict.sh
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
  SQL_DRILL_SESSIONS=2 scripts/rust-sql-drill.sh
  for target in native-sqlite server-postgres; do
    log="$scratch/sql-drill-flush_after_dispatch-$target.log"
    if SQL_DRILL_TARGETS=$target SQL_DRILL_MODE=crash SQL_DRILL_MUTATION=flush_after_dispatch scripts/rust-sql-drill.sh > "$log" 2>&1; then
      echo "drill: flush_after_dispatch survived the SQL crash drill on $target" >&2
      exit 1
    fi
    if ! grep -qE 'external [0-9]+/[0-9]+ ran (before its call record was durable|as its recorded tool)' "$log"; then
      echo "drill: flush_after_dispatch failed for another reason on $target:" >&2
      cat "$log" >&2
      exit 1
    fi
    log="$scratch/sql-drill-no_fence-$target.log"
    if SQL_DRILL_TARGETS=$target SQL_DRILL_MODE=takeover SQL_DRILL_MUTATION=no_fence scripts/rust-sql-drill.sh > "$log" 2>&1; then
      echo "drill: no_fence survived the takeover drill on $target" >&2
      exit 1
    fi
    if ! grep -qE 'fence: external [0-9]+/[0-9]+ dispatched by run a\.|taken_over: only a stale owner writes the marker' "$log"; then
      echo "drill: no_fence failed for another reason on $target:" >&2
      cat "$log" >&2
      exit 1
    fi
  done
  echo "drill: flush_after_dispatch and no_fence are caught in the real SQLite and Postgres databases"
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
  plant=nested_enum_unchecked
  if RUST_DIFF_SEEDS=0..300 RUST_DIFF_OPT=0 RUST_DIFF_FILES=examples/code_tasks.hl RUST_DIFF_PLANT=$plant scripts/rust-diff.sh > "$scratch/rust-diff-$plant.log" 2>&1; then
    echo "rust-diff: plant $plant matched the interpreter" >&2
    exit 1
  fi
  if ! grep -q 'FAIL invariant=backend_equal' "$scratch/rust-diff-$plant.log"; then
    echo "rust-diff: plant $plant failed for another reason:" >&2
    cat "$scratch/rust-diff-$plant.log" >&2
    exit 1
  fi
  echo "rust-diff: plant $plant is caught by the differential"
fi
if [ "${HL_CLOUDFLARE:-0}" = 1 ]; then
  scripts/cloudflare-drill.sh
fi
echo "check.sh: ok"
