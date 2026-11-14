#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

file=$1
seeds="${FUZZ_SEEDS:-0..100}"
backends="${FUZZ_BACKENDS:-native-jsonl cloudflare vercel}"
opt="${FUZZ_RUST_OPT:-0}"
hl=target/release/hl
name=$(basename "$file" .hl)
root="${FUZZ_DIR:-output/fuzz/run}/$name"
mkdir -p "$root"

failed=0

fail() {
  echo "FAIL file=$file backend=$1: $2"
  failed=1
}

trace() {
  local target=$1 dir=$2
  if ! "$hl" sim --target "$target" --seeds "$seeds" --trace "$dir/trace.jsonl" "$file" > "$dir/sim.log" 2>&1; then
    fail "interpreter-$target" "$(grep -m1 -E '^FAIL|error' "$dir/sim.log" || head -c 400 "$dir/sim.log")"
    return 1
  fi
}

emit() {
  local target=$1 out=$2
  if ! "$hl" emit --target "$target" "$file" > "$out" 2> "$out.err"; then
    fail "emit-$target" "$(head -c 400 "$out.err")"
    return 1
  fi
}

store_of() {
  case "$1" in
    native-jsonl) echo jsonl ;;
    *) echo sql ;;
  esac
}

rust_one() {
  local target=$1 dir=$root/$1
  mkdir -p "$dir"
  emit "$target" "$dir/harness.rs" || return 0
  cp scripts/rust/sim_driver.rs "$dir/sim_driver.rs"
  cp "scripts/rust/sim_store_$(store_of "$target").rs" "$dir/store.rs"
  if ! rustc --edition 2024 -C opt-level="$opt" -o "$dir/driver" "$dir/sim_driver.rs" > "$dir/rustc.log" 2>&1; then
    fail "rustc-$target" "$(grep -m1 -A4 '^error' "$dir/rustc.log" | tr '\n' ' ' | head -c 600)"
    return 0
  fi
  trace "$target" "$dir" || return 0
  if ! "$dir/driver" "$dir/trace.jsonl" > "$dir/driver.log" 2>&1 || ! grep -q '^ok backend=rust' "$dir/driver.log"; then
    fail "rust-$target" "$(grep -m1 -E 'FAIL|panicked' "$dir/driver.log" | head -c 600 || head -c 600 "$dir/driver.log")"
    return 0
  fi
  echo "ok file=$file backend=rust-$target $(cat "$dir/driver.log")"
}

js_one() {
  local target=$1 dir=$root/$1
  mkdir -p "$dir"
  emit "$target" "$dir/harness.mjs" || return 0
  trace "$target" "$dir" || return 0
  if ! node scripts/js-sim.mjs --target "$target" --module "$dir/harness.mjs" --trace "$dir/trace.jsonl" > "$dir/js.log" 2>&1; then
    fail "js-$target" "$(grep -m1 -E 'FAIL|Error' "$dir/js.log" | head -c 600 || head -c 600 "$dir/js.log")"
    return 0
  fi
  echo "ok file=$file backend=js-$target $(cat "$dir/js.log")"
}

for target in $backends; do
  case "$target" in
    native-jsonl | native-sqlite | server-postgres) rust_one "$target" ;;
    cloudflare | vercel) js_one "$target" ;;
    *) echo "fuzz-one: unknown backend $target" >&2; exit 2 ;;
  esac
done
exit "$failed"
