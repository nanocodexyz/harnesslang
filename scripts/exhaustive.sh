#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

hl=target/release/hl
file=examples/probe.hl
seeds="${EXHAUSTIVE_SEEDS:-0..6}"
depth="${EXHAUSTIVE_DEPTH:-2}"
mutation_seeds="${EXHAUSTIVE_MUTATION_SEEDS:-0..1}"
targets="native-jsonl native-sqlite server-postgres cloudflare vercel"
scratch=output/check
mkdir -p "$scratch"

[ -x "$hl" ] || cargo build --release --locked

pids=()
for target in $targets; do
  "$hl" sim --exhaustive --depth "$depth" --target "$target" --seeds "$seeds" "$file" > "$scratch/exhaustive-$target.log" 2>&1 &
  pids+=($!)
done
failed=0
for pid in "${pids[@]}"; do
  if ! wait "$pid"; then
    failed=1
  fi
done
cat "$scratch"/exhaustive-*.log
if [ "$failed" -ne 0 ]; then
  echo "exhaustive: a clean exploration failed" >&2
  exit 1
fi

expected() {
  case $1 in
    flush_after_dispatch | gate_releases_early | recovery_reports_failed) echo unknown_reported ;;
    recovery_redispatches_external | vercel_retry_redispatches) echo external_once ;;
    postgres_no_fence | rerun_rewrites_call) echo seq_unique ;;
    jsonl_no_checksum) echo liveness ;;
    finish_before_flush) echo finish_once ;;
    idempotent_reported_unknown) echo idempotent_known ;;
    checkpoint_hash_order) echo replay_equal ;;
    recovery_skipped) echo panic ;;
    *) echo "no expected invariant for mutation $1" >&2; return 1 ;;
  esac
}

mutations="flush_after_dispatch recovery_redispatches_external vercel_retry_redispatches postgres_no_fence jsonl_no_checksum finish_before_flush idempotent_reported_unknown gate_releases_early recovery_reports_failed rerun_rewrites_call checkpoint_hash_order recovery_skipped"
caught=0
for mutation in $mutations; do
  invariant=$(expected "$mutation")
  for target in $targets; do
    set +e
    output=$("$hl" sim --exhaustive --depth 1 --target "$target" --seeds "$mutation_seeds" --mutation "$mutation" "$file" 2> /dev/null)
    status=$?
    set -e
    if [ "$status" -eq 2 ]; then
      continue
    fi
    if [ "$status" -ne 1 ] || ! grep -q "^FAIL invariant=$invariant " <<< "$output"; then
      echo "exhaustive: mutation $mutation on $target was not caught with $invariant in seeds $mutation_seeds at depth 1:" >&2
      echo "$output" >&2
      exit 1
    fi
    caught=$((caught + 1))
  done
done
echo "exhaustive: probe.hl seeds $seeds at depth $depth pass on every target; $caught mutation and target pairs caught with their invariant at depth 1 in seeds $mutation_seeds"

cell_file=examples/code_tasks.hl
cell_seeds="${EXHAUSTIVE_CELL_SEEDS:-0..2}"
pids=()
for target in $targets; do
  "$hl" sim --exhaustive --depth 1 --target "$target" --seeds "$cell_seeds" "$cell_file" > "$scratch/exhaustive-cells-$target.log" 2>&1 &
  pids+=($!)
done
failed=0
for pid in "${pids[@]}"; do
  if ! wait "$pid"; then
    failed=1
  fi
done
cat "$scratch"/exhaustive-cells-*.log
if [ "$failed" -ne 0 ]; then
  echo "exhaustive: a clean exploration of $cell_file failed" >&2
  exit 1
fi

cell_mutations="cell_header_rides_past_external:nested_external_once nested_flush_after_dispatch:nested_external_once cell_recovery_redispatches_external:nested_external_once cell_recovery_reports_failed:unknown_nested_reported cell_replay_ignores_journal:nested_external_once|seq_unique cell_rerun_after_finish:cell_finishes_once timeout_rides:header_before_wait nested_ids_restart:unknown_nested_reported|seq_unique vercel_nested_retry_redispatches:nested_external_once cell_gate_releases_early:nested_external_once postgres_cell_unfenced:nested_external_once nested_idempotent_reported_unknown:nested_known nested_enum_unchecked:nested_enum_valid"
caught=0
for entry in $cell_mutations; do
  mutation=${entry%%:*}
  invariants=${entry#*:}
  for target in $targets; do
    set +e
    output=$("$hl" sim --exhaustive --depth 1 --target "$target" --seeds "$mutation_seeds" --mutation "$mutation" "$cell_file" 2> /dev/null)
    status=$?
    set -e
    if [ "$status" -eq 2 ]; then
      continue
    fi
    if [ "$status" -ne 1 ] || ! grep -qE "^FAIL invariant=($invariants) " <<< "$output"; then
      echo "exhaustive: mutation $mutation on $target was not caught with $invariants in $cell_file seeds $mutation_seeds at depth 1:" >&2
      echo "$output" >&2
      exit 1
    fi
    caught=$((caught + 1))
  done
done
if "$hl" sim --exhaustive --target native-jsonl --seeds 0..1 --mutation divergence_dispatches "$cell_file" > /dev/null 2> "$scratch/stderr"; then
  echo "exhaustive: divergence_dispatches was not refused" >&2
  exit 1
fi
grep -q 'injects no engine divergence' "$scratch/stderr"
echo "exhaustive: $cell_file seeds $cell_seeds at depth 1 pass on every target; $caught cell mutation and target pairs caught with their invariant at depth 1 in seeds $mutation_seeds; divergence_dispatches refused"
