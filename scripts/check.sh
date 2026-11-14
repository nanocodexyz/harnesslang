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
    "$hl" check --target "$target" "$file" > /dev/null
  done
done
echo "check: every example passes on every target"

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
expect_error examples/invalid/over_cloudflare.hl cloudflare "handler 'prompt': worst case 17408 outbound requests exceeds cloudflare's limit of 10000 outbound requests per invocation; loops: step below turn_steps_max (1024) x call below calls_per_step_max (16)"
for target in native-jsonl native-sqlite server-postgres vercel; do
  "$hl" check --target "$target" examples/invalid/over_cloudflare.hl > /dev/null
done
echo "check: every invalid example fails with its expected error"

pids=()
for target in $targets; do
  (
    for file in examples/*.hl; do
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
fi
echo "check.sh: ok"
