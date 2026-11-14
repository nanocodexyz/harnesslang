#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

turns="${BENCH_TURNS:-50}"
rounds="${BENCH_ROUNDS:-5}"
hl_root="$(cd "${HL_ROOT:-$PWD}" && pwd)"
file="${BENCH_FILE:-$hl_root/examples/agent.hl}"
contenders="${BENCH_CONTENDERS:-hl-jsonl hl-sqlite nanocode-turnlog}"
plant="${BENCH_PLANT:-none}"
use_strace="${BENCH_STRACE:-auto}"
sqlite_lib="${BENCH_SQLITE_LIB:-/usr/lib/x86_64-linux-gnu/libsqlite3.so.0}"
opt="${BENCH_OPT:-2}"
out="${BENCH_OUT:-$PWD/output/bench}"
hl="$hl_root/target/release/hl"
turnlog_bench="$PWD/oracle/target/release/turnlog_bench"
jsonl_sync='^    disk.sync().map_err(|_| Stop::Crash)?;$'
jsonl_directory_syncs=1

finished=0
report() {
  local status=$?
  if [ "$finished" -ne 1 ]; then
    echo "bench: FAILED (exit $status)"
    [ "$status" -ne 0 ] || status=1
  fi
  exit "$status"
}
trap report EXIT

fail() {
  echo "bench: $*" >&2
  exit 1
}

case "$plant" in
  none | extra_fsync | sidecar_short | sidecar_merge) ;;
  *) fail "unknown BENCH_PLANT $plant (none, extra_fsync, sidecar_short or sidecar_merge)" ;;
esac
case "$use_strace" in
  auto) if command -v strace > /dev/null; then use_strace=1; else use_strace=0; fi ;;
  0 | 1) ;;
  *) fail "BENCH_STRACE is auto, 0 or 1, not $use_strace" ;;
esac
[ "$use_strace" -eq 0 ] || command -v strace > /dev/null || fail "BENCH_STRACE=1 but strace is not installed"
[ "$turns" -ge 1 ] && [ "$rounds" -ge 1 ] || fail "BENCH_TURNS and BENCH_ROUNDS are at least 1"
for contender in $contenders; do
  case "$contender" in
    hl-jsonl | hl-sqlite | nanocode-turnlog) ;;
    *) fail "unknown contender $contender (hl-jsonl, hl-sqlite or nanocode-turnlog)" ;;
  esac
done

rm -rf "$out/runs" "$out/input"
mkdir -p "$out/build" "$out/runs" "$out/input"

if [ ! -x "$hl" ]; then
  (cd "$hl_root" && nice cargo build --release --locked -j1 --quiet)
fi
if [[ " $contenders " == *" nanocode-turnlog "* ]]; then
  (cd oracle && nice cargo build --release --locked -j1 --quiet --bin turnlog_bench)
fi

build_host() {
  local target=$1 store=$2
  local dir="$out/build/$target"
  mkdir -p "$dir"
  "$hl" emit --target "$target" "$file" > "$dir/harness.rs.new"
  if [ "$plant" = extra_fsync ] && [ "$target" = native-jsonl ]; then
    [ "$(grep -c "$jsonl_sync" "$dir/harness.rs.new")" -eq 1 ] ||
      fail "the generated native-jsonl code no longer has one sync per flush to plant a second after"
    sed -i "s/$jsonl_sync/&\n    disk.sync().map_err(|_| Stop::Crash)?;/" "$dir/harness.rs.new"
  fi
  mv "$dir/harness.rs.new" "$dir/harness.rs"
  cp scripts/rust/bench_host.rs "$dir/bench_host.rs"
  cp "scripts/rust/bench_store_$store.rs" "$dir/store.rs"
  local link=()
  [ "$store" = jsonl ] || link=(-C "link-arg=$sqlite_lib")
  local key
  key=$(cat "$dir/harness.rs" "$dir/bench_host.rs" "$dir/store.rs" | cksum | cut -d' ' -f1)-$opt-$sqlite_lib
  if [ -x "$dir/host" ] && [ "$(cat "$dir/key" 2> /dev/null)" = "$key" ]; then
    return
  fi
  nice rustc --edition 2024 -C opt-level="$opt" "${link[@]}" -o "$dir/host" "$dir/bench_host.rs" 2> "$dir/rustc.log" ||
    { cat "$dir/rustc.log" >&2; fail "the $target bench host does not compile"; }
  echo "$key" > "$dir/key"
}

build_host native-jsonl jsonl
[[ " $contenders " != *" hl-sqlite "* ]] || build_host native-sqlite sql

