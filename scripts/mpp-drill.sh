#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

runs="${MPP_DRILL_RUNS:-6}"
pay_sleep_ms="${MPP_DRILL_PAY_SLEEP_MS:-1200}"
seed="${MPP_DRILL_SEED:-19}"
model="${MPP_DRILL_MODEL:-gpt-6-luna}"
charge=20000
charge_max=50000
out=output/mpp-drill
target=hosts/native/target
agent=$target/release/hl-host
fixtures=scripts/fixtures/target/release
killed_status=137

finished=0
pids=()
report() {
  local status=$?
  for pid in "${pids[@]}"; do
    kill "$pid" 2> /dev/null || true
  done
  if [ "$finished" -ne 1 ]; then
    echo "mpp-drill: FAILED (exit $status)"
    [ "$status" -ne 0 ] || status=1
  fi
  exit "$status"
}
trap report EXIT

cargo build --release --locked -j2 --quiet
CARGO_TARGET_DIR="$(pwd)/$target" HL_BIN="$(pwd)/target/release/hl" cargo build --release --locked -j2 --quiet --manifest-path hosts/native/Cargo.toml
cargo build --release --locked -j2 --quiet --manifest-path scripts/fixtures/Cargo.toml

rm -rf "$out"
mkdir -p "$out"
out=$(cd "$out" && pwd)
printf '[{"answer":"Hello from the paid model."},{"answer":"Checked: nothing to verify."},{"answer":"Checked again."}]\n' > "$out/script.json"
printf 'Say hello.\n' > "$out/instruction.md"

wait_port() {
  for _ in $(seq 1 100); do
    [ -s "$1" ] && return 0
    sleep 0.05
  done
  echo "mpp-drill: no port in $1" >&2
  exit 1
}

"$fixtures/mock-responses" "$out/model.port" "$out/script.json" 20 2> "$out/model.log" &
pids+=($!)
wait_port "$out/model.port"
"$fixtures/mpp-gate" serve "$out/gate.port" "$(cat "$out/model.port")" "$out/ledger" "$charge" 2> "$out/gate.log" &
pids+=($!)
wait_port "$out/gate.port"
gate="http://127.0.0.1:$(cat "$out/gate.port")"

run_agent() {
  local run=$1
  env -u OPENAI_API_KEY -u OPENAI_BASE_URL -u ANTHROPIC_API_KEY -u ANTHROPIC_BASE_URL \
    HOME="$run/home" HL_MPP_BASE_URL="$gate" HL_MPP_PAYER="$(pwd)/$fixtures/mpp-pay" MPP_CHAIN_URL="$gate" MPP_PAY_SLEEP_MS="$pay_sleep_ms" \
    "$agent" run --instruction "$out/instruction.md" --cwd "$run/ws" --model "$model" --journal "$run/journal" >> "$run/agent.out" 2>> "$run/agent.err"
}

RANDOM=$seed
clean=0
killed=0
refused=0
for index in $(seq 0 "$runs"); do
  run=$out/run-$index
  mkdir -p "$run/ws" "$run/home"
  if [ "$index" -eq 0 ]; then
    run_agent "$run"
    grep -q '^used ' "$out/ledger" || { echo "mpp-drill: the paid run was never served" >&2; tail -n 5 "$run/agent.err" >&2; exit 1; }
    clean=1
    continue
  fi
  run_agent "$run" &
  pid=$!
  for _ in $(seq 1 400); do
    grep -q "exec_command: c='Payment " "$run/agent.err" 2> /dev/null && break
    sleep 0.01
  done
  delay_ms=$(( RANDOM % (pay_sleep_ms * 3) ))
  sleep "$(printf '%d.%03d' $((delay_ms / 1000)) $((delay_ms % 1000)))"
  pkill -KILL -P "$pid" 2> /dev/null || true
  kill -KILL "$pid" 2> /dev/null || true
  set +e
  wait "$pid" 2> /dev/null
  status=$?
  set -e
  if [ "$status" -eq "$killed_status" ]; then
    killed=$((killed + 1))
    echo "mpp-drill: run $index killed ${delay_ms} ms into the payment" >> "$out/kills.log"
  fi
  sleep "$(printf '%d.%03d' $(((pay_sleep_ms * 3) / 1000)) $(((pay_sleep_ms * 3) % 1000)))"
  set +e
  run_agent "$run"
  status=$?
  set -e
  if grep -q 'not paid again' "$run/agent.out" "$run/agent.err" 2> /dev/null; then
    refused=$((refused + 1))
  fi
done

judged=$("$fixtures/mpp-gate" judge "$out/ledger" "$charge_max")
echo "$judged"
case "$judged" in
  *" paid_twice=0 over_charge=0 "*) ;;
  *)
    echo "mpp-drill: a challenge was paid twice or above the cap" >&2
    exit 1
    ;;
esac
finished=1
echo "mpp-drill: ok model=$model clean=$clean killed=$killed unknown_not_repaid=$refused runs=$runs"
