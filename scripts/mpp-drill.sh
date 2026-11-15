#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${MPP_DRILL_SEEDS:-1..4}"
kills_max="${MPP_DRILL_KILLS:-4}"
kill_ms_max="${MPP_DRILL_KILL_MS_MAX:-900}"
resources="${MPP_DRILL_RESOURCES:-6}"
mock_delay_ms="${MPP_DRILL_MOCK_DELAY_MS:-50}"
balance="${MPP_DRILL_BALANCE:-6000}"
plant="${MPP_DRILL_PLANT:-none}"
oracle="${MPP_DRILL_ORACLE:-}"
model=claude-haiku-4-5
spend_cap=$(sed -n 's/^const spend_cap_units: u64 = \([0-9]*\)$/\1/p' examples/mpp_agent.hl)
out=output/mpp-drill
[ "$plant" = none ] || out="$out-$plant"
target=agent/target
agent=$target/release/hl-agent
mock=$target/release/mock-messages
server=$target/release/mpp-server

payer_mutation=none
server_mutation=none
case "$plant" in
  none) ;;
  pay_twice | settle_twice | commit_on_refusal) payer_mutation=$plant ;;
  receipt_on_refusal) server_mutation=$plant ;;
  *)
    echo "mpp-drill: unknown MPP_DRILL_PLANT $plant (none, pay_twice, settle_twice, commit_on_refusal, receipt_on_refusal)" >&2
    exit 2
    ;;
esac

pids=()
cleanup() {
  for pid in "${pids[@]}"; do
    kill "$pid" 2> /dev/null || true
  done
}
trap cleanup EXIT

cargo build --release --locked -j2 --quiet
CARGO_TARGET_DIR="$(pwd)/$target" HL_BIN="$(pwd)/target/release/hl" cargo build --release --locked -j2 --quiet --manifest-path agent/Cargo.toml

wait_port() {
  for _ in $(seq 1 200); do
    [ -s "$1" ] && return 0
    sleep 0.05
  done
  echo "mpp-drill: no port in $1" >&2
  exit 1
}

kill_run() {
  pkill -KILL -P "$1" 2> /dev/null || true
  kill -KILL "$1" 2> /dev/null || true
  set +e
  wait "$1" 2> /dev/null
  set -e
}

rm -rf "$out"
mkdir -p "$out"
passed=0
failed=0
total_kills=0
first=${seeds%..*}
last=${seeds#*..}
for seed in $(seq "$first" "$last"); do
  run=$out/$seed
  mkdir -p "$run/ws"
  echo "seed $seed notes" > "$run/ws/notes.txt"
  "$server" serve "$run/server.port" "$run/server.jsonl" "$seed" "$server_mutation" 2> "$run/server.err" &
  server_pid=$!
  pids+=("$server_pid")
  wait_port "$run/server.port"
  base="http://127.0.0.1:$(cat "$run/server.port")"
  {
    printf '['
    for index in $(seq 1 2 "$resources"); do
      printf '{"text":"Fetching.","calls":[{"name":"fetch","input":{"url":"%s/r%d"}},{"name":"fetch","input":{"url":"%s/r%d"}}]},' "$base" "$index" "$base" "$(( index + 1 ))"
    done
    printf '{"calls":[{"name":"read","input":{"path":"notes.txt"}}]},'
    printf '{"answer":"Fetched %d resources."}]\n' "$resources"
  } > "$run/script.json"
  "$mock" "$run/mock.port" "$run/script.json" "$mock_delay_ms" 2> "$run/mock.err" &
  mock_pid=$!
  pids+=("$mock_pid")
  wait_port "$run/mock.port"
  printf 'Fetch %s/r1 through %s/r%d, then read notes.txt.\n' "$base" "$base" "$resources" > "$run/instruction.md"
  run_agent() {
    ANTHROPIC_BASE_URL="http://127.0.0.1:$(cat "$run/mock.port")" ANTHROPIC_API_KEY=mock \
      HL_AGENT_WALLET_BALANCE="$balance" HL_AGENT_PAYER_MUTATION="$payer_mutation" \
      "$agent" pay --instruction "$run/instruction.md" --cwd "$run/ws" --model "$model" --journal "$run/journal" >> "$run/agent.out" 2>> "$run/agent.err"
  }
  RANDOM=$seed
  kills=0
  status=1
  for _ in $(seq 1 "$kills_max"); do
    run_agent &
    pid=$!
    delay_ms=$(( RANDOM % kill_ms_max ))
    sleep "$(printf '%d.%03d' $((delay_ms / 1000)) $((delay_ms % 1000)))"
    if kill -0 "$pid" 2> /dev/null; then
      kill_run "$pid" 2> /dev/null
      kills=$((kills + 1))
      echo "kill $kills after $delay_ms ms during: $(tail -n 1 "$run/agent.err" 2> /dev/null)" >> "$run/kills.log"
      continue
    fi
    set +e
    wait "$pid"
    status=$?
    set -e
    break
  done
  if [ "$status" -ne 0 ]; then
    set +e
    run_agent
    status=$?
    set -e
  fi
  kill "$mock_pid" "$server_pid" 2> /dev/null || true
  total_kills=$((total_kills + kills))
  verdict=ok
  if [ "$status" -ne 0 ]; then
    verdict="hl-agent exited $status: $(tail -n 1 "$run/agent.err")"
  elif ! "$server" judge "$run/server.jsonl" "$run/journal/state/payments.jsonl" "$spend_cap" > "$run/judge.log"; then
    verdict="$(cat "$run/judge.log")"
  elif [ -n "$oracle" ] && ! "$oracle" --mpp "$run/journal/journal.jsonl" "$run/journal/state/payments.jsonl" > "$run/oracle.log"; then
    verdict="oracle: $(tail -n 1 "$run/oracle.log")"
  fi
  if [ "$verdict" = ok ]; then
    passed=$((passed + 1))
    shown=$(cut -d' ' -f2- "$run/judge.log")
    [ -z "$oracle" ] || shown="$shown oracle: $(tail -n 1 "$run/oracle.log")"
    echo "ok seed=$seed kills=$kills $shown"
  else
    failed=$((failed + 1))
    echo "FAIL seed=$seed kills=$kills: $verdict"
  fi
done
echo "RESULT seeds=$seeds plant=$plant passed=$passed failed=$failed kills=$total_kills"
if [ "$plant" = none ]; then
  [ "$failed" -eq 0 ]
elif [ "$failed" -gt 0 ]; then
  echo "PLANT $plant caught by $failed seeds"
else
  echo "PLANT $plant NOT caught"
  exit 1
fi
