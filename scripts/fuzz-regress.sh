#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${FUZZ_REGRESS_SEEDS:-0..20}"
backends="native-jsonl native-sqlite server-postgres cloudflare vercel"
root=output/fuzz/regress
rm -rf "$root"
mkdir -p "$root"
failed=0
for file in scripts/fuzz-regressions/*.hl; do
  if ! FUZZ_SEEDS=$seeds FUZZ_BACKENDS=$backends FUZZ_DIR=$root scripts/fuzz-one.sh "$file"; then
    failed=1
  fi
done
for file in scripts/fuzz-regressions/unproved/*.hl; do
  if target/release/hl prove "$file" > "$root/$(basename "$file" .hl).prove.log" 2>&1; then
    echo "fuzz-regress: $file proves, so it no longer covers a program the prover refuses" >&2
    failed=1
  fi
  if ! FUZZ_UNPROVED_ARITH=1 FUZZ_SEEDS=$seeds FUZZ_BACKENDS=$backends FUZZ_DIR=$root scripts/fuzz-one.sh "$file"; then
    failed=1
  fi
done
generated="${FUZZ_REGRESS_PROGRAMS:-0..400}"
if ! target/release/hl gen --seeds "$generated" --out "$root/generated" > "$root/gen.log" 2>&1; then
  echo "fuzz-regress: hl gen wrote a program that fails to parse or check: $(grep -m1 -E 'panicked|error' "$root/gen.log")" >&2
  failed=1
fi
if ! target/release/hl fuzz-prove --seeds "$generated" --sim-seeds 5; then
  failed=1
fi
if [ "$failed" -ne 0 ]; then
  echo "fuzz-regress: a regression program diverged or the generator broke" >&2
  exit 1
fi
echo "fuzz-regress: ok programs=$(ls scripts/fuzz-regressions/*.hl scripts/fuzz-regressions/unproved/*.hl | wc -l) seeds=$seeds generated=$generated"
