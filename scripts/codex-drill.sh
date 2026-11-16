#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

kills_max="${CODEX_DRILL_KILLS:-12}"
kill_ms_max="${CODEX_DRILL_KILL_MS_MAX:-1500}"
steps="${CODEX_DRILL_STEPS:-6}"
mock_delay_ms="${CODEX_DRILL_MOCK_DELAY_MS:-300}"
tool_sleep="${CODEX_DRILL_TOOL_SLEEP:-0.3}"
model="${CODEX_DRILL_MODEL:-gpt-6-luna}"
plant="${CODEX_DRILL_PLANT:-none}"
seed="${CODEX_DRILL_SEED:-1}"
cancel_sleep="${CODEX_DRILL_CANCEL_SLEEP:-30}"
cancel_polls="${CODEX_DRILL_CANCEL_POLLS:-400}"
cancel_stream_ms="${CODEX_DRILL_CANCEL_STREAM_MS:-20000}"
out=output/codex-drill
[ "$plant" = none ] || out="$out-$plant"
target=agent/target
[ "$plant" = none ] || target=output/agent-target-$plant
agent=$target/release/hl-agent
mock=$target/release/mock-responses
killed_status=137

finished=0
mock_pid=
report() {
  local status=$?
  [ -z "$mock_pid" ] || kill "$mock_pid" 2> /dev/null || true
  if [ "$finished" -ne 1 ]; then
    echo "codex-drill: FAILED (exit $status)"
    [ "$status" -ne 0 ] || status=1
  fi
  exit "$status"
}
trap report EXIT

case "$plant" in
  none) plant_text= ;;
  batch_flush_after_dispatch) plant_text='if external { ==> if false && external {' ;;
  *)
    echo "codex-drill: unknown CODEX_DRILL_PLANT $plant (none, batch_flush_after_dispatch)" >&2
    exit 2
    ;;
esac

cargo build --release --locked -j2 --quiet
HL_AGENT_PLANT="$plant_text" CARGO_TARGET_DIR="$(pwd)/$target" HL_BIN="$(pwd)/target/release/hl" cargo build --release --locked -j2 --quiet --manifest-path agent/Cargo.toml

start_mock() {
  local directory=$1
  rm -f "$directory/port"
  "$mock" "$directory/port" "$directory/script.json" "$mock_delay_ms" 2> "$directory/mock.log" &
  mock_pid=$!
  for _ in $(seq 1 100); do
    [ -s "$directory/port" ] && break
    sleep 0.05
  done
  export OPENAI_BASE_URL="http://127.0.0.1:$(cat "$directory/port")"
  export OPENAI_API_KEY=mock
}

stop_mock() {
  kill "$mock_pid" 2> /dev/null || true
  wait "$mock_pid" 2> /dev/null || true
  mock_pid=
}

