#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

mode="${AGENT_DRILL_MODE:-mock}"
kills_max="${AGENT_DRILL_KILLS:-12}"
kill_ms_max="${AGENT_DRILL_KILL_MS_MAX:-1500}"
steps="${AGENT_DRILL_STEPS:-6}"
spawn_after="${AGENT_DRILL_SPAWN_AFTER:-2}"
job_after="${AGENT_DRILL_JOB_AFTER:-1}"
job_sleep="${AGENT_DRILL_JOB_SLEEP:-3}"
job_timeout_ms="${AGENT_DRILL_JOB_TIMEOUT_MS:-100}"
job_wait_polls="${AGENT_DRILL_JOB_WAIT_POLLS:-400}"
child_steps="${AGENT_DRILL_CHILD_STEPS:-2}"
mock_delay_ms="${AGENT_DRILL_MOCK_DELAY_MS:-300}"
tool_sleep="${AGENT_DRILL_TOOL_SLEEP:-0.3}"
model="${AGENT_DRILL_MODEL:-claude-haiku-4-5}"
plant="${AGENT_DRILL_PLANT:-none}"
seed="${AGENT_DRILL_SEED:-1}"
out=output/agent-drill-$mode
[ "$plant" = none ] || out="$out-$plant"
host="${DRILL_HOST:-hl-agent}"
case "$host" in
  hl-agent)
    target=agent/target
    [ "$plant" = none ] || target=output/agent-target-$plant
    manifest=agent/Cargo.toml
    mock=$target/release/mock-messages
    ;;
  hl-host)
    target=hosts/native/target
    [ "$plant" = none ] || target=output/host-target-$plant
    manifest=hosts/native/Cargo.toml
    mock=scripts/fixtures/target/release/mock-messages
    ;;
  *)
    echo "agent-drill: unknown DRILL_HOST $host (hl-agent, hl-host)" >&2
    exit 2
    ;;
esac
agent=$target/release/$host
killed_status=137

finished=0
mock_pid=
report() {
  local status=$?
  [ -z "$mock_pid" ] || kill "$mock_pid" 2> /dev/null || true
  if [ "$finished" -ne 1 ]; then
    echo "agent-drill: FAILED (exit $status)"
    [ "$status" -ne 0 ] || status=1
  fi
  exit "$status"
}
trap report EXIT

case "$plant" in
  none) wasm_plant= ;;
  batch_flush_after_dispatch) wasm_plant=batch_flush_skipped ;;
  *)
    echo "agent-drill: unknown AGENT_DRILL_PLANT $plant (none, batch_flush_after_dispatch)" >&2
    exit 2
    ;;
esac

cargo build --release --locked -j2 --quiet
HL_AGENT_PLANT="$wasm_plant" CARGO_TARGET_DIR="$(pwd)/$target" cargo build --release --locked -j2 --quiet --manifest-path "$manifest"
[ "$host" = hl-agent ] || cargo build --release --locked -j2 --quiet --manifest-path scripts/fixtures/Cargo.toml

