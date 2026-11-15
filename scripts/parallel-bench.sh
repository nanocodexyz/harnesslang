#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

turns="${BENCH_TURNS:-10}"
calls="${BENCH_CALLS:-16}"
latency_ms="${BENCH_LATENCY_MS:-20}"
rounds="${BENCH_ROUNDS:-3}"
hl=target/release/hl
out=output/parallel-bench
mkdir -p "$out"

[ -x "$hl" ] || cargo build --release --locked

cat > "$out/parallel.hl" <<EOF
harness batch

const calls_max: u64 = $calls
const context_max: u64 = 4096

tool Read(path: text) -> text effect none

model think calls calls_max

state {
  context: list<entry, context_max>
}

on ask(question: text) -> text
  requires len(context) + 2 + calls_max <= context_max
{
  push context user(question)
  let reply = think(context)
  push context assistant(reply)
  match reply {
    answer(words) => { return words }
    calls(requested) => {
      for call in requested parallel {
        match dispatch(call) {
          ok(output) => { push context result(call, output) }
          failed(error) => { push context error(call, error) }
          unknown => { push context unknown(call) }
        }
      }
      return "ran the calls"
    }
  }
}
EOF
sed 's/ in requested parallel {/ in requested {/' "$out/parallel.hl" > "$out/sequential.hl"
if cmp -s "$out/parallel.hl" "$out/sequential.hl"; then
  echo "parallel-bench: the sequential program is the parallel one" >&2
  exit 1
fi

build() {
  local name=$1 dir="$out/$1"
  mkdir -p "$dir"
  "$hl" emit --format source --target native-jsonl "$out/$name.hl" > "$dir/harness.rs"
  cp scripts/rust/parallel_bench.rs "$dir/main.rs"
  rustc --edition 2024 -C opt-level=2 -o "$dir/bench" "$dir/main.rs"
}
build parallel
build sequential

for round in $(seq "$rounds"); do
  for variant in "sequential serial" "parallel serial" "parallel concurrent"; do
    set -- $variant
    journal="$out/$1-$2.jsonl"
    rm -f "$journal"
    echo "round=$round program=$1 $("$out/$1/bench" "$journal" "$2" "$turns" "$calls" "$latency_ms")"
  done
done