rm -rf "$out"
mkdir -p "$out/ws"
workspace=$(cd "$out/ws" && pwd)
faults=(throttle cut failed overloaded)
{
  printf '['
  for step in $(seq 1 "$steps"); do
    fault=${faults[$(( (step - 1) % ${#faults[@]} ))]}
    printf '{"text":"Step %d.","faults":["%s"],"calls":[{"name":"exec_command","input":{"cmd":"echo step%d >> effects.log && sleep %s"}},{"name":"update_plan","input":{"plan":[{"step":"step %d","status":"in_progress"}]}}]},' "$step" "$fault" "$step" "$tool_sleep" "$step"
  done
  printf '{"calls":[{"name":"exec_command","input":{"cmd":"sleep 1; echo polled >> effects.log","yield_time_ms":250}}]},'
  printf '{"calls":[{"name":"write_stdin","input":{"session_id":1,"yield_time_ms":5000}}]},'
  printf '{"calls":[{"name":"apply_patch","patch":"*** Begin Patch\\n*** Add File: done.txt\\n+done\\n*** End Patch\\n"}]},'
  printf '{"answer":"All steps ran."},{"answer":"Checked: effects.log and done.txt are present."},{"answer":"Checked again."}]\n'
} > "$out/script.json"
printf 'Run each step with exec_command, then add done.txt with apply_patch.\n' > "$out/instruction.md"
start_mock "$out"

run_agent() {
  "$agent" run --instruction "$out/instruction.md" --cwd "$workspace" --model "$model" --journal "$out/journal" >> "$out/agent.out" 2>> "$out/agent.err"
}

RANDOM=$seed
kills=0
kills_model=0
kills_tool=0
status=
for _ in $(seq 1 "$kills_max"); do
  run_agent &
  pid=$!
  delay_ms=$(( RANDOM % kill_ms_max ))
  sleep "$(printf '%d.%03d' $((delay_ms / 1000)) $((delay_ms % 1000)))"
  if kill -0 "$pid" 2> /dev/null; then
    last=$(tail -n 1 "$out/agent.err" 2> /dev/null || true)
    pkill -KILL -P "$pid" 2> /dev/null || true
    kill -KILL "$pid" 2> /dev/null || true
    set +e
    wait "$pid" 2> /dev/null
    status=$?
    set -e
    if [ "$status" -eq "$killed_status" ]; then
      kills=$((kills + 1))
      case "$last" in
        *" model "*) kills_model=$((kills_model + 1)) ;;
        *) kills_tool=$((kills_tool + 1)) ;;
      esac
      echo "codex-drill: kill $kills after ${delay_ms} ms, during: ${last:-start}" >> "$out/kills.log"
      continue
    fi
  else
    set +e
    wait "$pid" 2> /dev/null
    status=$?
    set -e
  fi
  break
done
if [ "$status" != 0 ]; then
  set +e
  run_agent
  status=$?
  set -e
fi
if [ "$status" -ne 0 ]; then
  echo "codex-drill: hl-agent exited $status after $kills kills" >&2
  tail -n 20 "$out/agent.err" >&2
  exit 1
fi
set +e
run_agent
rerun=$?
set -e
if [ "$rerun" -ne 0 ]; then
  echo "codex-drill: rerunning a finished journal exited $rerun" >&2
  exit 1
fi
stop_mock

python3 - "$out" "$steps" <<'EOF'
import collections, json, os, sys
out, steps = sys.argv[1], int(sys.argv[2])
external = {"exec_command", "write_stdin", "apply_patch"}
dispatches = collections.Counter()
recovered = collections.Counter()
discarded = []
for line in open(f"{out}/journal/usage.jsonl"):
    record = json.loads(line)
    if record.get("kind") == "tool" and record["tool"] in external:
        dispatches[record["call"]] += 1
    if record.get("kind") == "recover":
        recovered[record["answer"]] += 1
    if record.get("kind") == "stream_discarded":
        discarded.append(record["call"])
repeated = {call: count for call, count in dispatches.items() if count > 1}
if repeated:
    sys.exit(f"codex-drill: external calls dispatched more than once: {repeated}")
usage = json.load(open(f"{out}/journal/usage.json"))
if usage["finish"] != "completed":
    sys.exit(f"codex-drill: the harness finished {usage['finish']}")
log = f"{out}/ws/effects.log"
effects = collections.Counter(open(log).read().split()) if os.path.exists(log) else collections.Counter()
twice = {step: count for step, count in effects.items() if count > 1}
if twice:
    sys.exit(f"codex-drill: effects ran more than once: {twice}")
expected = {f"step{index}" for index in range(1, steps + 1)} | {"polled"}
if set(effects) - expected:
    sys.exit(f"codex-drill: unexpected effects {sorted(effects)}")
replies = set()
unknown = 0
for line in open(f"{out}/journal/journal.jsonl"):
    record = json.loads(line)["record"]
    if "recovery" in record:
        unknown += len(record["recovery"].get("unknown", []))
    if "model" in record:
        replies.add(record["model"]["call_id"])
unanswered = sorted(set(discarded) - replies)
if unanswered:
    sys.exit(f"codex-drill: streams cut by a kill were not requested again to a journaled reply: {unanswered}")
leftover = [name for name in os.listdir(f"{out}/journal/state") if name.endswith(".sse")]
if leftover:
    sys.exit(f"codex-drill: partial streams left after the turn finished: {leftover}")
done = f"{out}/ws/done.txt"
if os.path.exists(done) and open(done).read() != "done\n":
    sys.exit("codex-drill: done.txt is wrong")
faults = collections.Counter(line.split("fault ")[1].strip() for line in open(f"{out}/mock.log") if " fault " in line)
rejected = [line for line in open(f"{out}/mock.log") if " 400 " in line]
if rejected:
    sys.exit(f"codex-drill: the fake Responses API rejected requests: {rejected[:3]}")
print(f"codex-drill: recovered={dict(recovered)} external_dispatches={sum(dispatches.values())} distinct={len(dispatches)} effects={sum(effects.values())} unknown_recovered={unknown} streams_cut={len(discarded)} done={os.path.exists(done)} faults={dict(faults)} requests={usage['requests']} tool_calls={usage['tool_calls']} input_tokens={usage['input_tokens']} cache_read_tokens={usage['cache_read_tokens']} cost_usd={usage.get('cost_usd')} wall_ms={usage['wall_ms']}")
EOF

control=$out/control
mkdir -p "$control/ws"
control_ws=$(cd "$control/ws" && pwd)
printf '[{"calls":[{"name":"exec_command","input":{"cmd":"sleep 2; echo first >> effects.log"}}]},{"expect":"also write steered.txt","calls":[{"name":"exec_command","input":{"cmd":"echo steered > steered.txt"}}]},{"answer":"Steered."},{"answer":"Checked: steered.txt is present."},{"answer":"Checked again."},{"calls":[{"name":"exec_command","input":{"cmd":"sleep %s; echo late >> effects.log"}}]},{"delay_ms":%d,"answer":"never"}]\n' "$cancel_sleep" "$cancel_stream_ms" > "$control/script.json"
printf 'Run the commands.\n' > "$control/instruction.md"
start_mock "$control"
run_control() {
  "$agent" run --instruction "$control/instruction.md" --cwd "$control_ws" --model "$model" --journal "$control/journal" --turn "$1" >> "$control/agent.out" 2>> "$control/agent.err"
}
wait_for() {
  for _ in $(seq 1 "$cancel_polls"); do
    grep -q "$1" "$control/agent.err" 2> /dev/null && return 0
    sleep 0.05
  done
  echo "codex-drill: never saw '$1' in the control run" >&2
  tail -n 20 "$control/agent.err" >&2
  exit 1
}
run_control 1 &
pid=$!
wait_for 'exec_command: sleep 2'
"$agent" steer --journal "$control/journal" --text "also write steered.txt" > /dev/null
wait "$pid"
[ "$(cat "$control_ws/steered.txt")" = steered ] || { echo "codex-drill: the steer did not reach the model" >&2; exit 1; }
run_control 4 &
pid=$!
wait_for "exec_command: sleep $cancel_sleep"
began=$(date +%s%N)
"$agent" cancel --journal "$control/journal" > /dev/null
wait "$pid"
tool_cancel_ms=$(( ($(date +%s%N) - began) / 1000000 ))
if pgrep -f "sleep $cancel_sleep; echo late" > /dev/null; then
  echo "codex-drill: the cancelled command's process group outlived the cancel" >&2
  exit 1
fi
models_before=$(grep -c ' model ' "$control/agent.err")
run_control 5 &
pid=$!
for _ in $(seq 1 "$cancel_polls"); do
  [ "$(grep -c ' model ' "$control/agent.err")" -gt "$models_before" ] && break
  sleep 0.05
done
sleep 0.5
began=$(date +%s%N)
"$agent" cancel --journal "$control/journal" > /dev/null
wait "$pid"
model_cancel_ms=$(( ($(date +%s%N) - began) / 1000000 ))
stop_mock
python3 - "$control" "$tool_cancel_ms" "$model_cancel_ms" <<'EOF'
import json, sys
control, tool_ms, model_ms = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
statuses = {}
events = []
for line in open(f"{control}/journal/journal.jsonl"):
    record = json.loads(line)["record"]
    if "status" in record and "finished" in record["status"]["status"]:
        statuses[record["status"]["turn"]] = record["status"]["status"]["finished"]["result"]
    if "event" in record:
        events.append(record["event"])
expected = {"1": {"completed": "Steered."}, "2": {"completed": "Checked: steered.txt is present."}, "3": {"completed": "Checked again."}, "4": "cancelled_turn", "5": "cancelled_turn"}
if statuses != expected:
    sys.exit(f"codex-drill: control turns finished {statuses}, expected {expected}")
if "late" in open(f"{control}/ws/effects.log").read():
    sys.exit("codex-drill: the cancelled command ran to its end")
print(f"codex-drill: control steer and cancel ok: events={len(events)} tool_cancel_ms={tool_ms} model_cancel_ms={model_ms}")
EOF
finished=1
echo "codex-drill: ok model=$model plant=$plant kills=$kills (model $kills_model, tool $kills_tool)"