field() {
  local name=$1 path=$2
  local value
  value=$(sed -n "s/.* $name=\([^ ]*\).*/\1/p" "$path")
  [ -n "$value" ] || fail "$path has no $name"
  echo "$value"
}

on_disk() {
  local data=$1 total=0 path
  for path in "$data" "$data-wal" "$data-shm" "$data-journal"; do
    [ ! -e "$path" ] || total=$((total + $(stat -c %s "$path")))
  done
  echo "$total"
}

traced() {
  local trace=$1
  shift
  if [ "$use_strace" -eq 1 ]; then
    strace -f -qq --seccomp-bpf -c -e trace=fsync,fdatasync -o "$trace" "$@"
  else
    "$@"
  fi
}

fsyncs_of() {
  local trace=$1
  awk '$NF == "fsync" || $NF == "fdatasync" { calls += $4 } END { print calls + 0 }' "$trace"
}

input="$out/input"
"$out/build/native-jsonl/host" run "$input/journal.jsonl" "$turns" > "$input/run.out" ||
  fail "the hl-jsonl run that writes the reference journal failed"
reference_records=$(field records "$input/run.out")
case "$plant" in
  sidecar_short) sed -i '$d' "$input/journal.flushes" ;;
  sidecar_merge)
    awk 'NR == 1 { first = $1; next } NR == 2 { print first + $1; next } { print }' "$input/journal.flushes" > "$input/merged"
    mv "$input/merged" "$input/journal.flushes"
    ;;
esac

run_one() {
  local contender=$1 round=$2
  local dir="$out/runs/$contender-$round"
  mkdir -p "$dir"
  local data run open
  case "$contender" in
    hl-jsonl)
      data="$dir/journal.jsonl"
      run=("$out/build/native-jsonl/host" run "$data" "$turns")
      open=("$out/build/native-jsonl/host" open "$data" "$turns")
      ;;
    hl-sqlite)
      data="$dir/journal.db"
      run=("$out/build/native-sqlite/host" run "$data" "$turns")
      open=("$out/build/native-sqlite/host" open "$data" "$turns")
      ;;
    nanocode-turnlog)
      data="$dir/turnlog.log"
      run=("$turnlog_bench" run "$input/journal.jsonl" "$input/journal.flushes" "$data" "$turns")
      open=("$turnlog_bench" open "$data" "$turns")
      ;;
  esac
  traced "$dir/strace" "${run[@]}" > "$dir/run.out" 2> "$dir/run.err" ||
    { cat "$dir/run.err" >&2; fail "$contender round $round: the run failed"; }
  local bytes
  bytes=$(on_disk "$data")
  "${open[@]}" > "$dir/open.out" 2> "$dir/open.err" ||
    { cat "$dir/open.err" >&2; fail "$contender round $round: the open failed"; }
  check_run "$contender" "$round" "$dir" "$data" "$bytes"
}

check_run() {
  local contender=$1 round=$2 dir=$3 data=$4 bytes=$5
  local records counted fsyncs=n/a
  records=$(field records "$dir/run.out")
  [ "$records" -eq "$reference_records" ] ||
    fail "$contender round $round wrote $records records, the reference journal holds $reference_records"
  if [ "$use_strace" -eq 1 ]; then
    fsyncs=$(fsyncs_of "$dir/strace")
  fi
  if [ "$contender" != hl-sqlite ]; then
    counted=$(field syncs_counted "$dir/run.out")
    if [ "$use_strace" -eq 1 ]; then
      [ "$fsyncs" -eq "$counted" ] ||
        fail "$contender round $round: strace saw $fsyncs fsyncs, the process counted $counted"
    fi
    fsyncs=$counted
  fi
  if [ "$contender" = hl-jsonl ]; then
    cmp -s "$data" "$input/journal.jsonl" || fail "hl-jsonl round $round wrote a journal unlike the reference run's"
    local flushes model
    flushes=$(field flushes "$dir/run.out")
    model=$((flushes + turns + jsonl_directory_syncs))
    [ "$fsyncs" -eq "$model" ] ||
      fail "hl-jsonl round $round made $fsyncs fsyncs; native-jsonl's model is one per flush ($flushes), one per open ($turns) and one for the new file's directory ($jsonl_directory_syncs): $model"
  fi
  local wall cpu open_ms flush_ms line
  wall=$(field wall_ms "$dir/run.out")
  cpu=$(field cpu_ms "$dir/run.out")
  flush_ms=$(field flush_ms "$dir/run.out")
  open_ms=$(field open_ms "$dir/open.out")
  line=$(awk -v c="$contender" -v r="$round" -v t="$turns" -v w="$wall" -v f="$flush_ms" -v u="$cpu" -v b="$bytes" -v o="$open_ms" -v s="$fsyncs" -v n="$records" 'BEGIN {
    printf "bench contender=%s round=%d turns=%d wall_ms=%.1f ms_per_turn=%.3f cpu_ms_per_turn=%.3f flush_ms=%.1f bytes=%d bytes_per_turn=%.0f open_ms=%.3f fsyncs=%s records=%d", c, r, t, w, w / t, u / t, f, b, b / t, o, s, n
  }')
  echo "$line"
  echo "$line" >> "$out/results"
}

