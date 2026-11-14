#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

hl=target/release/hl
scratch=output/prove-mutations
files="examples/agent.hl examples/reviewer.hl"
targets="native-jsonl native-sqlite server-postgres cloudflare vercel"
usage_exit=2
mkdir -p "$scratch"

[ -x "$hl" ] || cargo build --release --locked

property_of() {
  case $1 in
    flush_after_dispatch | drop_flush) echo durable_before_external ;;
    recovery_redispatches_external | vercel_retry_redispatches | postgres_no_fence | recovery_skipped) echo external_once ;;
    jsonl_no_checksum) echo liveness ;;
    finish_before_flush | drop_finish) echo finish_once ;;
    idempotent_reported_unknown) echo idempotent_known ;;
    gate_releases_early) echo output_gate ;;
    recovery_reports_failed) echo unknown_reported ;;
    rerun_rewrites_call) echo seq_unique ;;
    checkpoint_hash_order) echo replay_equal ;;
    drop_requires | repeat_assign) echo invariant_preserved ;;
    *) return 1 ;;
  esac
}

for file in examples/*.hl examples/sound/*.hl; do
  if ! "$hl" prove "$file" > /dev/null 2> "$scratch/stderr"; then
    echo "$file does not prove:" >&2
    cat "$scratch/stderr" >&2
    exit 1
  fi
done
echo "prove: every example and every sound adversarial example proves on every target"

set +e
usage=$("$hl" prove 2>&1)
status=$?
set -e
if [ "$status" -ne "$usage_exit" ]; then
  echo "hl prove without a file must print its usage and exit $usage_exit, exited $status" >&2
  exit 1
fi
mutations=$(sed -n 's/.*--mutation \([a-z_|]*\)\].*/\1/p' <<< "$usage" | tr '|' ' ')
check_mutation() {
  local mutation=$1 property=$2 file target status applied
  for file in $files; do
    applied=0
    for target in $targets; do
      set +e
      "$hl" prove --target "$target" --mutation "$mutation" "$file" > /dev/null 2> "$scratch/$mutation.stderr"
      status=$?
      set -e
      if [ "$status" -eq "$usage_exit" ]; then
        continue
      fi
      applied=$((applied + 1))
      if [ "$status" -ne 1 ] || ! grep -q ": $property: " "$scratch/$mutation.stderr"; then
        echo "mutation $mutation on $target: proving $file did not fail with $property (exit $status):"
        cat "$scratch/$mutation.stderr"
        return 1
      fi
    done
    if [ "$applied" -eq 0 ]; then
      echo "mutation $mutation applies to no target"
      return 1
    fi
    echo "caught $mutation in $file on $applied target(s): $property"
  done
}

pids=()
names=()
for mutation in $mutations; do
  [ "$mutation" = none ] && continue
  if ! property=$(property_of "$mutation"); then
    echo "mutation $mutation has no expected property in $0" >&2
    exit 1
  fi
  check_mutation "$mutation" "$property" > "$scratch/$mutation.log" 2>&1 &
  pids+=($!)
  names+=("$mutation")
done
if [ "${#pids[@]}" -eq 0 ]; then
  echo "hl prove lists no mutations" >&2
  exit 1
fi
failed=0
for index in "${!pids[@]}"; do
  if ! wait "${pids[$index]}"; then
    failed=1
  fi
  cat "$scratch/${names[$index]}.log"
done
if [ "$failed" -ne 0 ]; then
  echo "prove-mutations: a mutation was not caught" >&2
  exit 1
fi

expect_error() {
  local file=$1 expected=$2
  if "$hl" prove "$file" > /dev/null 2> "$scratch/stderr"; then
    echo "$file: expected a failed proof, it proved" >&2
    exit 1
  fi
  if ! grep -qF -- "$expected" "$scratch/stderr"; then
    echo "$file: expected error text: $expected" >&2
    cat "$scratch/stderr" >&2
    exit 1
  fi
}

expect_error examples/invalid/overflow_counter.hl "handler 'ping': no_overflow: u64 overflow in 'turns + 1' is possible; path: entry; counterexample: turns = 18446744073709551615"
expect_error examples/invalid/underflow_elapsed.hl "handler 'time': no_overflow: u64 underflow in 'finished - started' is possible; path: entry; counterexample: started = 1, finished = 0"
expect_error examples/invalid/loop_overflow.hl "handler 'spend': no_overflow: u64 overflow in 'spent + step_cost' is possible; path: loop step below steps_max (8)"
expect_error examples/invalid/product_overflow.hl "handler 'measure': no_overflow: u64 overflow in 'across * down' is possible; path: match count(width): ok > match count(height): ok"
expect_error examples/invalid/branch_overflow.hl "handler 'shift': no_overflow: u64 overflow in 'base + offset' is possible; path: entry"
expect_error examples/invalid/push_unbounded.hl "handler 'ask': push_bound: push onto history may find it full and trap with bound(history); path: entry; counterexample: len(history) = 256"
expect_error examples/invalid/push_unknown_arm.hl "handler 'ask': push_bound: push onto history may find it full and trap with bound(history); path: loop step below steps_max (4) > match reply: calls > for call in requested (at most calls_max = 4) > match dispatch(call): unknown"
expect_error examples/invalid/push_self_iteration.hl "handler 'echo': push_bound: push onto history may find it full and trap with bound(history); path: for item in history (at most history_max = 64)"
expect_error examples/invalid/push_early_return.hl "handler 'run': push_bound: push onto log may find it full and trap with bound(log); path: loop step below steps_max (10)"
expect_error examples/invalid/invariant_return.hl "handler 'tally': invariant_preserved: invariant 'count_bounded' (count <= count_max) may not hold when the handler returns"
expect_error examples/invalid/invariant_trap.hl "handler 'ask': invariant_preserved: invariant 'idle' (busy == 0) may not hold when the reply of think(history) fails to decode and the turn traps; path: entry; counterexample: busy = 1"
expect_error examples/invalid/invariant_initial.hl "handler '(zero state)': invariant_initial: invariant 'started' (turns >= turns_min) may not hold in the zero state; path: the zero state; counterexample: turns = 0"
expect_error examples/invalid/invariant_unsupported.hl "handler 'arm': invariant_preserved: invariant 'disarmed' (not ready) may not hold when the handler returns; path: entry; counterexample: the prover reasons only about u64 comparisons joined by and, or and not"
expect_error examples/invalid/coefficient_overflow.hl "handler 'double': no_overflow: u64 overflow in '((((x - y) * half) * half) + z) + ((((x - y) * half) * half) + z)' is possible; path: entry"
expect_error examples/invalid/and_overflow.hl "handler 'take': no_overflow: u64 overflow in 'total + value' is possible; path: match measure(what): ok; counterexample: total = 5000000, value = 18446744073704551616"
expect_error examples/invalid/loop_sum_lower.hl "handler 'add': no_overflow: u64 underflow in 'total - step_sum' is possible; path: entry; counterexample: total = 28"
expect_error examples/invalid/invariant_underflow.hl "handler 'earn': invariant_preserved: invariant 'close' ((spent - earned) <= gap_max) may trap with u64 underflow in 'spent - earned' when the handler returns; path: entry"
expect_error examples/invalid/retry_unbounded.hl "handler 'ask': invariant_preserved: invariant 'failures_bounded' (failures < attempts_max) may not hold when the handler returns; path: entry; counterexample: failures = 16"
echo "prove: every prover invalid example fails with its expected error"
echo "prove-mutations: every mutation is caught"
