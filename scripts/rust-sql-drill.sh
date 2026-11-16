#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

targets="${SQL_DRILL_TARGETS:-native-sqlite server-postgres}"
mode="${SQL_DRILL_MODE:-all}"
sessions="${SQL_DRILL_SESSIONS:-8}"
turns="${SQL_DRILL_TURNS:-6}"
delay_ms_max="${SQL_DRILL_DELAY_MS_MAX:-60}"
kills_per_session_max="${SQL_DRILL_KILLS_MAX:-400}"
takeover_externals="${SQL_DRILL_TAKEOVER_EXTERNALS:-1 2 3 4 5 6}"
takeover_turns="${SQL_DRILL_TAKEOVER_TURNS:-12}"
takeover_lease_ms="${SQL_DRILL_LEASE_MS:-200}"
file="${SQL_DRILL_FILE:-examples/agent.hl}"
seed_base="${SQL_DRILL_SEED:-0}"
mutation="${SQL_DRILL_MUTATION:-none}"
pg_port="${SQL_DRILL_PG_PORT:-55471}"
pg_container="${SQL_DRILL_PG_CONTAINER:-hl-rust-sql-drill}"
pg_image="${SQL_DRILL_PG_IMAGE:-postgres:16}"
hl=target/release/hl
out=output/rust-sql-drill
long_lease_ms=60000
lease_margin_ms=100
pause_polls_max=10000
pause_poll_seconds=0.002
pg_ready_polls=300
pg_ready_poll_seconds=0.1
held_retries_max=50
verify_retries_max=10
killed_status=137
held_status=3
fenced_status=4
store_lost_status=5
case "$mutation" in
  none) ;;
  flush_after_dispatch | no_fence) out="$out-$mutation" ;;
  *)
    echo "drill: unknown SQL_DRILL_MUTATION $mutation (none, flush_after_dispatch or no_fence)" >&2
    exit 2
    ;;
esac
mkdir -p "$out"

finished=0
started_postgres=0
killer_pid=""
report() {
  local status=$?
  if [ -n "$killer_pid" ]; then
    kill "$killer_pid" 2> /dev/null || true
  fi
  if [ "$started_postgres" -eq 1 ]; then
    docker stop "$pg_container" > /dev/null 2>&1 || true
  fi
  if [ "$finished" -ne 1 ]; then
    echo "drill: FAILED (exit $status)"
    [ "$status" -ne 0 ] || status=1
  fi
  exit "$status"
}
trap report EXIT

fail() {
  echo "drill: $*" >&2
  exit 1
}

uses_postgres() {
  case " $targets " in *" server-postgres "*) return 0 ;; esac
  return 1
}

pg_admin() {
  PGOPTIONS='-c client_min_messages=warning' psql -h 127.0.0.1 -p "$pg_port" -U postgres -X -q -At -v ON_ERROR_STOP=1 -d postgres "$@"
}

wait_postgres() {
  for _ in $(seq 1 "$pg_ready_polls"); do
    if pg_admin -c 'SELECT 1' > /dev/null 2>&1; then
      return 0
    fi
    sleep "$pg_ready_poll_seconds"
  done
  fail "postgres on port $pg_port did not accept connections"
}

start_postgres() {
  if ! docker inspect "$pg_container" > /dev/null 2>&1; then
    docker run -d --rm --name "$pg_container" -e POSTGRES_HOST_AUTH_METHOD=trust \
      -p "127.0.0.1:$pg_port:5432" "$pg_image" > /dev/null
    started_postgres=1
  fi
  wait_postgres
  pg_admin -c "SELECT current_setting('server_version_num')::int >= 160000" | grep -qx t ||
    fail "postgres on port $pg_port is older than 16"
  coproc KILLER { docker exec -i "$pg_container" sh -c 'while read pid; do kill -9 "$pid" 2> /dev/null; done'; }
  killer_pid=$KILLER_PID
}

