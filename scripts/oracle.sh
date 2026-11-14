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
"$oracle" "$journals"

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
