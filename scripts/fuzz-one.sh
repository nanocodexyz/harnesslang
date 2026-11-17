#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

file=$1
seeds="${FUZZ_SEEDS:-0..100}"
backends="${FUZZ_BACKENDS:-native-jsonl cloudflare vercel}"
hl=target/release/hl
name=$(basename "$file" .hl)
root="${FUZZ_DIR:-output/fuzz/run}/$name"
mkdir -p "$root"

failed=0

host_of() {
  case "$1" in
    native-jsonl | native-sqlite | server-postgres) echo native ;;
    cloudflare | vercel) echo platform ;;
    *) echo "fuzz-one: unknown backend $1" >&2; exit 2 ;;
  esac
}

for target in $backends; do
  host=$(host_of "$target")
  if ! "$hl" emit --format wasm --target "$target" "$file" > /dev/null 2> "$root/$target.emit.err"; then
    echo "FAIL file=$file backend=emit-$target: $(head -c 400 "$root/$target.emit.err")"
    failed=1
    continue
  fi
  if ! WASM_DIFF_UNPROVED_ARITH="${FUZZ_UNPROVED_ARITH:-0}" WASM_DIFF_OUT="$root/$target" WASM_DIFF_HOST=$host WASM_DIFF_SEEDS=$seeds WASM_DIFF_TARGETS=$target WASM_DIFF_FILES=$file scripts/wasm-diff.sh > "$root/$target.log" 2>&1; then
    echo "FAIL file=$file backend=wasm-$target: $(grep -m1 -E 'FAIL|error|failed' "$root/$target.log" | head -c 600)"
    failed=1
    continue
  fi
  echo "$(grep -m1 '^ok backend=wasm' "$root/$target.log")"
done
exit "$failed"
