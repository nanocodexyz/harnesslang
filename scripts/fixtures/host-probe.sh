#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

out=output/host-probe
host=hosts/native/target/release/hl-host

cargo build --release --locked --quiet
cargo build --release --locked --quiet --manifest-path hosts/native/Cargo.toml
rm -rf "$out"
mkdir -p "$out/modules" "$out/ws"
cat scripts/fixtures/programs/syscalls.hl scripts/fixtures/shell.hl > "$out/syscalls.hl"
cat scripts/fixtures/programs/syscalls_child.hl scripts/fixtures/shell.hl > "$out/syscalls_child.hl"
target/release/hl emit --target native-jsonl --format wasm "$out/syscalls.hl" > "$out/modules/syscalls.wasm"
target/release/hl emit --target native-jsonl --format wasm "$out/syscalls_child.hl" > "$out/modules/syscalls_child.wasm"
mkdir -p "$out/sqlite-modules"
target/release/hl emit --target native-sqlite --format wasm "$out/syscalls.hl" > "$out/sqlite-modules/syscalls.wasm"
workspace=$(cd "$out/ws" && pwd)

run() {
  local journal=$1 handler=$2 args=$3
  "$host" run --module "$out/modules/syscalls.wasm" --journal "$out/$journal" --cwd "$workspace" --handler "$handler" --args "$args" --spawn worker=syscalls_child:work --texts scripts/fixtures/prompts/codex-host.json 2>> "$out/host.err"
}

files=$(run files files "{\"dir\":\"$workspace\"}")
[ "$files" = '{"done":"beta\ngamma|no syscall mystery"}' ] || { echo "host-probe: files answered $files" >&2; exit 1; }
again=$(run files files "{\"dir\":\"$workspace\"}")
[ "$again" = "$files" ] || { echo "host-probe: a second run answered $again" >&2; exit 1; }
stored=$("$host" run --module "$out/sqlite-modules/syscalls.wasm" --journal "$out/sqlite" --store "sqlite:$out/sqlite.db" --cwd "$workspace" --handler files --args "{\"dir\":\"$workspace\"}" --texts scripts/fixtures/prompts/codex-host.json 2>> "$out/host.err")
[ "$stored" = "$files" ] || { echo "host-probe: the SQLite journal answered $stored" >&2; exit 1; }
stored_again=$("$host" run --module "$out/sqlite-modules/syscalls.wasm" --journal "$out/sqlite" --store "sqlite:$out/sqlite.db" --cwd "$workspace" --handler files --args "{\"dir\":\"$workspace\"}" --texts scripts/fixtures/prompts/codex-host.json 2>> "$out/host.err")
[ "$stored_again" = "$files" ] || { echo "host-probe: a second SQLite run answered $stored_again" >&2; exit 1; }
[ ! -e "$out/sqlite/journal.jsonl" ] || { echo "host-probe: the SQLite run wrote a jsonl journal" >&2; exit 1; }
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
