#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${RUST_DIFF_SEEDS:-0..2000}"
targets="${RUST_DIFF_TARGETS:-native-jsonl native-sqlite server-postgres}"
files="${RUST_DIFF_FILES:-$(echo examples/*.hl)}"
opt="${RUST_DIFF_OPT:-2}"
plant="${RUST_DIFF_PLANT:-none}"
scope="${RUST_DIFF_SCOPE:-}"
scope_args=()
suffix=
if [ -n "$scope" ]; then
  scope_args=(--scope "$scope")
  suffix=-scoped
fi
hl=target/release/hl
out=output/rust-diff/$plant
mkdir -p "$out"

[ -x "$hl" ] || cargo build --release --locked

plant_expression() {
  case "$1" in
    none) echo 's/^$//' ;;
    flush_after_dispatch) echo 's/if site.durability.flushes(call.effect) {/if site.durability.flushes(call.effect) \&\& false {/' ;;
    bound_message) echo 's/format!("bound({name})")/format!("bound {name}")/' ;;
    double_fsync) echo 's/^    disk.sync().map_err(|_| Stop::Crash)?;$/    disk.sync().map_err(|_| Stop::Crash)?;\n    disk.sync().map_err(|_| Stop::Crash)?;/' ;;
    batch_seq_from_zero) echo 's/("seq", Json::Number(head + 1 + index as u64))/("seq", Json::Number(head + index as u64))/' ;;
    recovery_skipped) echo 's/SiteLog::Open(call) if call.effect.may_be_unknown() => Some(\*seq),/SiteLog::Open(call) if false => Some(*seq),/' ;;
    nested_flush_after_dispatch) echo 's/if tool.effect.may_be_unknown() {/if false \&\& tool.effect.may_be_unknown() {/' ;;
    timeout_rides) echo 's/if self.header_unsaved {/if false \&\& self.header_unsaved {/' ;;
    enum_unchecked) echo 's/None => checked_output(\&call.tool, returns, output),$/None => output,/' ;;
    nested_enum_unchecked) echo 's/Ok(checked_output(tool.name, tool.returns, output))$/Ok(output)/' ;;
    default_unfilled) echo 's/fields.entry(name.to_owned()).or_insert(value);/drop((fields, name, value));/' ;;
    mode_dropped) echo 's/                    mode: site.mode,$/                    mode: None,/' ;;
    rewrite_kept_lost) echo 's/ => rewritten.push(kept),$/ => drop(kept),/' ;;
    settings_dropped) echo 's/                        settings: settings.as_ref(),$/                        settings: None,/' ;;
    payment_unset) echo 's/fields.insert(String::from("payment"), Json::String(payment));/drop(payment);/' ;;
    text_before_whole) echo 's/Some((before, _)) => before.to_owned(),/Some(_) => subject.clone(),/' ;;
    blob_unjournaled) echo 's/Json::object(\[(BLOB_KEY, Json::text(\&digest)), (DATA_KEY, Json::text(base64))\])/digest_json(\&digest)/' ;;
    grandchild_unscoped) echo 's/^    if scope.is_empty() {$/    if true {/' ;;
    idle_early) echo 's/^                if clock < due {$/                if false \&\& clock < due {/' ;;
    interrupted_undecoded) echo 's/        (0, Some(Json::Array(items))) if items.is_empty() => Ok(Vec::new()),$/        (0, Some(Json::Array(items))) if false \&\& items.is_empty() => Ok(Vec::new()),/' ;;
    interrupt_ignored) echo 's/let interrupted = self.drain_interrupting(interrupts)? \&\& !preempted;/let interrupted = self.drain_interrupting(interrupts)? \&\& false;/' ;;
    recover_skipped) echo 's/let recovered = if program::recoverable(&call.tool) {/let recovered = if false {/' ;;
    job_lost_dropped) echo 's/        .map(|key| lost_delivery(key))$/        .filter(|_| false).map(|key| lost_delivery(key))/' ;;
    fork_hook_skipped) echo 's/program::run_forked(\&mut state, origin.to_owned(), discarded)/Ok::<(), Halt>(())/' ;;
    fork_entry_ignored) echo 's/let entry = \*revision == ENTRY_REVISION \&\& session.turns.is_empty() \&\& session.start == Start::Beginning;/let entry = *revision == ENTRY_REVISION \&\& session.turns.is_empty() \&\& session.start == Start::Beginning; if entry { return Ok(()); }/' ;;
    cell_leaks) echo 's/^        if !cells_yield() {$/        if true {/' ;;
    cell_cancel_ignored) echo 's/^        if !program::CODES\[code\].cancel {$/        if true {/' ;;
    *) echo "rust-diff: unknown plant $1" >&2; return 1 ;;
  esac
}

store_of() {
  case "$1" in
    native-jsonl) echo jsonl ;;
    *) echo sql ;;
  esac
}

build() {
  local file=$1 target=$2 dir=$3
  mkdir -p "$dir"
  "$hl" emit --target "$target" "$file" | sed "$(plant_expression "$plant")" > "$dir/harness.rs.new"
  if [ "$plant" != none ] && cmp -s <("$hl" emit --target "$target" "$file") "$dir/harness.rs.new"; then
    echo "rust-diff: plant $plant changes nothing in $file for $target" >&2
    return 1
  fi
  cp scripts/rust/sim_driver.rs "$dir/sim_driver.rs"
  cp "scripts/rust/sim_store_$(store_of "$target").rs" "$dir/store.rs"
  mv "$dir/harness.rs.new" "$dir/harness.rs"
  local key
  key=$(cat "$dir/harness.rs" "$dir/sim_driver.rs" "$dir/store.rs" | cksum | cut -d' ' -f1)-$opt
  if [ -x "$dir/driver" ] && [ "$(cat "$dir/key" 2>/dev/null)" = "$key" ]; then
    return
  fi
  local started ended
  started=$(date +%s%N)
  rustc --edition 2024 -C opt-level="$opt" -o "$dir/driver" "$dir/sim_driver.rs" 2> "$dir/rustc.log" || {
    cat "$dir/rustc.log" >&2
    return 1
  }
  ended=$(date +%s%N)
  echo "$key" > "$dir/key"
  echo "compiled $file for $target: $(wc -l < "$dir/harness.rs") lines, $(( (ended - started) / 1000000 )) ms, opt-level $opt"
}

diff_one() {
  local file=$1 target=$2
  local name
  name=$(basename "$file" .hl)
  local dir="$out/$target-$name$suffix"
  build "$file" "$target" "$dir"
  local fifo="$dir/trace"
  rm -f "$fifo"
  mkfifo "$fifo"
  "$dir/driver" "$fifo" > "$dir/driver.log" 2>&1 &
  local driver=$!
  local status=0
  local started ended
  started=$(date +%s%N)
  "$hl" sim --target "$target" --seeds "$seeds" --trace "$fifo" "${scope_args[@]}" "$file" > "$dir/sim.log" 2>&1 || status=$?
  wait "$driver" || status=1
  ended=$(date +%s%N)
  rm -f "$fifo"
  if [ "$status" -ne 0 ] || ! grep -q '^ok backend=rust' "$dir/driver.log"; then
    echo "rust-diff: $file on $target failed:" >&2
    cat "$dir/sim.log" "$dir/driver.log" >&2
    return 1
  fi
  echo "$(cat "$dir/driver.log") wall_ms=$(( (ended - started) / 1000000 ))"
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
  echo "rust-diff: a differential run failed" >&2
  exit 1
fi
echo "rust-diff: generated Rust matches the interpreter on seeds $seeds of $files on $targets"
