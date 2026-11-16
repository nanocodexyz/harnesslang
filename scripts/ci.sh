#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

export CHECK_JOBS=${CHECK_JOBS:-4}
export SEEDS=${SEEDS:-0..300}
export TWICE_SEEDS=${TWICE_SEEDS:-0..60}
export RUST_DIFF_SEEDS=${RUST_DIFF_SEEDS:-0..40}
export JS_DIFF_SEEDS=${JS_DIFF_SEEDS:-0..40}
export WASM_DIFF_SEEDS=${WASM_DIFF_SEEDS:-0..40}
export WASM_NATIVE_DIFF_SEEDS=${WASM_NATIVE_DIFF_SEEDS:-0..10}
unset HL_LONG

context="CI Gate"
repo=nanocodexyz/harnesslang

post() {
  gh api -X POST "repos/$repo/statuses/$1" -f state="$2" -f context="$context" -f description="$3" > /dev/null
}

case "${1:-}" in
  "")
    started=$(date +%s)
    cargo build --release --locked
    scripts/check.sh
    scripts/gate.sh --prove
    scripts/gate.sh
    echo "ci: passed in $(( $(date +%s) - started )) s"
    ;;
  --post)
    [ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "ci: --post needs a clean checkout" >&2; exit 1; }
    sha=$(git rev-parse HEAD)
    post "$sha" pending "scripts/ci.sh running on $(hostname)"
    started=$(date +%s)
    if "$0" > output/ci.log 2>&1; then
      post "$sha" success "scripts/ci.sh passed in $(( $(date +%s) - started )) s on $(hostname)"
      tail -n 3 output/ci.log
    else
      post "$sha" failure "scripts/ci.sh failed on $(hostname): see output/ci.log"
      tail -n 40 output/ci.log
      exit 1
    fi
    ;;
  *)
    echo "usage: scripts/ci.sh [--post]" >&2
    exit 1
    ;;
esac
