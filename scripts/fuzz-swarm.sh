#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

programs="${1:-0..20}"
jobs="${FUZZ_JOBS:-2}"
export FUZZ_SEEDS="${FUZZ_SEEDS:-0..20}"
rotate="${FUZZ_ROTATE:-1}"
rust_targets="native-jsonl native-sqlite server-postgres"
js_targets="cloudflare vercel"
exhaustive_seeds="${FUZZ_EXHAUSTIVE_SEEDS:-0..2}"
exhaustive_depth="${FUZZ_EXHAUSTIVE_DEPTH:-1}"
exhaustive_targets="${FUZZ_EXHAUSTIVE_TARGETS:-native-jsonl native-sqlite server-postgres cloudflare vercel}"
root="${FUZZ_ROOT:-output/fuzz/swarm-${programs/../-}}"
hl=target/release/hl

[ -x "$hl" ] || cargo build --release --locked
rm -rf "$root"
mkdir -p "$root/programs" "$root/logs" "$root/run"
"$hl" gen --seeds "$programs" --out "$root/programs" > "$root/gen.log"

one() {
  local file=$1 name
  name=$(basename "$file" .hl)
  local log=$root/logs/$name.log dir=$root/run/$name
  local status=0
  mkdir -p "$dir"
  local backends="$rust_targets $js_targets"
  if [ "$rotate" = 1 ]; then
    local index=${name#fuzz}
    local rust=($rust_targets)
    backends="${rust[index % ${#rust[@]}]} $js_targets"
  fi
  FUZZ_BACKENDS=$backends FUZZ_DIR=$root/run scripts/fuzz-one.sh "$file" > "$log" 2>&1 || status=1
  if [ "$exhaustive_depth" -gt 0 ]; then
    for target in $exhaustive_targets; do
      if ! "$hl" sim --exhaustive --depth "$exhaustive_depth" --target "$target" --seeds "$exhaustive_seeds" "$file" > "$dir/exhaustive-$target.log" 2>&1; then
        echo "FAIL file=$file backend=exhaustive-$target: $(grep -m1 -E '^FAIL|error|panicked' "$dir/exhaustive-$target.log" | head -c 600)" >> "$log"
        status=1
      fi
    done
  fi
  if [ "$status" -eq 0 ]; then
    rm -rf "$dir"
    echo "ok $name"
  else
    echo "FAIL $name"
  fi
}
export -f one
export root hl exhaustive_seeds exhaustive_depth exhaustive_targets rotate rust_targets js_targets

start=$SECONDS
find "$root/programs" -name '*.hl' | sort -V | xargs -P "$jobs" -n 1 bash -c 'one "$1"' _ > "$root/results.txt" || true
elapsed=$((SECONDS - start))
total=$(wc -l < "$root/results.txt")
failed=$(grep -c '^FAIL' "$root/results.txt" || true)
{ grep -h '^FAIL' "$root"/logs/*.log || true; } | sed -E 's/^FAIL file=[^ ]* //' | cut -c1-200 | sort | uniq -c | sort -rn > "$root/failures.txt"
echo "fuzz-swarm: programs=$total failed=$failed jobs=$jobs seconds=$elapsed programs_per_hour=$((total * 3600 / (elapsed > 0 ? elapsed : 1))) root=$root"
cat "$root/failures.txt"
[ "$failed" -eq 0 ]