rm -rf "$out"
mkdir -p "$out/ws"
workspace=$(cd "$out/ws" && pwd)
case "$mode" in
  mock)
    {
      printf '['
      for step in $(seq 1 "$steps"); do
        printf '{"text":"Step %d.","throttle":%d,"calls":[{"name":"Bash","input":{"command":"echo step%d >> effects.log && sleep %s","description":"Record step %d"}},{"name":"Read","input":{"file_path":"effects.log"}}]},' "$step" "$(( step == 1 ))" "$step" "$tool_sleep" "$step"
        if [ "$step" -eq "$job_after" ]; then
          printf '{"text":"Starting the slow step.","calls":[{"name":"Bash","input":{"command":"sleep %s && echo moved >> effects.log","timeout":%d,"description":"Slow step"}}]},' "$job_sleep" "$job_timeout_ms"
        fi
        if [ "$step" -eq "$spawn_after" ]; then
          printf '{"text":"Delegating the child steps.","calls":[{"name":"spawn_agent","input":{"role":"recorder","task":"Record the child steps in effects.log, then submit what you recorded."}}]},'
        fi
        if [ "$step" -eq $(( spawn_after + 1 )) ]; then
          printf '{"calls":[{"name":"wait_agent","input":{"agent_id":"$spawned"}}]},'
        fi
      done
      printf '{"calls":[{"name":"Read","input":{"file_path":"pixel.png"}}]},'
      printf '{"text":"Writing done.txt n","stop":"max_tokens"},'
      printf '{"calls":[{"name":"Write","input":{"file_path":"done.txt","content":"done\\n"}}]},'
      printf '{"answer":"All steps ran."},'
      printf '{"answer":"The slow step reported."}]\n'
    } > "$out/script.json"
    {
      printf '['
      for step in $(seq 1 "$child_steps"); do
        printf '{"text":"Child step %d.","calls":[{"name":"Bash","input":{"command":"echo child%d >> effects.log && sleep %s","description":"Record child step %d"}}]},' "$step" "$step" "$tool_sleep" "$step"
      done
      printf '{"calls":[{"name":"Read","input":{"file_path":"effects.log"}}]},'
      printf '{"calls":[{"name":"submit_result","input":{"output":"the child recorded %d steps"}}]}]\n' "$child_steps"
    } > "$out/child.json"
    printf '\211PNG\r\n\032\nhl-agent drill image' > "$workspace/pixel.png"
    "$mock" "$out/port" "$out/script.json" "$mock_delay_ms" "$out/child.json" 2> "$out/mock.log" &
    mock_pid=$!
    for _ in $(seq 1 100); do
      [ -s "$out/port" ] && break
      sleep 0.05
    done
    export ANTHROPIC_BASE_URL="http://127.0.0.1:$(cat "$out/port")"
    export ANTHROPIC_API_KEY=mock
    ;;
  real)
    : "${ANTHROPIC_API_KEY:?the real drill needs ANTHROPIC_API_KEY}"
    ;;
  *)
    echo "agent-drill: unknown AGENT_DRILL_MODE $mode (mock, real)" >&2
    exit 2
    ;;
esac
{
  printf 'Run each of these %d shell commands with the Bash tool, one Bash call per command, one after another, in this order:\n' "$steps"
  for step in $(seq 1 "$steps"); do
    printf -- '- echo step%d >> effects.log && sleep %s\n' "$step" "$tool_sleep"
  done
  printf 'If a call reports that its outcome is unknown, do not run that command again: go on with the next one. Then write the file done.txt containing the single line done. Do not change anything else.\n'
} > "$out/instruction.md"

run_agent() {
  "$agent" run --instruction "$out/instruction.md" --cwd "$workspace" --model "$model" --journal "$out/journal" >> "$out/agent.out" 2>> "$out/agent.err"
}

kill_run() {
  local pid=$1
  pkill -KILL -P "$pid" 2> /dev/null || true
  kill -KILL "$pid" 2> /dev/null || true
  set +e
  wait "$pid" 2> /dev/null
  set -e
}

kills_job=0
if [ "$mode" = mock ]; then
  run_agent &
  pid=$!
  for _ in $(seq 1 "$job_wait_polls"); do
    grep -q 'became background job' "$out/agent.err" 2> /dev/null && break
    kill -0 "$pid" 2> /dev/null || break
    sleep 0.05
  done
  if kill -0 "$pid" 2> /dev/null && grep -q 'became background job' "$out/agent.err"; then
    kill_run "$pid"
    kills_job=1
    echo "agent-drill: kill during the background job" >> "$out/kills.log"
  else
    kill_run "$pid"
  fi
fi

RANDOM=$seed
kills=0
kills_model=0
kills_tool=0
kills_child=0
status=
for attempt in $(seq 1 "$kills_max"); do
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
        "$host["*) kills_child=$((kills_child + 1)) ;;
      esac
      case "$last" in
        *" model "*) kills_model=$((kills_model + 1)) ;;
        *) kills_tool=$((kills_tool + 1)) ;;
      esac
      echo "agent-drill: kill $kills after ${delay_ms} ms, during: ${last:-start}" >> "$out/kills.log"
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
  echo "agent-drill: $host exited $status after $kills kills" >&2
  tail -n 20 "$out/agent.err" >&2
  exit 1
