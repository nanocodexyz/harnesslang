#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${JS_DIFF_SEEDS:-0..2000}"
targets="${JS_DIFF_TARGETS:-cloudflare vercel}"
files="${JS_DIFF_FILES:-$(echo examples/*.hl)}"
plant="${JS_DIFF_PLANT:-none}"
scope="${JS_DIFF_SCOPE:-}"
scope_args=()
suffix=
if [ -n "$scope" ]; then
  scope_args=(--scope "$scope")
  suffix=-scoped
fi
hl=target/release/hl
out=output/js-diff/$plant
mkdir -p "$out"

[ -x "$hl" ] || cargo build --release --locked

plant_expression() {
  case "$1" in
    none) echo 's/^$//' ;;
    flush_after_dispatch) echo 's/const required = flushes(site.durability, call.effect);/const required = false;/' ;;
    bound_message) echo 's/new DecodeError(`bound(${type.bound})`)/new DecodeError(`bound ${type.bound}`)/' ;;
    enum_unchecked) echo 's/^function checkedOutput(tool, returns, output) {$/&\n  return output;/' ;;
    default_unfilled) echo 's/if (defaults\[index\] !== null \&\& !Object.hasOwn(input, param.name)) input\[param.name\] = defaults\[index\];/void defaults;/' ;;
    mode_dropped) echo 's/world.model({ turn: this.turn, seq, callsMax, mode: site.mode,/world.model({ turn: this.turn, seq, callsMax, mode: null,/' ;;
    blob_unjournaled) echo 's/this.blobs.journaled(data, true)/this.blobs.journaled(data, false)/' ;;
    text_after_kept) echo 's/return at < 0 ? "" : subject.slice(at + separator.length);/return at < 0 ? "" : subject.slice(at);/' ;;
    grandchild_unscoped) echo 's/return { tag: "ok", value: scope === ROOT_SCOPE ? key : scope + SPAWN_SCOPE_SEPARATOR + key };/return { tag: "ok", value: key };/' ;;
    idle_early) echo 's/if (clock < due) return { ran: "not_due"/if (false \&\& clock < due) return { ran: "not_due"/' ;;
    *) echo "js-diff: unknown plant $1" >&2; return 1 ;;
  esac
}

diff_one() {
  local file=$1 target=$2
  local dir started ended
  dir="$out/$target-$(basename "$file" .hl)$suffix"
  mkdir -p "$dir"
  "$hl" emit --target "$target" "$file" | sed "$(plant_expression "$plant")" > "$dir/harness.mjs"
  if [ "$plant" != none ] && cmp -s <("$hl" emit --target "$target" "$file") "$dir/harness.mjs"; then
    echo "js-diff: plant $plant changes nothing in $file for $target" >&2
    return 1
  fi
  started=$(date +%s%N)
  if ! "$hl" sim --target "$target" --seeds "$seeds" --trace "$dir/trace.jsonl" "${scope_args[@]}" "$file" > "$dir/sim.log" 2>&1; then
    echo "js-diff: the interpreter failed $file on $target:" >&2
    cat "$dir/sim.log" >&2
    return 1
  fi
  if ! node scripts/js-sim.mjs --target "$target" --module "$dir/harness.mjs" --trace "$dir/trace.jsonl" > "$dir/js.log" 2>&1; then
    echo "js-diff: $file on $target failed:" >&2
    cat "$dir/js.log" >&2
    return 1
  fi
  ended=$(date +%s%N)
  rm -f "$dir/trace.jsonl"
  echo "ok backend=js file=$file $(cut -d' ' -f2- "$dir/js.log") wall_ms=$(( (ended - started) / 1000000 ))"
}

pids=()
for target in $targets; do
  (
    for file in $files; do
      diff_one "$file" "$target"
    done
  ) > "$out/$target.$$.log" 2>&1 &
  pids+=($!)
done
failed=0
for pid in "${pids[@]}"; do
  wait "$pid" || failed=1
done
for target in $targets; do
  cat "$out/$target.$$.log"
  rm -f "$out/$target.$$.log"
done
if [ "$failed" -ne 0 ]; then
  echo "js-diff: a differential run failed" >&2
  exit 1
fi
echo "js-diff: generated JavaScript matches the interpreter on seeds $seeds of $files on $targets"
