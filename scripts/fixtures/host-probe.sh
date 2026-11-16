#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

out=output/host-probe
host=hosts/native/target/release/hl-host

cargo build --release --locked --quiet
cargo build --release --locked --quiet --manifest-path hosts/native/Cargo.toml
rm -rf "$out"
mkdir -p "$out/modules" "$out/ws"
for program in syscalls syscalls_child; do
  target/release/hl emit --target native-jsonl --format wasm "scripts/fixtures/programs/$program.hl" > "$out/modules/$program.wasm"
done
workspace=$(cd "$out/ws" && pwd)

run() {
  local journal=$1 handler=$2 args=$3
  "$host" run --module "$out/modules/syscalls.wasm" --journal "$out/$journal" --cwd "$workspace" --handler "$handler" --args "$args" --spawn worker=syscalls_child:work 2>> "$out/host.err"
}

files=$(run files files "{\"dir\":\"$workspace\"}")
[ "$files" = '{"done":"beta\ngamma|no syscall mystery"}' ] || { echo "host-probe: files answered $files" >&2; exit 1; }
again=$(run files files "{\"dir\":\"$workspace\"}")
[ "$again" = "$files" ] || { echo "host-probe: a second run answered $again" >&2; exit 1; }
delegated=$(run delegate delegate '{"task":"hi"}')
[ "$delegated" = '{"done":"{\"finished\":\"child:hi\"}"}' ] || { echo "host-probe: delegate answered $delegated" >&2; exit 1; }
"$host" steer --journal "$out/delegate" --text "probe" > /dev/null
"$host" cancel --journal "$out/delegate" > /dev/null
[ "$(ls "$out/delegate/control" | wc -l)" = 2 ] || { echo "host-probe: the control directory holds $(ls "$out/delegate/control")" >&2; exit 1; }
python3 - "$out" <<'EOF'
import json, sys
out = sys.argv[1]
files = json.load(open(f"{out}/files/usage.json"))
if files["tool_calls"] != 5 or files["finish"] != "done":
    sys.exit(f"host-probe: files usage is {files}")
delegated = json.load(open(f"{out}/delegate/usage.json"))
if delegated["children"] != 1 or delegated["tool_calls"] != 2:
    sys.exit(f"host-probe: delegate usage is {delegated}")
print(f"host-probe: ok files_tool_calls={files['tool_calls']} children={delegated['children']}")
EOF
