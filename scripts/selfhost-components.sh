#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$#" -ne 0 ]; then
  echo 'usage: scripts/selfhost-components.sh' >&2
  exit 2
fi
hl="$PWD/target/release/hl"
host="$PWD/hosts/native/target/release/hl-host"
[ -x "$hl" ] && [ -x "$host" ]
rustfmt --edition 2024 --check scripts/selfhost-layout-oracle.rs scripts/selfhost-catalog-oracle.rs scripts/selfhost-expr-oracle.rs
mkdir -p output
work=$(mktemp -d "$PWD/output/selfhost-components.XXXXXXXX")
echo "selfhost components: retaining inputs, logs and results in $work"
wat="$work/encoder-original.wat"
wat_sha=148575c5ff0a8c7132f6642cd3dc1a1be6664ad54e421d2a02928689010e7425
wasm_sha=68b59d50dc5df6e8d5b861ae64c6250ab3d0fb029565151fda18e7998c2a03fb
"$hl" emit --target native-jsonl --format wat compiler/encode.hl > "$wat"
printf '%s  %s\n' "$wat_sha" "$wat" | sha256sum --check
"$hl" assemble < "$wat" > "$work/encoder-original.wasm"
printf '%s  %s\n' "$wasm_sha" "$work/encoder-original.wasm" | sha256sum --check
python3 scripts/selfhost-layout.py --allocated --runs 3 --hl "$hl" --host "$host" \
  --encoder-wat "$wat" --encoder-sha256 "$wat_sha" --out "$work/layout"
if ! rustc --edition=2024 -O scripts/selfhost-catalog-oracle.rs -o "$work/catalog-oracle" > "$work/catalog-oracle-build.log" 2>&1; then
  cat "$work/catalog-oracle-build.log" >&2
  exit 1
fi
python3 scripts/selfhost-catalog.py --allocated --runs 3 --hl "$hl" --host "$host" \
  --oracle "$work/catalog-oracle" --encoder-wat "$wat" --out "$work/catalog"
python3 scripts/selfhost-expr.py --allocated --runs 3 --hl "$hl" --host "$host" \
  --catalog-root "$PWD" --encoder-wat "$wat" --out "$work/expressions"
echo 'selfhost components: three fresh differential repetitions passed; no compiler bootstrap claimed'
