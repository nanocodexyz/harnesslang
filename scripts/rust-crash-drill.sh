#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

sessions="${DRILL_SESSIONS:-16}"
turns="${DRILL_TURNS:-6}"
delay_ms_max="${DRILL_DELAY_MS_MAX:-12}"
kills_per_session_max="${DRILL_KILLS_MAX:-400}"
file="${DRILL_FILE:-examples/agent.hl}"
seed_base="${DRILL_SEED:-0}"
mutation="${DRILL_MUTATION:-none}"
hl=target/release/hl
out=output/rust-drill
flush_before_external='if site.durability.flushes(call.effect) {'
case "$mutation" in
  none) ;;
  flush_after_dispatch) out="output/rust-drill-$mutation" ;;
  *)
    echo "drill: unknown DRILL_MUTATION $mutation (none or flush_after_dispatch)" >&2
    exit 2
    ;;
esac
killed_status=137
mkdir -p "$out"

finished=0
report() {
  local status=$?
  if [ "$finished" -ne 1 ]; then
    echo "drill: FAILED (exit $status)"
    [ "$status" -ne 0 ] || status=1
  fi
  exit "$status"
}
trap report EXIT

cargo build --release --locked -j2 --quiet

"$hl" emit --target native-jsonl "$file" > "$out/harness.rs.new"
if [ "$mutation" = flush_after_dispatch ]; then
  if [ "$(grep -cF "$flush_before_external" "$out/harness.rs.new")" -ne 1 ]; then
    echo "drill: the generated code no longer has one flush before an external effect to mutate" >&2
    exit 1
  fi
  sed -i "s/$flush_before_external/if false \&\& site.durability.flushes(call.effect) {/" "$out/harness.rs.new"
fi
cp scripts/rust/drill_host.rs "$out/drill_host.rs"
mv "$out/harness.rs.new" "$out/harness.rs"
key=$(cat "$out/harness.rs" "$out/drill_host.rs" | cksum | cut -d' ' -f1)
if [ ! -x "$out/drill_host" ] || [ "$(cat "$out/key" 2>/dev/null)" != "$key" ]; then
  rustc --edition 2024 -O -o "$out/drill_host" "$out/drill_host.rs"
  echo "$key" > "$out/key"
fi

total_kills=0
total_runs=0
for session in $(seq 1 "$sessions"); do
  dir="$out/session-$session"
  rm -rf "$dir"
  mkdir -p "$dir"
  seed=$((seed_base + session))
  RANDOM=$seed
  kills=0
  runs=0
  while true; do
    runs=$((runs + 1))
    "$out/drill_host" run "$dir/journal.jsonl" "$dir/ledger" "$dir/acks" "$seed" "$turns" 2>> "$dir/stderr" &
    pid=$!
    delay_ms=$((RANDOM % (delay_ms_max + 1)))
    sleep "$(printf '0.%03d' "$delay_ms")"
    kill -9 "$pid" 2> /dev/null || true
    status=0
    { wait "$pid"; } 2> /dev/null || status=$?
    if [ "$status" -eq 0 ]; then
      break
    fi
    if [ "$status" -ne "$killed_status" ]; then
      echo "drill: session $session run $runs exited $status:" >&2
      cat "$dir/stderr" >&2
      exit 1
    fi
    kills=$((kills + 1))
    if [ "$kills" -ge "$kills_per_session_max" ]; then
      echo "drill: session $session was killed kills_per_session_max ($kills_per_session_max) times without finishing" >&2
      exit 1
    fi
  done
  verified=$("$out/drill_host" verify "$dir/journal.jsonl" "$dir/ledger" "$dir/acks" "$turns" 2>&1) || {
    echo "drill: session $session failed verification after $kills kills:" >&2
    echo "$verified" >&2
    exit 1
  }
  torn=$(grep -c '^dropped ' "$dir/stderr" || true)
  echo "session=$session seed=$seed kills=$kills opens_with_torn_tail=$torn $verified"
  total_kills=$((total_kills + kills))
  total_runs=$((total_runs + runs))
done
echo "drill: $sessions sessions of $turns turns (seeds $((seed_base + 1))..$((seed_base + sessions))) survived $total_kills kill -9s over $total_runs runs: external_once, unknown_reported, idempotent_known and finish_once hold on the real files"
finished=1
