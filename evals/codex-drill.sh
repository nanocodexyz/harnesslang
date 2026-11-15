#!/bin/sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${EVAL_OUT:-$here/../output/eval}
root=${AGENT_ROOT:-$out/agent-root}
tasks=${TASKS:-$out/tasks}
drill=${DRILL_OUT:-$here/../output/codex-drill}
model=${MODEL:-gpt-5.5}
limits=${LIMITS:-none}
agents=${AGENTS:-codex}
upstream_key=fake-upstream-key
for agent in $(echo "$agents" | tr , ' '); do
  [ -x "$root/bin/$agent" ] || { echo "codex-drill: $root/bin/$agent is missing; run CODEX=1 evals/setup.sh" >&2; exit 1; }
done
gateway=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
free_port() { python3 -c 'import socket,sys; s=socket.socket(); s.bind((sys.argv[1], 0)); print(s.getsockname()[1])' "$1"; }
rm -rf "$drill"
mkdir -p "$drill"
fake_pid=
cleanup() { [ -z "$fake_pid" ] || kill "$fake_pid" 2>/dev/null || true; }
trap cleanup EXIT
failures=0
for agent in $(echo "$agents" | tr , ' '); do
for task in $(ls "$tasks/nanoeval"); do
  dir=$drill/$agent/$task
  mkdir -p "$dir"
  printf 'nanoeval %s\n' "$task" > "$dir/suite.txt"
  fake_port=$(free_port 127.0.0.1)
  python3 "$here/fake_openai.py" 127.0.0.1 "$fake_port" "$tasks/nanoeval/$task/solution/solve.sh" "$dir/fake.jsonl" "$model" &
  fake_pid=$!
  env -u ANTHROPIC_API_KEY OPENAI_API_KEY=$upstream_key NANOEVAL_OPENAI_UPSTREAM=http://127.0.0.1:$fake_port \
    python3 "$here/run.py" --suite "$dir/suite.txt" --tasks "$tasks" --agent-root "$root" --out "$dir/run" --agents "$agent" \
    --attempts 1 --model "$model" --parallel 1 --limits "$limits" --meter-port "$(free_port "$gateway")" --relay-port "$(free_port "$gateway")" > "$dir/run.log" 2>&1
  kill "$fake_pid"
  fake_pid=
  if python3 "$here/codex_drill_check.py" "$dir" "$upstream_key"; then :; else failures=$((failures + 1)); fi
done
done
echo "codex-drill: $failures failing rows"
[ "$failures" = 0 ]