fi

set +e
run_agent
rerun=$?
set -e
if [ "$rerun" -ne 0 ]; then
  echo "agent-drill: rerunning a finished journal exited $rerun" >&2
  exit 1
fi

python3 - "$out" "$mode" "$steps" "$child_steps" <<'EOF'
import collections, json, os, sys
out, mode, steps, child_steps = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
external = {"Bash", "Edit", "Write", "spawn_agent"}
children_dir = f"{out}/journal/children"
children = sorted(os.listdir(children_dir)) if os.path.isdir(children_dir) else []
journals = [f"{out}/journal"] + [f"{children_dir}/{key}" for key in children]
dispatches = collections.Counter()
for journal in journals:
    if not os.path.exists(f"{journal}/usage.jsonl"):
        continue
    for line in open(f"{journal}/usage.jsonl"):
        record = json.loads(line)
        if record.get("kind") == "tool" and record["tool"] in external:
            dispatches[(journal, record["call"])] += 1
repeated = {call: count for call, count in dispatches.items() if count > 1}
if repeated:
    sys.exit(f"agent-drill: external calls dispatched more than once: {repeated}")
usage = json.load(open(f"{out}/journal/usage.json"))
if usage["finish"] != "completed":
    sys.exit(f"agent-drill: the harness finished {usage['finish']}")
log = f"{out}/ws/effects.log"
effects = collections.Counter(open(log).read().split()) if os.path.exists(log) else collections.Counter()
twice = {step: count for step, count in effects.items() if count > 1}
if mode == "mock" and twice:
    sys.exit(f"agent-drill: effects ran more than once: {twice}")
expected = {f"step{index}" for index in range(1, steps + 1)} | {f"child{index}" for index in range(1, child_steps + 1)} | {"moved"}
if set(effects) - expected:
    sys.exit(f"agent-drill: unexpected effects {sorted(effects)}")
if open(f"{out}/ws/done.txt").read().strip() != "done":
    sys.exit("agent-drill: done.txt is wrong")
unknown = 0
child_unknown = 0
discarded = []
parent_unknown = set()
spawn_calls = set()
for line in open(f"{out}/journal/journal.jsonl"):
    record = json.loads(line)["record"]
    if "recovery" in record:
        parent_unknown.update(record["recovery"].get("unknown", []))
    if record.get("call", {}).get("tool") == "spawn_agent":
        spawn_calls.add(record["call"]["call_id"])
for journal in journals:
    replies = set()
    orphaned = journal != journals[0] and os.path.basename(journal).replace(".", "/") in parent_unknown
    missing = [name for name in ("journal.jsonl", "usage.jsonl") if not os.path.exists(f"{journal}/{name}")]
    if missing and not orphaned:
        sys.exit(f"agent-drill: {journal} lacks {missing} and its spawn was not left unknown by a kill")
    if missing:
        continue
    for line in open(f"{journal}/journal.jsonl"):
        record = json.loads(line)["record"]
        if "recovery" in record:
            unknown += len(record["recovery"].get("unknown", []))
            if journal != journals[0]:
                child_unknown += len(record["recovery"].get("unknown", []))
        if "model" in record:
            replies.add(record["model"]["call_id"])
    cut = [json.loads(line) for line in open(f"{journal}/usage.jsonl")]
    cut = [record["call"] for record in cut if record.get("kind") == "stream_discarded"]
    discarded += cut
    unanswered = sorted(set(cut) - replies)
    if unanswered:
        sys.exit(f"agent-drill: streams cut by a kill in {journal} were not requested again to a journaled reply: {unanswered}")
    leftover = [name for name in os.listdir(f"{journal}/state") if name.endswith(".sse")]
    if leftover and not orphaned:
        sys.exit(f"agent-drill: partial streams left in {journal} after the turn finished: {leftover}")
