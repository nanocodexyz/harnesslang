#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

hl=target/release/hl
host=hosts/native/target/release/hl-host
out=output/selfhost-encode
encoder=$out/encode.wasm
targets="${SELFHOST_TARGETS:-native-jsonl native-sqlite server-postgres cloudflare vercel}"

fail() {
  echo "selfhost-encode: $*" >&2
  exit 1
}

hex_of() {
  od -An -v -tx1 "$1" | tr -d ' \n'
}

prepare() {
  [ -x "$hl" ] || cargo build --release --locked
  cargo build --release --locked --quiet --manifest-path hosts/native/Cargo.toml
  mkdir -p "$out"
  "$hl" check compiler/encode.hl > "$out/check.log" 2>&1 || { cat "$out/check.log" >&2; fail "compiler/encode.hl does not check"; }
  "$hl" emit --format wasm --target native-jsonl compiler/encode.hl > "$encoder.tmp"
  mv "$encoder.tmp" "$encoder"
}

one() {
  local file=$1 target=$2 dir
  dir="$out/$target-$(basename "$(dirname "$file")")-$(basename "$file" .hl)"
  rm -rf "$dir"
  mkdir -p "$dir"
  "$hl" emit --format wasm --target "$target" "$file" > "$dir/rust.wasm" 2> /dev/null || return 2
  "$hl" emit --format flat --target "$target" "$file" > "$dir/module.flat"
  hex_of "$dir/rust.wasm" > "$dir/rust.hex"
  "$hl" run compiler/encode.hl --handler encode --arg-file "$dir/module.flat" > "$dir/run.jsonl"
  grep -o '"finished":{"last":[0-9]*,"outcome":"completed","result":"[0-9a-f]*"' "$dir/run.jsonl" \
    | sed 's/.*"result":"//; s/"$//' | tr -d '\n' > "$dir/interpreter.hex"
  cmp -s "$dir/interpreter.hex" "$dir/rust.hex" \
    || fail "$file on $target: the interpreter's encode.hl bytes differ from encode.rs (see $dir)"
  "$host" run --module "$encoder" --journal "$dir/journal" --cwd "$dir" --handler compile \
    --args '{"input_path":"module.flat","output_path":"hl.wasm"}' > "$dir/host.out" 2> "$dir/host.err" \
    || fail "$file on $target: hl-host failed running encode.wasm (see $dir/host.err)"
  cmp -s "$dir/hl.wasm" "$dir/rust.wasm" \
    || fail "$file on $target: the compiled encode.wasm bytes differ from encode.rs (see $dir)"
  echo "ok selfhost-encode file=$file target=$target module_bytes=$(wc -c < "$dir/rust.wasm")"
}

if [ "${1:-}" = --prepare ]; then
  prepare
  exit 0
fi
[ -f "$encoder" ] && [ -x "$host" ] || prepare
files=("$@")
[ "${#files[@]}" -gt 0 ] || files=(examples/*.hl docs/snippets/*.hl)
modules=0
for file in "${files[@]}"; do
  for target in $targets; do
    status=0
    one "$file" "$target" || status=$?
    case $status in
      0) modules=$((modules + 1)) ;;
      2) ;;
      *) exit "$status" ;;
    esac
  done
done
echo "selfhost-encode: compiler/encode.hl writes the same bytes as encode.rs for $modules module(s) under the interpreter and as wasm under hl-host"
