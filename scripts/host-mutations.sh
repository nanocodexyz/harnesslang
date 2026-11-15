#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${HOST_MUTATION_SEEDS:-0..100}"
hl=target/release/hl
out=output/host-mutations
mkdir -p "$out"
[ -x "$hl" ] || cargo build --release --locked

caught() {
  local mutation=$1 file=$2
  local name dir
  name=$(basename "$file" .hl)
  dir="$out/$mutation-$name"
  mkdir -p "$dir/host"
  "$hl" emit --target vercel --format mjs "$file" > "$dir/host/program.mjs"
  "$hl" emit --format source --target vercel "$file" > "$dir/generated.mjs"
  "$hl" sim --target vercel --seeds "$seeds" --trace "$dir/trace.jsonl" "$file" > /dev/null
  node oracle/cloudflare/mutate.mjs "$mutation" hosts/vercel.mjs "$dir/host/vercel.mjs"
  if node scripts/js-sim.mjs --target vercel --module "$dir/host/vercel.mjs" --runtime "$dir/generated.mjs" --trace "$dir/trace.jsonl" > "$dir/log" 2>&1; then
    echo "host mutations: $mutation survived $file on the fixed Vercel host" >&2
    exit 1
  fi
  if ! grep -q '^FAIL invariant=backend_equal' "$dir/log"; then
    echo "host mutations: $mutation failed $file for another reason than backend_equal:" >&2
    cat "$dir/log" >&2
    exit 1
  fi
  echo "host mutations: $mutation caught on $file: $(grep -m1 '^FAIL' "$dir/log" | cut -c1-160)"
}

caught vercel_step_continues examples/minimal.hl
caught vercel_ack_unhooked examples/minimal.hl
caught vercel_resumed_dropped examples/hooks.hl
echo "host mutations: ok"
