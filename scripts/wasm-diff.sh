#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${WASM_DIFF_SEEDS:-0..2000}"
targets="${WASM_DIFF_TARGETS:-native-jsonl native-sqlite server-postgres cloudflare vercel}"
files="${WASM_DIFF_FILES:-$(echo examples/*.hl)}"
plant="${WASM_DIFF_PLANT:-none}"
host="${WASM_DIFF_HOST:-node}"
scope="${WASM_DIFF_SCOPE:-}"
scope_args=()
suffix=
if [ -n "$scope" ]; then
  scope_args=(--scope "$scope")
  suffix=-scoped
fi
hl=target/release/hl
out=output/wasm-diff/$plant/$host
mkdir -p "$out"

[ -x "$hl" ] || cargo build --release --locked

proved_bounds() {
  local file=$1 target=$2 log=$3 line
  if ! "$hl" prove --target "$target" "$file" > "$log" 2>&1; then
    echo "wasm-diff: hl prove failed $file on $target:" >&2
    cat "$log" >&2
    return 1
  fi
  while read -r line; do
    runner+=($line)
  done < <(sed -nE \
    -e "s/^  proved handler ([A-Za-z0-9_]+) on $target: worst_case: ([0-9]+) journal records per run .*/--records-max \1=\2/p" \
    -e "s/^  proved handler ([A-Za-z0-9_]+) on $target: worst_case: ([0-9]+) outbound requests <= .*/--requests-max \1=\2/p" \
    "$log")
}

diff_one() {
  local file=$1 target=$2
  local dir started ended runner
  dir="$out/$target-$(basename "$file" .hl)$suffix"
  mkdir -p "$dir"
  "$hl" emit --target "$target" --format wasm --plant "$plant" "$file" > "$dir/harness.wasm"
  if [ "$plant" != none ] && cmp -s <("$hl" emit --target "$target" --format wasm "$file") "$dir/harness.wasm"; then
    echo "wasm-diff: plant $plant changes nothing in $file for $target" >&2
    return 1
  fi
  started=$(date +%s%N)
  if ! "$hl" sim --target "$target" --seeds "$seeds" --trace "$dir/trace.jsonl" "${scope_args[@]}" "$file" > "$dir/sim.log" 2>&1; then
    echo "wasm-diff: the interpreter failed $file on $target:" >&2
    cat "$dir/sim.log" >&2
    return 1
  fi
  case $host in
    node) runner=(node scripts/wasm-sim.mjs --target "$target" --module "$dir/harness.wasm" --trace "$dir/trace.jsonl") ;;
    native)
      runner=("$hl" wasm-sim --target "$target" --module "$dir/harness.wasm" --trace "$dir/trace.jsonl")
      proved_bounds "$file" "$target" "$dir/prove.log" || return 1
      ;;
    platform)
      case $target in
        cloudflare | vercel) ;;
        *) echo "wasm-diff: the platform host runs cloudflare and vercel, not $target" >&2; return 1 ;;
      esac
      mkdir -p "$dir/host"
      cp "hosts/$target.mjs" "$dir/host/$target.mjs"
      if [ "$target" = vercel ]; then
        "$hl" emit --target vercel --format mjs --plant "$plant" "$file" > "$dir/host/program.mjs"
      else
        cp "$dir/harness.wasm" "$dir/host/program.wasm"
      fi
      runner=(node scripts/js-sim.mjs --target "$target" --module "$dir/host/$target.mjs" --trace "$dir/trace.jsonl")
      ;;
    *) echo "wasm-diff: unknown host $host" >&2; return 1 ;;
  esac
  if ! "${runner[@]}" > "$dir/$host.log" 2>&1; then
    echo "wasm-diff: $file on $target failed on the $host host:" >&2
    cat "$dir/$host.log" >&2
    return 1
  fi
  ended=$(date +%s%N)
  rm -f "$dir/trace.jsonl"
  echo "ok backend=wasm host=$host file=$file $(cut -d' ' -f3- "$dir/$host.log") wall_ms=$(( (ended - started) / 1000000 ))"
}

pids=()
for target in $targets; do
  (
    for file in $files; do
      diff_one "$file" "$target"
    done
  ) > "$out/$target.$$.log" 2>&1 &
  pids+=($!)
done
failed=0
for pid in "${pids[@]}"; do
  wait "$pid" || failed=1
done
for target in $targets; do
  cat "$out/$target.$$.log"
  rm -f "$out/$target.$$.log"
done
if [ "$failed" -ne 0 ]; then
  echo "wasm-diff: a differential run failed" >&2
  exit 1
fi
echo "wasm-diff: the wasm module matches the interpreter on seeds $seeds of $files on $targets ($host host)"
