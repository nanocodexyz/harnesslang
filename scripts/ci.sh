#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

export CHECK_JOBS=${CHECK_JOBS:-4}
export SEEDS=${SEEDS:-0..300}
export TWICE_SEEDS=${TWICE_SEEDS:-0..60}
export WASM_PLATFORM_DIFF_SEEDS=${WASM_PLATFORM_DIFF_SEEDS:-0..10}
export WASM_DIFF_SEEDS=${WASM_DIFF_SEEDS:-0..40}
export WASM_NATIVE_DIFF_SEEDS=${WASM_NATIVE_DIFF_SEEDS:-0..10}
unset HL_LONG

started=$(date +%s)
cargo build --release --locked
scripts/check.sh
scripts/gate.sh --prove
scripts/gate.sh
echo "ci: passed in $(( $(date +%s) - started )) s at $(git rev-parse --short HEAD) on $(hostname)"
