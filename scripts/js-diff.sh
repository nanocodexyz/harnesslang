#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${JS_DIFF_SEEDS:-0..500}"
targets="${JS_DIFF_TARGETS:-cloudflare vercel}"
files="${JS_DIFF_FILES:-examples/*.hl}"
mutation="${JS_DIFF_MUTATION:-none}"
hl=target/release/hl
out=output/js-diff/$mutation
mkdir -p "$out"

[ -x "$hl" ] || cargo build --release --locked

diff_one() {
  local file=$1 target=$2
  local name
  name=$(basename "$file" .hl)
  local dir="$out/$target-$name"
  mkdir -p "$dir"
  "$hl" emit --target "$target" "$file" > "$dir/harness.mjs"
  local status=0
  "$hl" sim --target "$target" --seeds "$seeds" --mutation "$mutation" --trace "$dir/trace.jsonl" "$file" > "$dir/sim.log" 2>&1 || status=$?
  if [ "$mutation" = none ] && [ "$status" -ne 0 ]; then
    echo "js-diff: the interpreter failed $file on $target:" >&2
    cat "$dir/sim.log" >&2
    return 1
  fi
  node scripts/js-sim.mjs --target "$target" --module "$dir/harness.mjs" --trace "$dir/trace.jsonl" --mutation "$mutation" > "$dir/js.log" 2>&1 || status=$?
  if [ "$status" -ne 0 ]; then
    echo "js-diff: $file on $target failed:" >&2
    tail -n 5 "$dir/sim.log" "$dir/js.log" >&2
    return 1
  fi
  cat "$dir/js.log"
}

pids=()
for target in $targets; do
  (
    for file in $files; do
      diff_one "$file" "$target"
    done
  ) > "$out/$target.log" 2>&1 &
  pids+=($!)
done
failed=0
for pid in "${pids[@]}"; do
  wait "$pid" || failed=1
done
for target in $targets; do
  cat "$out/$target.log"
done
if [ "$failed" -ne 0 ]; then
  echo "js-diff: a differential run failed" >&2
  exit 1
fi
echo "js-diff: generated JavaScript matches the interpreter on seeds $seeds of every example and target"