build() {
  local target=$1 dir=$out/$1/build plant=none
  mkdir -p "$dir"
  [ "$mutation" = flush_after_dispatch ] && plant=flush_after_dispatch
  "$hl" emit --target "$target" --plant "$plant" "$file" > "$dir/program.wasm"
  for source in module_sql_drill_host sql_drill_checks sqlite_store postgres_store; do
    cp "scripts/rust/$source.rs" "$dir/$source.rs"
  done
  local key
  key=$(cat "$dir"/*.rs target/release/libharnesslang.rlib | cksum | cut -d' ' -f1)
  if [ ! -x "$dir/host" ] || [ "$(cat "$dir/key" 2> /dev/null)" != "$key" ]; then
    nice rustc --edition 2024 -O -o "$dir/host" "$dir/module_sql_drill_host.rs" \
      --extern harnesslang=target/release/libharnesslang.rlib -L target/release/deps
    echo "$key" > "$dir/key"
  fi
}

host() {
  local target=$1 command=$2 dir=$out/$1/build
  local fence=none
  [ "$mutation" = no_fence ] && fence=no_fence
  case "$command" in
    run) exec "$dir/host" run "target=$target" "module=$dir/program.wasm" "mutation=$fence" "${@:3}" ;;
    *) exec "$dir/host" "$command" "target=$target" "${@:3}" ;;
  esac
}

store_for() {
  local target=$1 dir=$2
  if [ "$target" = server-postgres ]; then
    echo "127.0.0.1:$pg_port/hl_rust_sql_drill"
  else
    echo "$dir/journal.db"
  fi
}

wait_file_or_exit() {
  local path=$1 pid=$2
  for _ in $(seq 1 "$pause_polls_max"); do
    [ -e "$path" ] && return 0
    if ! kill -0 "$pid" 2> /dev/null; then
      [ -e "$path" ] && return 0
      return 1
    fi
    sleep "$pause_poll_seconds"
  done
  fail "$path did not appear within $pause_polls_max polls"
}

ms_seconds() {
  printf '%d.%03d' $(($1 / 1000)) $(($1 % 1000))
}

verify_store() {
  local target=$1
  shift
  local status
  for _ in $(seq 1 "$verify_retries_max"); do
    status=0
    (host "$target" verify "$@") 2>&1 || status=$?
    [ "$status" -eq "$store_lost_status" ] || return "$status"
    wait_postgres
  done
  echo "drill: verification found the store unavailable $verify_retries_max times"
  return 1
}

total_kills=0
total_client_kills=0
total_backend_kills=0
total_runs=0
total_lost=0

crash_session() {
  local target=$1 session=$2
  local dir=$out/$target/crash-$session seed=$((seed_base + session))
  rm -rf "$dir"
  mkdir -p "$dir"
  local store
  store=$(store_for "$target" "$dir")
  RANDOM=$seed
  local kills=0 client_kills=0 backend_kills=0 runs=0 lost=0
  while true; do
    runs=$((runs + 1))
    local backend_run=0
    if [ "$target" = server-postgres ] && [ $((runs % 2)) -eq 0 ]; then backend_run=1; fi
    rm -f "$dir/pid"
    host "$target" run "store=$store" "session=crash-$seed" owner=crash "lease_ms=$long_lease_ms" \
      "ledger=$dir/ledger" "acks=$dir/acks" "seed=$seed" "turns=$turns" "pid_file=$dir/pid" 2>> "$dir/stderr" &
    local pid=$! delay_ms=$((RANDOM % (delay_ms_max + 1))) sent=0
    if [ "$backend_run" -eq 1 ]; then
      if wait_file_or_exit "$dir/pid" "$pid"; then
        sleep "$(ms_seconds "$delay_ms")"
        cat "$dir/pid" >&"${KILLER[1]}"
        echo >&"${KILLER[1]}"
        sent=1
      fi
    else
      sleep "$(ms_seconds "$delay_ms")"
      kill -9 "$pid" 2> /dev/null || true
    fi
    local status=0
    { wait "$pid"; } 2> /dev/null || status=$?
    case "$status" in
      0) break ;;
      "$killed_status")
        kills=$((kills + 1))
        client_kills=$((client_kills + 1))
        ;;
      "$store_lost_status")
        if [ "$sent" -eq 1 ]; then
          kills=$((kills + 1))
          backend_kills=$((backend_kills + 1))
        else
          lost=$((lost + 1))
        fi
        wait_postgres
        ;;
      *)
        echo "drill: $target session $session run $runs exited $status:" >&2
        tail -n 20 "$dir/stderr" >&2
        exit 1
        ;;
    esac
    [ "$kills" -lt "$kills_per_session_max" ] ||
      fail "$target session $session was killed kills_per_session_max ($kills_per_session_max) times without finishing"
    [ "$lost" -lt "$kills_per_session_max" ] ||
      fail "$target session $session lost its store $lost times without a kill"
  done
  local verified
  verified=$(verify_store "$target" "store=$store" "session=crash-$seed" "ledger=$dir/ledger" "acks=$dir/acks" "turns=$turns" taken_over=0) || {
    echo "drill: $target session $session failed verification after $kills kills:" >&2
    echo "$verified" >&2
    exit 1
  }
  echo "crash target=$target session=$session seed=$seed kills=$kills client=$client_kills backend=$backend_kills store_unavailable=$lost $verified"
  total_kills=$((total_kills + kills))
  total_client_kills=$((total_client_kills + client_kills))
  total_backend_kills=$((total_backend_kills + backend_kills))
  total_runs=$((total_runs + runs))
  total_lost=$((total_lost + lost))
}

start_owner() {
  local target=$1 dir=$2 owner=$3 lease_ms=$4 pause=$5 seed=$6
  host "$target" run "store=$(store_for "$target" "$dir")" "session=takeover-$seed" "owner=$owner" "lease_ms=$lease_ms" \
    "ledger=$dir/ledger" "acks=$dir/acks" "seed=$seed" "turns=$takeover_turns" "pause=$pause" "pause_dir=$dir" \
    2>> "$dir/stderr.$owner" &
}

second_pid=""
start_second_owner() {
  local target=$1 dir=$2 seed=$3
  for _ in $(seq 1 "$held_retries_max"); do
    start_owner "$target" "$dir" b "$long_lease_ms" flush "$seed"
    local pid=$!
    if wait_file_or_exit "$dir/paused.b" "$pid"; then
      second_pid=$pid
      return 0
    fi
    local status=0
    wait "$pid" || status=$?
    [ "$status" -eq "$held_status" ] || fail "$target takeover: b exited $status before its first flush"
    sleep "$(ms_seconds "$lease_margin_ms")"
  done
  fail "$target takeover: b found the lease held $held_retries_max times"
}

takeover() {
  local target=$1 external=$2
  local seed=$((seed_base + external)) dir=$out/$target/takeover-$external
  rm -rf "$dir"
  mkdir -p "$dir"
  start_owner "$target" "$dir" a "$takeover_lease_ms" "external:$external" "$seed"
  local first=$!
  if ! wait_file_or_exit "$dir/paused.a" "$first"; then
    local early=0
    wait "$first" || early=$?
    fail "$target takeover: a exited $early before its external $external"
  fi
  sleep "$(ms_seconds $((takeover_lease_ms + lease_margin_ms)))"
  start_second_owner "$target" "$dir" "$seed"
  touch "$dir/resume.a"
  local first_status=0 second_status=0
  wait "$first" || first_status=$?
  touch "$dir/resume.b"
  wait "$second_pid" || second_status=$?
  local verified verdict=ok
  verified=$(verify_store "$target" "store=$(store_for "$target" "$dir")" "session=takeover-$seed" "ledger=$dir/ledger" \
    "acks=$dir/acks" "turns=$takeover_turns" taken_over=1) || verdict=failed
  echo "takeover target=$target external=$external seed=$seed a_exit=$first_status b_exit=$second_status verify=$verdict $(echo "$verified" | grep -E '^(verified|assertion|[a-z_]+: )' | head -n 1)"
  [ "$first_status" -eq "$fenced_status" ] || fail "$target takeover external $external: a exited $first_status, not fenced ($fenced_status)"
  [ "$second_status" -eq 0 ] || fail "$target takeover external $external: b exited $second_status"
  [ "$verdict" = ok ] || fail "$target takeover external $external: verification failed"
}

[ -x "$hl" ] && [ -f target/release/libharnesslang.rlib ] || cargo build --release --locked -j1 --quiet
if uses_postgres; then
  start_postgres
  pg_admin -c "DROP DATABASE IF EXISTS hl_rust_sql_drill WITH (FORCE)" -c "CREATE DATABASE hl_rust_sql_drill"
fi
for target in $targets; do
  build "$target"
  echo "drill: $target host $(host "$target" version)"
done
if [ "$mode" = all ] || [ "$mode" = crash ]; then
  for target in $targets; do
    for session in $(seq 1 "$sessions"); do
      crash_session "$target" "$session"
    done
  done
  echo "drill: crash: $sessions sessions of $turns turns per target ($targets), seeds $((seed_base + 1))..$((seed_base + sessions)), survived $total_kills kill -9s ($total_client_kills of the host, $total_backend_kills of the Postgres backend) over $total_runs runs ($total_lost runs found the store unavailable): external_once, unknown_reported, idempotent_known, finish_once, acks and the fence hold in the real databases"
fi
if [ "$mode" = all ] || [ "$mode" = takeover ]; then
  for target in $targets; do
    for external in $takeover_externals; do
      takeover "$target" "$external"
    done
  done
  echo "drill: takeover: on $targets an owner with a $takeover_lease_ms ms lease paused in its external N ($takeover_externals) was fenced dispatching nothing once a second owner took the session, which finished $takeover_turns turns"
fi
finished=1