echo "bench: $file from $hl_root ($(git -C "$hl_root" rev-parse --short HEAD 2> /dev/null || echo unknown)), $turns turns, $rounds rounds, contenders: $contenders, plant: $plant, rustc opt-level $opt, $(rustc --version)"
echo "bench: $(nproc) cores, load $(cut -d' ' -f1-3 /proc/loadavg), $(df -T "$out" | awk 'NR == 2 { print $2 }') at $out, fsyncs $([ "$use_strace" -eq 1 ] && echo 'counted by strace -f -c -e trace=fsync,fdatasync' || echo 'counted by the processes themselves (no strace; hl-sqlite n/a)')"
rm -f "$out/results"
read -r -a order <<< "$contenders"
for round in $(seq 1 "$rounds"); do
  if [ $((round % 2)) -eq 1 ]; then
    sequence=("${order[@]}")
  else
    sequence=()
    for ((index = ${#order[@]} - 1; index >= 0; index--)); do
      sequence+=("${order[index]}")
    done
  fi
  for contender in "${sequence[@]}"; do
    run_one "$contender" "$round"
  done
done

summary() {
  awk '
    function value(line, name,   at, rest) {
      at = index(line, " " name "=")
      rest = substr(line, at + length(name) + 2)
      sub(/ .*/, "", rest)
      return rest
    }
    function stats(list, count,   sorted, i, j, swap, median) {
      for (i = 1; i <= count; i++) sorted[i] = list[i] + 0
      for (i = 1; i <= count; i++) for (j = i + 1; j <= count; j++) if (sorted[j] < sorted[i]) { swap = sorted[i]; sorted[i] = sorted[j]; sorted[j] = swap }
      median = count % 2 ? sorted[(count + 1) / 2] : (sorted[count / 2] + sorted[count / 2 + 1]) / 2
      return sprintf("%.3f [%.3f..%.3f]", median, sorted[1], sorted[count])
    }
    {
      c = value($0, "contender")
      if (!(c in runs)) order[++contenders] = c
      n = ++runs[c]
      per_turn[c, n] = value($0, "ms_per_turn"); cpu[c, n] = value($0, "cpu_ms_per_turn"); open[c, n] = value($0, "open_ms"); flush[c, n] = value($0, "flush_ms")
      bytes[c] = value($0, "bytes"); bytes_turn[c] = value($0, "bytes_per_turn"); fsyncs[c] = value($0, "fsyncs")
    }
    END {
      printf "%-17s %-30s %-30s %-28s %-32s %10s %14s %8s\n", "contender", "ms_per_turn median [min..max]", "cpu_ms_per_turn median [min..max]", "open_ms median [min..max]", "flush_ms median [min..max]", "bytes", "bytes_per_turn", "fsyncs"
      for (k = 1; k <= contenders; k++) {
        c = order[k]
        for (i = 1; i <= runs[c]; i++) { a[i] = per_turn[c, i]; u[i] = cpu[c, i]; o[i] = open[c, i]; f[i] = flush[c, i] }
        printf "%-17s %-30s %-30s %-28s %-32s %10s %14s %8s\n", c, stats(a, runs[c]), stats(u, runs[c]), stats(o, runs[c]), stats(f, runs[c]), bytes[c], bytes_turn[c], fsyncs[c]
      }
    }' "$out/results"
}

varying=$(sed 's/ round=[0-9]*//' "$out/results" | awk '{ key = ""; for (i = 1; i <= NF; i++) if ($i ~ /^(contender|bytes|fsyncs)=/) key = key " " $i; print key }' | sort -u | awk '{ print $1 }' | uniq -d)
[ -z "$varying" ] || echo "bench: note: bytes or fsyncs differ between rounds for $varying"
echo "bench: medians over $rounds rounds of $turns turns"
summary
finished=1
