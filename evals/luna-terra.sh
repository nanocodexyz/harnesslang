#!/bin/sh
set -u
here=$(cd "$(dirname "$0")" && pwd)
: "${OUT:?set OUT to the run directory}"
: "${AGENT_ROOT:?set AGENT_ROOT to the agent root agent-root.sh made}"
: "${TASKS:?set TASKS to the task tree that holds every row of SUITE}"
: "${METER_HOST:?set METER_HOST to the address the task containers reach the meter on}"
suite=${SUITE:-$here/suite-60.txt}
model=${MODEL:-gpt-6-luna}
attempts=${ATTEMPTS:-3}
parallel=${PARALLEL:-2}
max_load=${MAX_LOAD:-26}
max_running=${MAX_RUNNING:-4}
min_free_gb=${MIN_FREE_GB:-12}
meter_port=${METER_PORT:-19830}
relay_port=${RELAY_PORT:-19831}
login=${NANOEVAL_CHATGPT_LOGIN:-$HOME/.codex/auth.json}
gate_poll_sec=60
halt=$OUT/halt
mkdir -p "$OUT/rows"
log() { echo "$(date -u +%FT%T) $*" >> "$OUT/driver.log"; }
extra=""
for argument in "$@"; do extra="$extra --hl-agent-arg=$argument"; done
log "driver start $suite $model $AGENT_ROOT $*"
grep -v '^#' "$suite" | grep . > "$OUT/suite.txt"
while read -r source task; do
  [ -f "$halt" ] && { log halt; break; }
  [ -f "$OUT/rows/chatgpt-halted.json" ] && { log chatgpt-halted; break; }
  [ -f "$OUT/rows/$source.$task.hl-agent.$attempts/result.json" ] && continue
  while :; do
    [ -f "$halt" ] && break 2
    load=$(cut -d' ' -f1 /proc/loadavg | cut -d. -f1)
    free=$(df -BG --output=avail / | tail -1 | tr -dc 0-9)
    running=$(docker ps --format '{{.Names}}' | grep -c '^nanoeval-')
    if [ "$load" -le "$max_load" ] && [ "$free" -ge "$min_free_gb" ] && [ "$running" -lt "$max_running" ]; then break; fi
    log "waiting load $load free $free G running $running"
    sleep "$gate_poll_sec"
  done
  docker images --format '{{.Repository}}:{{.Tag}}' | sort > "$OUT/images-before.txt"
  echo "$source $task" > "$OUT/cur.txt"
  log "start $source $task load $load free $free G running $running"
  NANOEVAL_CHATGPT_LOGIN=$login nice -n 10 python3 "$here/run.py" --suite "$OUT/cur.txt" --tasks "$TASKS" --agent-root "$AGENT_ROOT" --agents hl-agent --attempts "$attempts" --model "$model" --parallel "$parallel" --limits none --meter-port "$meter_port" --relay-port "$relay_port" --openai-route chatgpt --meter-host "$METER_HOST" --out "$OUT" $extra >> "$OUT/run.log" 2>&1
  log "done $source $task exit $?"
  docker images --format '{{.Repository}}:{{.Tag}}' | sort | comm -13 "$OUT/images-before.txt" - | grep -E "^(alexgshaw/$task|nanoeval-eval/nanoeval-$task):" | xargs -r docker rmi -f >> "$OUT/driver.log" 2>&1
  refused=$(cat "$OUT"/rows/*.hl-agent.*/result.json 2>/dev/null | python3 -c "import sys,re;print(sum(1 for m in re.findall(r'\"statuses\": \{[^}]*\}',sys.stdin.read()) if re.search(r'\"(401|403|429)\"',m)))")
  [ "$refused" -gt 0 ] && { log "auth or rate status seen, stopping"; touch "$halt"; break; }
done < "$OUT/suite.txt"
log "driver end"
echo end > "$OUT/go.exit"
