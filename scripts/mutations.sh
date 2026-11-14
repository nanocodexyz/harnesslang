#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${MUTATION_SEEDS:-0..200}"
hl=target/release/hl
targets="native-jsonl native-sqlite server-postgres cloudflare vercel"
mutations="flush_after_dispatch recovery_redispatches_external vercel_retry_redispatches postgres_no_fence jsonl_no_checksum finish_before_flush idempotent_reported_unknown gate_releases_early recovery_reports_failed rerun_rewrites_call checkpoint_hash_order recovery_skipped enum_unchecked model_failure_unrecorded retire_skipped seal_skipped"

file_of() {
  case $1 in
    enum_unchecked) echo examples/hooks.hl ;;
    model_failure_unrecorded) echo examples/claude_agent.hl ;;
    retire_skipped | seal_skipped) echo docs/snippets/compaction.hl ;;
    *) echo examples/agent.hl ;;
  esac
}

[ -x "$hl" ] || cargo build --release --locked

for mutation in $mutations; do
  file=$(file_of "$mutation")
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
echo "mutations: every mutation is caught"
