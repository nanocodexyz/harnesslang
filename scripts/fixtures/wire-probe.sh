#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

out=output/wire-probe
host=hosts/native/target/release/hl-host
fixtures=scripts/fixtures/target/release
mock_pid=

stop_mock() {
  [ -z "$mock_pid" ] || kill "$mock_pid" 2> /dev/null || true
  mock_pid=
}
trap stop_mock EXIT

cargo build --release --quiet --manifest-path hosts/native/Cargo.toml
cargo build --release --quiet --manifest-path scripts/fixtures/Cargo.toml
rm -rf "$out"
mkdir -p "$out"

start_mock() {
  local name=$1 script=$2
  rm -f "$out/port"
  "$fixtures/$name" "$out/port" "$script" 0 2> "$out/$name.log" &
  mock_pid=$!
  for _ in $(seq 1 100); do
    [ -s "$out/port" ] && return 0
    sleep 0.05
  done
  echo "wire-probe: $name did not publish its port" >&2
  exit 1
}

request() {
  local file=$1 url=$2 body=$3 auth_name=$4 auth_value=$5
  python3 - "$file" "$url" "$body" "$auth_name" "$auth_value" <<'EOF'
import json, sys
file, url, body, auth_name, auth_value = sys.argv[1:]
json.dump({"method": "POST", "url": url, "headers": [["content-type", "application/json"], [auth_name, auth_value]], "body": body}, open(file, "w"))
EOF
}

printf '[{"text":"Hello from the mock.","calls":[{"name":"Bash","input":{"command":"true"}}]},{"answer":"done"}]\n' > "$out/messages.json"
start_mock mock-messages "$out/messages.json"
export WIRE_PROBE_BASE="http://127.0.0.1:$(cat "$out/port")"
export WIRE_PROBE_KEY=probe
request "$out/messages-stream.json" '${WIRE_PROBE_BASE}/v1/messages' '{"model":"claude-haiku-4-5","max_tokens":64,"stream":true,"system":"probe","tools":[],"messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}' x-api-key '${WIRE_PROBE_KEY}'
"$host" wire --request "$out/messages-stream.json" --scratch "$out/scratch" > "$out/messages-stream.out"
request "$out/messages-default.json" "\${WIRE_PROBE_UNSET:-$WIRE_PROBE_BASE}/v1/messages" '{}' x-api-key '${WIRE_PROBE_KEY}'
"$host" wire --request "$out/messages-default.json" --scratch "$out/scratch" > "$out/messages-default.out" || true
request "$out/messages-missing.json" '${WIRE_PROBE_UNSET}/v1/messages' '{}' x-api-key k
"$host" wire --request "$out/messages-missing.json" --scratch "$out/scratch" > "$out/messages-missing.out"
stop_mock

printf '[{"text":"Hello from the mock.","calls":[{"name":"exec_command","input":{"cmd":"true"}}]},{"answer":"done"}]\n' > "$out/responses.json"
start_mock mock-responses "$out/responses.json"
export WIRE_PROBE_BASE="http://127.0.0.1:$(cat "$out/port")"
request "$out/responses-stream.json" '${WIRE_PROBE_BASE}/v1/responses' '{"model":"gpt-5","stream":true,"instructions":"probe","tools":[],"prompt_cache_key":"probe","store":false,"include":["reasoning.encrypted_content"],"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}]}' authorization 'Bearer ${WIRE_PROBE_KEY}'
"$host" wire --request "$out/responses-stream.json" --scratch "$out/scratch" > "$out/responses-stream.out"
stop_mock
"$host" wire --request "$out/responses-stream.json" --scratch "$out/scratch" > "$out/refused.out"

python3 - "$out" <<'EOF'
import json, sys
out = sys.argv[1]
def answer(name):
    return json.load(open(f"{out}/{name}.out"))
streamed = answer("messages-stream")
if streamed.get("status") != 200 or not streamed.get("events"):
    sys.exit(f"wire-probe: the Messages stream answered {streamed}")
names = [name for name, _ in streamed["events"]]
if names[0] != "message_start" or names[-1] != "message_stop":
    sys.exit(f"wire-probe: the Messages stream events are {names}")
for _, data in streamed["events"]:
    json.loads(data)
if not any(name == "content-type" and "text/event-stream" in value for name, value in streamed["headers"]):
    sys.exit(f"wire-probe: the Messages headers are {streamed['headers']}")
defaulted = answer("messages-default")
if "status" not in defaulted:
    sys.exit(f"wire-probe: a defaulted url did not reach the mock: {defaulted}")
missing = answer("messages-missing")
if "WIRE_PROBE_UNSET" not in missing.get("transport", ""):
    sys.exit(f"wire-probe: an unset name answered {missing}")
responses = answer("responses-stream")
kinds = [name for name, _ in responses.get("events", [])]
if responses.get("status") != 200 or "response.completed" not in kinds:
    sys.exit(f"wire-probe: the Responses stream answered {responses}")
refused = answer("refused")
if "transport" not in refused:
    sys.exit(f"wire-probe: a closed port answered {refused}")
print(f"wire-probe: ok messages_events={len(names)} responses_events={len(kinds)} defaulted_status={defaulted['status']}")
EOF
