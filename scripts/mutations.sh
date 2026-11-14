#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${MUTATION_SEEDS:-0..200}"
hl=target/release/hl
file=examples/agent.hl
targets="native-jsonl native-sqlite server-postgres cloudflare vercel"
mutations="flush_after_dispatch recovery_redispatches_external vercel_retry_redispatches postgres_no_fence jsonl_no_checksum finish_before_flush idempotent_reported_unknown gate_releases_early recovery_reports_failed rerun_rewrites_call checkpoint_hash_order recovery_skipped"

[ -x "$hl" ] || cargo build --release --locked

for mutation in $mutations; do
  applied=0
  for target in $targets; do
    set +e
    output=$("$hl" sim --target "$target" --seeds "$seeds" --twice --mutation "$mutation" "$file" 2>&1)
    status=$?
    set -e
    if [ "$status" -eq 2 ]; then
      continue
    fi
    applied=$((applied + 1))
    if [ "$status" -ne 1 ] || ! grep -q '^FAIL invariant=' <<< "$output"; then
      echo "mutation $mutation on $target was not caught in seeds $seeds:" >&2
      echo "$output" >&2
      exit 1
    fi
    echo "caught $mutation on $target: $(grep -m1 '^FAIL invariant=' <<< "$output" | cut -d: -f1)"
  done
  if [ "$applied" -eq 0 ]; then
    echo "mutation $mutation applies to no target" >&2
    exit 1
  fi
done
cell_seeds="${CELL_MUTATION_SEEDS:-0..1000}"
cell_file=examples/code_agent.hl
cell_mutations="cell_header_rides_past_external:nested_external_once nested_flush_after_dispatch:nested_external_once cell_recovery_redispatches_external:nested_external_once cell_recovery_reports_failed:unknown_nested_reported cell_replay_ignores_journal:nested_external_once cell_rerun_after_finish:cell_finishes_once timeout_rides:header_before_wait divergence_dispatches:cell_replay_equal nested_ids_restart:seq_unique cell_limit_unchecked:cell_bounded vercel_nested_retry_redispatches:nested_external_once cell_gate_releases_early:nested_external_once postgres_cell_unfenced:nested_external_once nested_idempotent_reported_unknown:nested_known"
for entry in $cell_mutations; do
  mutation=${entry%%:*}
  expected=${entry#*:}
  applied=0
  for target in $targets; do
    set +e
    output=$("$hl" sim --target "$target" --seeds "$cell_seeds" --mutation "$mutation" "$cell_file" 2>&1)
    status=$?
    set -e
    if [ "$status" -eq 2 ]; then
      continue
    fi
    if grep -q 'cells: unsupported' <<< "$output"; then
      continue
    fi
    applied=$((applied + 1))
    if [ "$status" -ne 1 ] || ! grep -q "^FAIL invariant=$expected " <<< "$output"; then
      echo "mutation $mutation on $target was not caught by $expected in seeds $cell_seeds:" >&2
      echo "$output" >&2
      exit 1
    fi
    echo "caught $mutation on $target: $(grep -m1 '^FAIL invariant=' <<< "$output" | cut -d: -f1)"
  done
  if [ "$applied" -eq 0 ]; then
    echo "mutation $mutation applies to no target" >&2
    exit 1
  fi
done
echo "mutations: every mutation is caught"
