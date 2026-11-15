#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

hl=target/release/hl
scratch=output/prove-mutations
files="examples/agent.hl examples/reviewer.hl examples/code_agent.hl"
cell_files="examples/code_agent.hl"
park_files="examples/hooks.hl"
recover_files="docs/snippets/in_flight.hl"
wait_files="docs/snippets/code_wait.hl"
pay_files="docs/snippets/payments.hl docs/snippets/paid_lanes.hl examples/mpp_agent.hl"
plain_files="examples/agent.hl examples/reviewer.hl"
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
    rerun_rewrites_call | nested_ids_restart) echo seq_unique ;;
    checkpoint_hash_order) echo replay_equal ;;
    drop_requires | repeat_assign) echo invariant_preserved ;;
    cell_header_rides_past_external | nested_flush_after_dispatch) echo durable_before_external ;;
    cell_recovery_redispatches_external | cell_replay_ignores_journal | vercel_nested_retry_redispatches | postgres_cell_unfenced) echo external_once ;;
    cell_recovery_reports_failed) echo unknown_nested_reported ;;
    cell_rerun_after_finish | drop_cell_finish) echo cell_finish_once ;;
    timeout_rides) echo header_before_wait ;;
    divergence_dispatches) echo cell_replay_equal ;;
    cell_limit_unchecked) echo cell_bounds ;;
    cell_gate_releases_early) echo output_gate ;;
    nested_idempotent_reported_unknown) echo nested_known ;;
    suspend_before_flush) echo suspend_durable ;;
    resume_replays_suspend) echo seq_unique ;;
    recover_trusts_lost) echo external_once ;;
    cell_leaks) echo cell_settled_at_turn_end ;;
    drop_pay_guard) echo invariant_preserved ;;
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
is_cell_mutation() {
  case $1 in
    cell_* | nested_* | timeout_rides | divergence_dispatches | vercel_nested_retry_redispatches | postgres_cell_unfenced | drop_cell_finish) return 0 ;;
    *) return 1 ;;
  esac
}

is_park_mutation() {
  case $1 in
    suspend_before_flush | resume_replays_suspend) return 0 ;;
    *) return 1 ;;
  esac
}

check_untouched() {
  local mutation=$1 file target status
  for file in $plain_files; do
    for target in $targets; do
      set +e
      "$hl" prove --target "$target" --mutation "$mutation" "$file" > /dev/null 2> "$scratch/$mutation.stderr"
      status=$?
      set -e
      if [ "$status" -ne 0 ] && [ "$status" -ne "$usage_exit" ]; then
        echo "mutation $mutation on $target: $file has no code tool, yet its proof failed (exit $status):"
        cat "$scratch/$mutation.stderr"
        return 1
      fi
    done
  done
  echo "mutation $mutation leaves $plain_files proved"
}

check_mutation() {
  local mutation=$1 property=$2 file target status applied mutated=$files
  if is_cell_mutation "$mutation"; then
    mutated=$cell_files
    check_untouched "$mutation" || return 1
  fi
  if is_park_mutation "$mutation"; then
    mutated=$park_files
    check_untouched "$mutation" || return 1
  fi
  if [ "$mutation" = cell_leaks ]; then
    mutated=$wait_files
  fi
  if [ "$mutation" = drop_pay_guard ]; then
    mutated=$pay_files
    check_untouched "$mutation" || return 1
  fi
  if [ "$mutation" = recover_trusts_lost ]; then
    mutated=$recover_files
    check_untouched "$mutation" || return 1
  fi
  for file in $mutated; do
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
expect_error examples/invalid/cell_journal_bytes.hl "handler '(code exec)': cell_bounds: journal_bytes (cell_journal_bytes_max = 67108864) exceeds nanocode's JOURNAL_LIMIT_BYTES (33554432); path: code exec"
expect_error examples/invalid/cell_journal_floor.hl "handler '(code exec)': cell_bounds: journal_bytes (cell_journal_bytes_max = 4096) cannot hold the entries of 256 nested calls and 64 timers (20480 bytes at 32 bytes per entry at least); path: code exec"
expect_error examples/invalid/cell_over_cloudflare.hl "handler 'prompt' on cloudflare: worst_case: worst case 16400 outbound requests (16384 of them nested calls in cells) exceeds cloudflare's limit of 10000 outbound requests per invocation; path: step below turn_steps_max (16) x call below calls_per_step_max (4)"
expect_error examples/invalid/cell_push_unbounded.hl "handler 'run': push_bound: push onto outputs may find it full and trap with bound(outputs); path: match exec(script): ok; counterexample: len(outputs) = 16"
for target in native-jsonl native-sqlite server-postgres vercel; do
  if ! "$hl" prove --target "$target" examples/invalid/cell_over_cloudflare.hl > /dev/null 2> "$scratch/stderr"; then
    echo "examples/invalid/cell_over_cloudflare.hl must prove on $target:" >&2
    cat "$scratch/stderr" >&2
    exit 1
  fi
done
expect_error examples/invalid/long_wait.hl "handler 'nap' on cloudflare: worst_case: worst case 3600000 milliseconds in its longest wait exceeds cloudflare's limit of 900000 milliseconds per wait; path: no loops"
expect_error examples/invalid/wait_uncapped.hl "handler 'poll': wait_bound: wait(after_ms) may sleep past its cap retry_cap_ms and trap with bound(retry_cap_ms); path: match status(job): ok; counterexample: after_ms = 32001"
expect_error examples/invalid/long_wait.hl "handler 'nap' on vercel: worst_case: worst case 3600000 milliseconds in its longest wait exceeds vercel's limit of 800000 milliseconds per wait; path: no loops"
for target in native-jsonl native-sqlite server-postgres cloudflare vercel; do
  if ! "$hl" prove --target "$target" examples/invalid/over_wait.hl > /dev/null 2> "$scratch/stderr"; then
    echo "examples/invalid/over_wait.hl must prove on $target:" >&2
    cat "$scratch/stderr" >&2
    exit 1
  fi
done
expect_error examples/invalid/coefficient_overflow.hl "handler 'double': no_overflow: u64 overflow in '((((x - y) * half) * half) + z) + ((((x - y) * half) * half) + z)' is possible; path: entry"
expect_error examples/invalid/and_overflow.hl "handler 'take': no_overflow: u64 overflow in 'total + value' is possible; path: match measure(what): ok; counterexample: total = 5000000, value = 18446744073704551616"
expect_error examples/invalid/loop_sum_lower.hl "handler 'add': no_overflow: u64 underflow in 'total - step_sum' is possible; path: entry; counterexample: total = 28"
expect_error examples/invalid/invariant_underflow.hl "handler 'earn': invariant_preserved: invariant 'close' ((spent - earned) <= gap_max) may trap with u64 underflow in 'spent - earned' when the handler returns; path: entry"
expect_error examples/invalid/retry_unbounded.hl "handler 'ask': invariant_preserved: invariant 'failures_bounded' (failures < attempts_max) may not hold when the handler returns; path: entry; counterexample: failures = 16"
echo "prove: every prover invalid example fails with its expected error"
echo "prove-mutations: every mutation is caught"
