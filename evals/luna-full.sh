#!/bin/sh
cd "$HOME/luna-full"
: "${METER_HOST:?set METER_HOST to the address the task containers reach the meter on}"
out=$HOME/luna-full/out
mkdir -p "$out/batches" "$out/rows"
[ -f "$out/images-before.txt" ] || docker images --format '{{.Repository}}:{{.Tag}}' | sort > "$out/images-before.txt"
[ -f "$out/batches/b00" ] || grep -v '^#' suite-60.txt | grep . | split -l 2 -d - "$out/batches/b"
log() { echo "$(date -u +%FT%T) $*" >> "$out/driver.log"; }
mine=$(python3 -c "import hashlib;print('nanoeval-'+hashlib.sha256(b'$out/rows'.replace(b'//',b'/')).hexdigest()[:12])")
log "driver start mine=$mine"
for b in "$out"/batches/b*; do
  [ -f "$out/halt" ] && { log halt; break; }
  [ -f "$out/rows/chatgpt-halted.json" ] && { log chatgpt-halted; break; }
  todo=0
  while read s t; do for a in 1 2 3; do [ -f "$out/rows/$s.$t.hl-agent.$a/result.json" ] || todo=$((todo + 1)); done; done < "$b"
  [ "$todo" -gt 0 ] || continue
  while :; do
    [ -f "$out/halt" ] && break 2
    load=$(cut -d' ' -f1 /proc/loadavg | cut -d. -f1)
    others=$(docker ps --format '{{.Names}}' | grep '^nanoeval-' | grep -vc "^$mine")
    par=$((3 - others))
    [ "$par" -gt "$todo" ] && par=$todo
    if [ "$load" -le 26 ] && [ "$par" -ge 1 ]; then break; fi
    log "waiting load $load others $others"
    sleep 60
  done
  free=$(df -BG --output=avail / | tail -1 | tr -dc 0-9)
  if [ "$free" -lt 12 ]; then log "free $free G, stopping"; touch "$out/diskstop"; break; fi
  log "start $b todo $todo par $par load $load free $free G"
  NANOEVAL_CHATGPT_LOGIN=$HOME/.codex/auth.json nice -n 10 python3 evals/run.py --suite "$b" --tasks tasks --agent-root "$HOME/hl-luna2/root" --agents hl-agent --attempts 3 --model gpt-6-luna --parallel "$par" --limits none --meter-port 19740 --relay-port 14740 --openai-route chatgpt --meter-host "$METER_HOST" --out "$out" >> "$out/run.log" 2>&1
  log "done $b exit $?"
  pats=$(awk '{printf "^(alexgshaw/%s|nanoeval-eval/nanoeval-%s):|", $2, $2}' "$b" | sed 's/|$//')
  docker images --format '{{.Repository}}:{{.Tag}}' | sort | comm -13 "$out/images-before.txt" - | grep -E "$pats" | xargs -r docker rmi -f >> "$out/driver.log" 2>&1
  log "free after cleanup $(df -BG --output=avail / | tail -1 | tr -dc 0-9) G"
done
log "driver end"
