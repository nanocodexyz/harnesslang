#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

hl=target/release/hl
baseline=gate.txt
scratch=output/gate
targets="native-jsonl native-sqlite server-postgres cloudflare vercel"
programs=$(echo examples/*.hl examples/inlined/*.hl examples/sound/*.hl)
per_program_hosts="agent"
counts="emitted_source_lines host_lines_beyond_fixed programs_not_on_wasm host_policy_lines"
prose='"([^"\\]|\\.)*[A-Za-z]+ [A-Za-z]+ [A-Za-z]+ [A-Za-z]+([^"\\]|\\.)*"'

fail() {
  echo "gate: $*" >&2
  exit 1
}

[ -x "$hl" ] || cargo build --release --locked
rm -rf "$scratch"
mkdir -p "$scratch"

source_backend() {
  if "$hl" emit --format source --target native-jsonl examples/minimal.hl > /dev/null 2> "$scratch/probe.err"; then
    return 0
  fi
  grep -q 'usage:' "$scratch/probe.err" && return 1
  cat "$scratch/probe.err" >&2
  fail "hl emit --format source failed on examples/minimal.hl for a reason other than a missing source backend"
}

emitted_source_lines() {
  local file target lines total=0
  source_backend || { echo 0; return; }
  for file in $programs; do
    for target in $targets; do
      if ! "$hl" emit --format source --target "$target" "$file" > "$scratch/source.txt" 2> "$scratch/source.err"; then
        grep -q 'has no source form' "$scratch/source.err" && continue
        cat "$scratch/source.err" >&2
        fail "hl emit --format source failed on $file for $target"
      fi
      lines=$(wc -l < "$scratch/source.txt")
      total=$((total + lines))
    done
  done
  echo "$total"
}

host_lines_beyond_fixed() {
  local files
  files=$(git ls-files -- $per_program_hosts)
  [ -n "$files" ] || { echo 0; return; }
  cat $files | wc -l
}

host_policy_lines() {
  local files
  files=$(git ls-files -- 'hosts/native/*.rs')
  [ -n "$files" ] || { echo 0; return; }
  cat $files | grep -cE "$prose" || true
}

fixed_host() {
  case $1 in
    cloudflare) [ -f hosts/cloudflare.mjs ] ;;
    vercel) [ -f hosts/vercel.mjs ] ;;
    *) [ -d hosts/native ] && [ -n "$(git ls-files hosts/native)" ] ;;
  esac
}

on_wasm() {
  local file=$1 target=$2 dir
  fixed_host "$target" || return 1
  dir="$scratch/bundle/$target-$(basename "$(dirname "$file")")-$(basename "$file" .hl)"
  "$hl" bundle --target "$target" --out "$dir" "$file" > "$scratch/bundle.log" 2>&1 || return 1
}

programs_not_on_wasm() {
  local file target pairs=0
  for file in $programs; do
    for target in $targets; do
      if ! on_wasm "$file" "$target"; then
        pairs=$((pairs + 1))
        echo "$file $target" >> "$scratch/not_on_wasm.txt"
      fi
    done
  done
  echo "$pairs"
}

host_sizes() {
  local platform files
  for platform in cloudflare vercel native; do
    case $platform in
      cloudflare) files=$(git ls-files hosts/cloudflare.mjs) ;;
      vercel) files=$(git ls-files 'hosts/vercel*.mjs') ;;
      native) files=$(git ls-files hosts/native) ;;
    esac
    if [ -z "$files" ]; then
      echo "gate: fixed host $platform: none"
    else
      echo "gate: fixed host $platform: $(cat $files | wc -l) lines in $(echo $files | wc -w) file(s): $(echo $files)"
    fi
  done
}

prove_all() {
  local file
  for file in $programs; do
    "$hl" prove "$file" > "$scratch/prove.log" 2>&1 || { cat "$scratch/prove.log" >&2; fail "hl prove failed on $file"; }
    tail -n 1 "$scratch/prove.log"
  done
  echo "gate: hl prove passes on all $(echo $programs | wc -w) programs on every target"
}

measure() {
  local name
  for name in $counts; do
    echo "$name $("$name")"
  done
}

case "${1:-}" in
  --prove) prove_all; exit 0 ;;
  --write) write=1 ;;
  "") write=0 ;;
  *) fail "usage: scripts/gate.sh [--prove|--write]" ;;
esac

[ -f "$baseline" ] || fail "$baseline is missing"
measure > "$scratch/measured.txt"
host_sizes
rose=0
fell=0
for name in $counts; do
  allowed=$(awk -v name="$name" '$1 == name { print $2 }' "$baseline")
  [ -n "$allowed" ] || fail "$baseline has no $name"
  measured=$(awk -v name="$name" '$1 == name { print $2 }' "$scratch/measured.txt")
  if [ "$measured" -gt "$allowed" ]; then
    echo "gate: $name rose from $allowed to $measured: it may only go down" >&2
    rose=1
  elif [ "$measured" -lt "$allowed" ]; then
    echo "gate: $name fell from $allowed to $measured: lower $baseline to it (scripts/gate.sh --write)" >&2
    fell=1
  else
    echo "gate: $name $measured (gate.txt $allowed)"
  fi
done
[ -s "$scratch/not_on_wasm.txt" ] && sed 's#^#gate: not on wasm: #' "$scratch/not_on_wasm.txt"
[ "$rose" -eq 0 ] || fail "a count rose"
if [ "$fell" -eq 1 ] && [ "$write" -eq 1 ]; then
  cp "$scratch/measured.txt" "$baseline"
  echo "gate: wrote $baseline"
  exit 0
fi
[ "$fell" -eq 0 ] || fail "a count fell: commit the lower $baseline with the change"
echo "gate: ok"