recovered = collections.Counter()
for line in open(f"{out}/journal/usage.jsonl"):
    record = json.loads(line)
    if record.get("kind") == "recover":
        recovered[record["answer"]] += 1
started_jobs = []
job_notices = []
for line in open(f"{out}/journal/journal.jsonl"):
    record = json.loads(line)["record"]
    output = record.get("result", {}).get("output", {})
    if isinstance(output.get("ok"), dict) and "job" in output["ok"]:
        started_jobs.append(record["result"]["call_id"])
    if "event" in record and "job" in record["event"]["event"]:
        job_notices.append(record["event"]["event"]["job"]["id"])
closed = [notice for notice in job_notices if notice.startswith(("done:", "lost:"))]
if len(closed) != len(set(closed)):
    sys.exit(f"agent-drill: a job was closed twice: {job_notices}")
if mode == "mock" and len(started_jobs) > 1:
    sys.exit(f"agent-drill: expected the slow step to become at most one background job, found {started_jobs}")
if mode == "mock" and len(closed) != len(started_jobs):
    sys.exit(f"agent-drill: the background job was started {len(started_jobs)} time(s) and closed by {closed}")
if mode == "mock" and recovered["reply"] + recovered["not_dispatched"] == 0:
    sys.exit(f"agent-drill: no kill left a call the recovery hook could answer: {dict(recovered)}")
joined = []
for line in open(f"{out}/journal/journal.jsonl"):
    record = json.loads(line)["record"]
    output = record.get("result", {}).get("output", {})
    if isinstance(output.get("ok"), str) and "the child recorded" in output["ok"]:
        joined.append(output["ok"])
spawn_unknown = any(key.replace(".", "/") in parent_unknown for key in children) or bool(spawn_calls & parent_unknown)
if mode == "mock" and not spawn_unknown:
    if len(children) != 1:
        sys.exit(f"agent-drill: expected one child journal under {children_dir}, found {children}")
    child_records = [json.loads(line)["record"] for line in open(f"{children_dir}/{children[0]}/journal.jsonl")]
    if not any("status" in record for record in child_records):
        sys.exit(f"agent-drill: the child's journal holds no status record: {sorted({next(iter(record)) for record in child_records})}")
    if len(joined) != 1 or json.loads(joined[0]) != {"submitted": f"the child recorded {child_steps} steps"}:
        sys.exit(f"agent-drill: the parent's journal holds {joined} for the join, expected the child's submitted result once")
blobs = []
for line in open(f"{out}/journal/journal.jsonl"):
    record = json.loads(line)["record"]
    output = record.get("result", {}).get("output", {})
    if isinstance(output.get("ok"), dict) and "blob" in output["ok"]:
        blobs.append(output["ok"])
if mode == "mock" and (len(blobs) != 1 or "data" not in blobs[0]):
    sys.exit(f"agent-drill: the image read journaled {len(blobs)} blob results, expected one holding its data")
child_dispatches = sum(count for (journal, _), count in dispatches.items() if journal != journals[0])
print(f"agent-drill: spawn_unknown={spawn_unknown} jobs={len(started_jobs)} job_notices={job_notices} recovered={dict(recovered)} children={len(children)} joined={len(joined)} child_dispatches={child_dispatches} child_unknown_recovered={child_unknown} children_reported={usage['children']} blobs_journaled={len(blobs)} external_dispatches={sum(dispatches.values())} distinct={len(dispatches)} effects={sum(effects.values())} unknown_recovered={unknown} streams_cut={len(discarded)} requests={usage['requests']} tool_calls={usage['tool_calls']} input_tokens={usage['input_tokens']} output_tokens={usage['output_tokens']} cache_read_tokens={usage['cache_read_tokens']} wall_ms={usage['wall_ms']}")
EOF
finished=1
echo "agent-drill: ok mode=$mode model=$model plant=$plant kills=$kills (model $kills_model, tool $kills_tool; in the child $kills_child) and $kills_job during the background job"
