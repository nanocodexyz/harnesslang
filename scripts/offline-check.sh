#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

case "${1:-}" in
  '' | --build) ;;
  *) echo 'usage: scripts/offline-check.sh [--build]' >&2; exit 2 ;;
esac
mkdir -p output
scratch=$(mktemp -d "$PWD/output/offline-check.XXXXXXXX")
export CARGO_HOME="$scratch/cargo-home"
export CARGO_TARGET_DIR="$scratch/target"
export SHIPPY_REMOTE_BUILD=0
mkdir -p "$CARGO_HOME"
python3 scripts/offline-vendor.py
for manifest in Cargo.toml oracle/Cargo.toml hosts/native/Cargo.toml scripts/fixtures/Cargo.toml compiler/bench/Cargo.toml; do
  name=${manifest//\//-}
  cargo +1.99.0 metadata --frozen --offline --format-version 1 --manifest-path "$manifest" > "$scratch/$name.json"
  if [ "${1:-}" = --build ]; then
    cargo +1.99.0 build --release --frozen --offline --manifest-path "$manifest"
  fi
done
node scripts/offline-npm.mjs "$scratch/npm" "$scratch/npm-cache"
echo "offline check: fresh Cargo and npm caches, logs in $scratch"
