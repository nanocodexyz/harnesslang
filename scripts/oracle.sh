#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${ORACLE_SEEDS:-0..500}"
mutation_seeds="${ORACLE_MUTATION_SEEDS:-0..200}"
targets="native-jsonl native-sqlite server-postgres cloudflare vercel"
hl=target/release/hl
oracle=oracle/target/release/hl-oracle
scratch=output/oracle
journals="$scratch/journals"

if grep -rnE '^\s*//|#\[test\]|#\[cfg\(test\)\]' oracle/src; then
  echo "oracle/src has comments or test functions" >&2
  exit 1
fi
cargo build --release --locked
cargo build --release --locked --manifest-path oracle/Cargo.toml
rm -rf "$scratch"
mkdir -p "$journals"

pids=()
for target in $targets; do
  (
    for file in examples/*.hl; do
      "$hl" sim --target "$target" --seeds "$seeds" --dump-journal "$journals" "$file"
    done
  ) > "$scratch/dump-$target.log" 2>&1 &
  pids+=($!)
done
failed=0
for pid in "${pids[@]}"; do
  if ! wait "$pid"; then
    failed=1
  fi
done
cat "$scratch"/dump-*.log
if [ "$failed" -ne 0 ]; then
  echo "oracle: a dump failed" >&2
  exit 1
fi
"$oracle" "$journals" | tee "$scratch/sim.log"
echo "oracle: sim dumps hold $(tail -1 "$scratch/sim.log" | grep -o 'cell_records=[0-9]*')"

cells="$scratch/cells"
"$oracle" --golden "$cells/golden"
"$oracle" --synthetic "$cells/synthetic"
for journal in "$cells"/synthetic/*/*.jsonl; do
  stem=$(basename "$journal" .jsonl)
  for target in $targets; do
    case "$stem" in
      *-"$target"-[0-9]*) "$hl" sim --target "$target" --summarize "$journal" examples/code_agent.hl > "${journal%.jsonl}.summary.jsonl" ;;
    esac
  done
done
"$oracle" "$cells/synthetic/good"
gap_log="$cells/synthetic/gap.log"
"$oracle" "$cells/synthetic/gap" > "$gap_log"
if ! grep -q 'rejected_by_both=1 ' "$gap_log"; then
  echo "oracle: a cell journal with a seq gap is not rejected by both folds" >&2
  cat "$gap_log" >&2
  exit 1
fi
echo "oracle: a cell journal with a seq gap is rejected by both folds"

expect_cell_disagreement() {
  local broken=$1 wanted=$2
  local log="$cells/synthetic/$broken.log"
  if "$oracle" "$cells/synthetic/$broken" > "$log"; then
    echo "oracle: broken cell journal $broken passed" >&2
    exit 1
  fi
  if ! grep -q "$wanted" "$log"; then
    echo "oracle: broken cell journal $broken failed for another reason:" >&2
    cat "$log" >&2
    exit 1
  fi
  echo "oracle disagrees with broken cell journal $broken: $wanted"
}

expect_cell_disagreement nested_call_before_its_entry "is recorded before its cell's Call entry"
expect_cell_disagreement unknown_delivered_as_failure "without delivering nanocode's unknown outcome"
expect_cell_disagreement site_result_differs "djs replaying cell 1 ends Done"

expect_disagreement() {
  local mutation=$1 target=$2
  local directory="$scratch/mutation-$mutation-$target"
  "$hl" sim --target "$target" --seeds "$mutation_seeds" --mutation "$mutation" --dump-journal "$directory" examples/agent.hl > /dev/null 2>&1 || true
  if "$oracle" "$directory" > "$directory.log"; then
    echo "oracle: mutation $mutation on $target wrote journals nanocode's fold agrees with" >&2
    exit 1
  fi
  echo "oracle disagrees under $mutation on $target: $(tail -1 "$directory.log" | grep -o 'disagreements=[0-9]*')"
}

expect_disagreement postgres_no_fence server-postgres
expect_disagreement idempotent_reported_unknown native-jsonl
expect_disagreement idempotent_reported_unknown cloudflare
expect_disagreement rerun_rewrites_call native-jsonl
expect_disagreement rerun_rewrites_call cloudflare
echo "oracle.sh: ok"
