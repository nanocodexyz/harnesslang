#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

hl=$PWD/target/release/hl
scratch=$PWD/output/sql-drill
program=$PWD/examples/agent.hl
replay_seeds="${SQL_REPLAY_SEEDS:-0..200}"
race_runs="${SQL_RACE_RUNS:-200}"
mutation_race_runs="${SQL_MUTATION_RACE_RUNS:-60}"
race_parallel="${SQL_RACE_PARALLEL:-4}"
race_jitter_ms="${SQL_RACE_JITTER_MS:-20}"
crash_runs="${SQL_CRASH_RUNS:-6}"
crash_turns="${SQL_CRASH_TURNS:-2000}"
crash_cache_pages=8
crash_polls=600
crash_poll_seconds=0.05
busy_timeout_ms=30000
long_lease_ms=60000
expired_lease_ms=0
pg_port="${SQL_PG_PORT:-55433}"
pg_container="${SQL_PG_CONTAINER:-hl-sql-drill}"
pg_image="${SQL_PG_IMAGE:-postgres:16}"
pg_ready_polls=150
drill_seed="${SQL_DRILL_SEED:-1}"

RANDOM=$drill_seed
rm -rf "$scratch"
mkdir -p "$scratch/db"
[ -x "$hl" ] || cargo build --release --locked

fail() {
  echo "sql-drill: FAIL: $*" >&2
  exit 1
}

started_postgres=0
stop_postgres() {
  if [ "$started_postgres" -eq 1 ]; then
    docker stop "$pg_container" > /dev/null 2>&1 || true
  fi
}
trap stop_postgres EXIT

pg_admin() {
  PGOPTIONS='-c client_min_messages=warning' psql -h 127.0.0.1 -p "$pg_port" -U postgres -X -q -v ON_ERROR_STOP=1 -d postgres "$@"
}

pg() {
  local db=$1
  shift
  psql -h 127.0.0.1 -p "$pg_port" -U postgres -X -q -v ON_ERROR_STOP=1 -v VERBOSITY=verbose -d "$db" "$@"
}

wait_postgres() {
  for _ in $(seq 1 "$pg_ready_polls"); do
    if pg_admin -c 'SELECT 1' > /dev/null 2>&1; then
      return 0
    fi
    sleep 0.2
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
}

store() {
  case "$1" in
    server-postgres) echo postgres ;;
    native-sqlite | cloudflare) echo sqlite ;;
    *) fail "no SQL store for target $1" ;;
  esac
}

fresh_store() {
  local target=$1 name=$2
  name=${name//-/_}
  if [ "$(store "$target")" = postgres ]; then
    pg_admin -c "DROP DATABASE IF EXISTS $name WITH (FORCE)" -c "CREATE DATABASE $name"
    echo "$name"
  else
    rm -f "$scratch/db/$name.db" "$scratch/db/$name.db-wal" "$scratch/db/$name.db-shm" "$scratch/db/$name.db-journal"
    echo "$scratch/db/$name.db"
  fi
}

run_sql() {
  local target=$1 where=$2
  if [ "$(store "$target")" = postgres ]; then
    pg "$where" -At -F '|'
  else
    { echo ".timeout $busy_timeout_ms"; cat; } | sqlite3 -bail "$where"
  fi
}

dump() {
  local target=$1 where=$2
  local journal="SELECT 'journal', session, seq, epoch, turn, kind, record FROM hl_journal"
  local sessions="SELECT 'session', session, epoch, head, finished, checkpoint FROM hl_sessions"
  if [ "$(store "$target")" = postgres ]; then
    pg "$where" -At -F $'\t' -c "$journal" -c "$sessions"
  else
    sqlite3 -separator $'\t' "$where" "$journal" "$sessions"
  fi
}

scalar() {
  local target=$1 where=$2 query=$3
  echo "$query;" | run_sql "$target" "$where"
}

render() {
  local target=$1 mutation=$2
  shift 2
  "$hl" sql render --target "$target" --mutation "$mutation" "$@"
}

schema() {
  local target=$1 mutation=$2 where=$3
  { render "$target" "$mutation" connect; render "$target" "$mutation" schema; } | run_sql "$target" "$where" > /dev/null
}

commit_group() {
  local target=$1 mutation=$2
  shift 2
  if [ "$target" = cloudflare ]; then
    echo "BEGIN;"
    render "$target" "$mutation" commit "$@"
    echo "COMMIT;"
  else
    render "$target" "$mutation" commit "$@"
  fi
}

fenced_text() {
  if [ "$(store "$1")" = postgres ]; then
    echo 'null value in column "epoch" of relation "hl_journal"'
  else
    echo 'NOT NULL constraint failed: hl_journal.epoch'
  fi
}

duplicate_text() {
  if [ "$(store "$1")" = postgres ]; then
    echo 'duplicate key value violates unique constraint "hl_journal_pkey"'
  else
    echo 'UNIQUE constraint failed: hl_journal.session, hl_journal.seq'
  fi
}

ms_seconds() {
  printf '0.%03d' "$1"
}

now_ns() {
  date +%s%N
}

finished_turn() {
  local file=$1 head=$2 turn=$3 turns=$4
  "$hl" sql batch --head "$head" --turn "$turn" --turns "$turns" > "$file"
}

replay() {
  local target=$1 mutation=$2
  local name=replay-$target dir=$scratch/replay-$target
  mkdir -p "$dir"
  local where
  where=$(fresh_store "$target" "$name")
  "$hl" sql replay --target "$target" --seeds "$replay_seeds" --mutation "$mutation" "$program" \
    > "$dir/replay.sql" 2> "$dir/report"
  local fenced
  fenced=$(sed -n 's/.* fenced=\([0-9]*\) .*/\1/p' "$dir/report")
  local start end
  start=$(now_ns)
  if [ "$(store "$target")" = postgres ]; then
    pg "$where" -v ON_ERROR_STOP=0 -f "$dir/replay.sql" > "$dir/loaded" 2> "$dir/errors" || true
    local errors refused
    errors=$(grep -c ': ERROR:' "$dir/errors" || true)
    refused=$(grep -c "$(fenced_text "$target")" "$dir/errors" || true)
    [ "$errors" -eq "$fenced" ] && [ "$refused" -eq "$fenced" ] ||
      fail "replay $target: expected $fenced fenced commits refused, saw $refused refusals among $errors errors (see $dir/errors)"
  else
    sqlite3 "$where" < "$dir/replay.sql" > "$dir/loaded" 2> "$dir/errors" || true
    local errors refused unopened
    errors=$(grep -c 'error near line' "$dir/errors" || true)
    refused=$(grep -c "$(fenced_text "$target")" "$dir/errors" || true)
    unopened=$(grep -c 'cannot commit - no transaction is active' "$dir/errors" || true)
    [ "$refused" -eq "$fenced" ] && [ "$errors" -eq $((refused + unopened)) ] && [ "$unopened" -le "$fenced" ] ||
      fail "replay $target: expected $fenced fenced commits refused, saw $refused refusals among $errors errors (see $dir/errors)"
  fi
  end=$(now_ns)
  dump "$target" "$where" > "$dir/dump.tsv"
  "$hl" sql verify --target "$target" --seeds "$replay_seeds" "$program" "$dir/dump.tsv"
  "$hl" sql decode "$dir/dump.tsv" > /dev/null
  local size
  if [ "$(store "$target")" = postgres ]; then
    size=$(scalar "$target" "$where" "SELECT pg_total_relation_size('hl_journal') + pg_total_relation_size('hl_sessions')")
  else
    sqlite3 "$where" 'PRAGMA wal_checkpoint(TRUNCATE)' > /dev/null
    size=$(stat -c %s "$where")
  fi
  echo "replay $target: $(cat "$dir/report" | sed 's/^replay //'); real database: $(((end - start) / 1000000)) ms, $(stat -c %s "$dir/loaded") bytes loaded, $size bytes stored"
}

lease() {
  local target=$1 mutation=$2
  local where
  where=$(fresh_store "$target" "lease-$target")
  schema "$target" "$mutation" "$where"
  local first refused renewed
  first=$(render "$target" "$mutation" acquire session=lease owner=a "lease_ms=$long_lease_ms" | run_sql "$target" "$where")
  refused=$(render "$target" "$mutation" acquire session=lease owner=b "lease_ms=$long_lease_ms" | run_sql "$target" "$where")
  renewed=$(render "$target" "$mutation" acquire session=lease owner=a "lease_ms=$expired_lease_ms" | run_sql "$target" "$where")
  local taken
  taken=$(render "$target" "$mutation" acquire session=lease owner=b "lease_ms=$expired_lease_ms" | run_sql "$target" "$where")
  [ "$first" = "1|0" ] || fail "lease $target: first acquire returned '$first', expected epoch 1 head 0"
  [ -z "$refused" ] || fail "lease $target: a second owner acquired an unexpired lease: '$refused'"
  [ "$renewed" = "2|0" ] || fail "lease $target: the owner's own reacquire returned '$renewed'"
  [ "$taken" = "3|0" ] || fail "lease $target: a second owner could not take an expired lease: '$taken'"
  echo "lease $target: an unexpired lease refuses a second owner; its owner reacquires; an expired lease is taken over"
}

duplicate() {
  local target=$1 mutation=$2
  local dir=$scratch/duplicate-$target
  mkdir -p "$dir"
  local where
  where=$(fresh_store "$target" "duplicate-$target")
  schema "$target" "$mutation" "$where"
  finished_turn "$dir/batch" 0 1 1
  {
    render "$target" "$mutation" acquire session=dup owner=a "lease_ms=$long_lease_ms"
    commit_group "$target" "$mutation" session=dup epoch=1 head=0 "lease_ms=$long_lease_ms" "batch=@$dir/batch"
  } | run_sql "$target" "$where" > /dev/null
  local raw="INSERT INTO hl_journal (session, seq, epoch, turn, kind, record) SELECT session, seq, epoch, turn, kind, record FROM hl_journal WHERE session = 'dup' AND seq = 1;"
  if echo "$raw" | run_sql "$target" "$where" > /dev/null 2> "$dir/raw.err"; then
    echo "duplicate $target mutation=$mutation: VIOLATION: a second row with seq 1 was accepted"
    return 1
  fi
  grep -qF "$(duplicate_text "$target")" "$dir/raw.err" || fail "duplicate $target: unexpected error: $(cat "$dir/raw.err")"
  if commit_group "$target" "$mutation" session=dup epoch=1 head=0 "lease_ms=$long_lease_ms" "batch=@$dir/batch" |
    run_sql "$target" "$where" > /dev/null 2> "$dir/again.err"; then
    echo "duplicate $target mutation=$mutation: VIOLATION: a commit of seqs 1..3 again was accepted"
    return 1
  fi
  grep -qF "$(fenced_text "$target")" "$dir/again.err" || fail "duplicate $target: unexpected error: $(cat "$dir/again.err")"
  dump "$target" "$where" > "$dir/dump.tsv"
  "$hl" sql decode "$dir/dump.tsv" > /dev/null
  echo "duplicate $target: a row with an existing (session, seq) is refused, and so is a commit from a head already passed"
}

crash_hold_seconds=600
client_check_interval_ms=100

race_run() {
  local target=$1 mutation=$2 where=$3 run=$4 jitter_a=$5 jitter_hold=$6 jitter_b=$7 dir=$8
  local session=race-$run
  finished_turn "$dir/$run.batch1" 0 1 1
  finished_turn "$dir/$run.batch2" 3 2 1
  if ! {
    render "$target" "$mutation" acquire "session=$session" owner=a "lease_ms=$expired_lease_ms"
    commit_group "$target" "$mutation" "session=$session" epoch=1 head=0 "lease_ms=$expired_lease_ms" "batch=@$dir/$run.batch1"
  } | run_sql "$target" "$where" > /dev/null 2> "$dir/$run.setup.err"; then
    echo "$run setup-failed"
    return
  fi
  if [ "$(store "$target")" = postgres ]; then
    {
      echo "BEGIN;"
      render "$target" "$mutation" commit "session=$session" epoch=1 head=3 "lease_ms=$expired_lease_ms" "batch=@$dir/$run.batch2"
      echo "SELECT pg_sleep($(ms_seconds "$jitter_hold"));"
      echo "COMMIT;"
    } > "$dir/$run.a.sql"
  else
    {
      render "$target" "$mutation" connect
      commit_group "$target" "$mutation" "session=$session" epoch=1 head=3 "lease_ms=$expired_lease_ms" "batch=@$dir/$run.batch2"
    } > "$dir/$run.a.sql"
  fi
  {
    render "$target" "$mutation" connect
    render "$target" "$mutation" acquire "session=$session" owner=b "lease_ms=$expired_lease_ms"
  } > "$dir/$run.b.sql"
  (sleep "$(ms_seconds "$jitter_a")" && run_sql "$target" "$where" < "$dir/$run.a.sql" > "$dir/$run.a.out" 2> "$dir/$run.a.err") &
  local pid_a=$!
  (sleep "$(ms_seconds "$jitter_b")" && run_sql "$target" "$where" < "$dir/$run.b.sql" > "$dir/$run.b.out" 2> "$dir/$run.b.err") &
  local pid_b=$!
  local status_a=0 status_b=0
  wait "$pid_a" || status_a=$?
  wait "$pid_b" || status_b=$?
  local a=error
  if [ "$status_a" -eq 0 ]; then
    a=ok
  elif grep -qF "$(fenced_text "$target")" "$dir/$run.a.err"; then
    a=fenced
  fi
  local row
  row=$(tail -n 1 "$dir/$run.b.out")
  if [ "$status_b" -ne 0 ] || [ -z "$row" ]; then
    echo "$run a=$a b=acquire-failed"
    return
  fi
  local b_epoch=${row%|*} b_head=${row#*|}
  finished_turn "$dir/$run.batch3" "$b_head" 3 1
  local b_commit=ok
  commit_group "$target" "$mutation" "session=$session" "epoch=$b_epoch" "head=$b_head" "lease_ms=$expired_lease_ms" "batch=@$dir/$run.batch3" |
    run_sql "$target" "$where" > /dev/null 2> "$dir/$run.c.err" || b_commit=failed
  echo "$run a=$a b_epoch=$b_epoch b_head=$b_head b_commit=$b_commit"
}

race() {
  local target=$1 mutation=$2 runs=$3
  local dir=$scratch/race-$target-$mutation
  mkdir -p "$dir"
  local where
  where=$(fresh_store "$target" "race-$target-$mutation")
  schema "$target" "$mutation" "$where"
  local start end
  start=$(now_ns)
  for run in $(seq 1 "$runs"); do
    local jitter_a=$((RANDOM % race_jitter_ms)) jitter_hold=$((RANDOM % race_jitter_ms)) jitter_b=$((RANDOM % race_jitter_ms))
    race_run "$target" "$mutation" "$where" "$run" "$jitter_a" "$jitter_hold" "$jitter_b" "$dir" > "$dir/$run.result" &
    while [ "$(jobs -rp | wc -l)" -ge "$race_parallel" ]; do
      wait -n || true
    done
  done
  wait
  end=$(now_ns)
  cat "$dir"/*.result | sort -n > "$dir/results"
  local first_a first_b stale broken
  first_a=$(grep -c ' a=ok b_epoch=2 b_head=6 b_commit=ok$' "$dir/results" || true)
  first_b=$(grep -c ' a=fenced b_epoch=2 b_head=3 b_commit=ok$' "$dir/results" || true)
  stale=$(grep -c ' a=ok b_epoch=2 b_head=3 ' "$dir/results" || true)
  broken=$((runs - first_a - first_b - stale))
  dump "$target" "$where" > "$dir/dump.tsv"
  local decoded=ok
  "$hl" sql decode "$dir/dump.tsv" > /dev/null 2> "$dir/decode.err" || decoded=failed
  echo "race $target mutation=$mutation: $runs runs, $race_parallel at a time, in $(((end - start) / 1000000)) ms: the stale owner committed before the takeover in $first_a, was fenced after it in $first_b, committed after it in $stale; journal decode $decoded"
  if [ "$broken" -ne 0 ]; then
    grep -vE ' a=(ok b_epoch=2 b_head=6|fenced b_epoch=2 b_head=3) b_commit=ok$| a=ok b_epoch=2 b_head=3 ' "$dir/results" | head -n 3 | sed 's/^/  unexpected: run /'
    fail "race $target mutation=$mutation: $broken runs ended in neither ordering (see $dir)"
  fi
  if [ "$stale" -ne 0 ] || [ "$decoded" != ok ]; then
    grep ' a=ok b_epoch=2 b_head=3 ' "$dir/results" | head -n 3 | sed 's/^/  violation: the stale owner committed after the takeover: run /'
    [ "$decoded" = ok ] || sed 's/^/  decode: /' "$dir/decode.err"
    return 1
  fi
}

wait_gone() {
  local where=$1 app=$2
  for _ in $(seq 1 "$crash_polls"); do
    if [ -z "$(echo "SELECT pid FROM pg_stat_activity WHERE application_name = '$app';" | pg "$where" -At)" ]; then
      return 0
    fi
    sleep "$crash_poll_seconds"
  done
  fail "crash: the killed owner's backend '$app' never went away"
}

recovered() {
  local target=$1 mutation=$2 where=$3 session=$4 dir=$5
  local rows head
  rows=$(scalar "$target" "$where" "SELECT count(*) FROM hl_journal WHERE session = '$session'")
  head=$(scalar "$target" "$where" "SELECT head FROM hl_sessions WHERE session = '$session'")
  [ "$rows" = 3 ] && [ "$head" = 3 ] || fail "crash $target: after the kill the session holds $rows rows with head $head; the durable point before it had 3"
  finished_turn "$dir/after" 3 2 1
  {
    render "$target" "$mutation" acquire "session=$session" owner=a "lease_ms=$long_lease_ms"
    commit_group "$target" "$mutation" "session=$session" epoch=2 head=3 "lease_ms=$long_lease_ms" "batch=@$dir/after"
  } | run_sql "$target" "$where" > /dev/null || fail "crash $target: the session cannot commit after recovery"
  dump "$target" "$where" > "$dir/dump.tsv"
  "$hl" sql decode "$dir/dump.tsv" > /dev/null
}

crash_setup() {
  local target=$1 mutation=$2 where=$3 session=$4 dir=$5
  finished_turn "$dir/batch1" 0 1 1
  finished_turn "$dir/big" 3 2 "$crash_turns"
  {
    render "$target" "$mutation" acquire "session=$session" owner=a "lease_ms=$long_lease_ms"
    commit_group "$target" "$mutation" "session=$session" epoch=1 head=0 "lease_ms=$long_lease_ms" "batch=@$dir/batch1"
  } | run_sql "$target" "$where" > /dev/null
}

crash_postgres() {
  local mutation=$1 run=$2 mode=$3 where=$4
  local target=server-postgres session=crash-$run app=hl-crash-$run dir=$scratch/crash-postgres/$run
  mkdir -p "$dir"
  crash_setup "$target" "$mutation" "$where" "$session" "$dir"
  {
    echo "SET client_connection_check_interval = '${client_check_interval_ms}ms';"
    echo "BEGIN;"
    render "$target" "$mutation" commit "session=$session" epoch=1 head=3 "lease_ms=$long_lease_ms" "batch=@$dir/big"
    echo "SELECT pg_sleep($crash_hold_seconds);"
    echo "COMMIT;"
  } > "$dir/crash.sql"
  (
    export PGAPPNAME=$app
    exec psql -h 127.0.0.1 -p "$pg_port" -U postgres -X -q -v ON_ERROR_STOP=1 -d "$where" -f "$dir/crash.sql"
  ) > "$dir/out" 2> "$dir/err" &
  local pid=$! backend=""
  for _ in $(seq 1 "$crash_polls"); do
    backend=$(echo "SELECT pid FROM pg_stat_activity WHERE application_name = '$app' AND state = 'active' AND query LIKE 'SELECT pg_sleep%';" | pg "$where" -At)
    [ -n "$backend" ] && break
    sleep "$crash_poll_seconds"
  done
  [ -n "$backend" ] || fail "crash postgres: the owner never reached its open transaction"
  local visible start
  visible=$(scalar "$target" "$where" "SELECT count(*) FROM hl_journal WHERE session = '$session'")
  start=$(now_ns)
  [ "$(cat "/proc/$pid/comm")" = psql ] || fail "crash postgres: pid $pid is $(cat "/proc/$pid/comm"), not the owner's psql"
  if [ "$mode" = client ]; then
    kill -9 "$pid"
    { wait "$pid"; } 2> /dev/null || true
    wait_gone "$where" "$app"
  else
    docker exec "$pg_container" kill -9 "$backend"
    { wait "$pid"; } 2> /dev/null || true
    wait_postgres
  fi
  local end
  end=$(now_ns)
  recovered "$target" "$mutation" "$where" "$session" "$dir"
  echo "crash server-postgres run $run: kill -9 of the $mode (pid $([ "$mode" = client ] && echo "$pid" || echo "$backend")) holding $((crash_turns * 3)) uncommitted rows ($(stat -c %s "$dir/big") batch bytes, $visible rows visible meanwhile); after $(((end - start) / 1000000)) ms the session holds its 3 durable rows and commits again"
}

crash_sqlite() {
  local target=$1 mutation=$2 run=$3
  local session=crash-$run dir=$scratch/crash-$target/$run
  mkdir -p "$dir"
  local where
  where=$(fresh_store "$target" "crash-$target-$run")
  schema "$target" "$mutation" "$where"
  crash_setup "$target" "$mutation" "$where" "$session" "$dir"
  local before
  before=$(stat -c %s "$where")
  mkfifo "$dir/in"
  sqlite3 -bail "$where" < "$dir/in" > "$dir/out" 2> "$dir/err" &
  local pid=$! feed
  exec {feed}> "$dir/in"
  {
    echo ".timeout $busy_timeout_ms"
    render "$target" "$mutation" connect
    echo "PRAGMA cache_size = $crash_cache_pages;"
    if [ "$target" = native-sqlite ]; then echo "BEGIN IMMEDIATE;"; else echo "BEGIN;"; fi
    render "$target" "$mutation" --bare commit "session=$session" epoch=1 head=3 "lease_ms=$long_lease_ms" "batch=@$dir/big"
    echo ".shell touch $dir/ready"
  } >&"$feed"
  for _ in $(seq 1 "$crash_polls"); do
    [ -e "$dir/ready" ] && break
    sleep "$crash_poll_seconds"
  done
  [ -e "$dir/ready" ] || fail "crash $target: the owner never reached its open transaction"
  local spilled=0 file
  for file in "$where" "$where-wal" "$where-journal"; do
    [ -e "$file" ] && spilled=$((spilled + $(stat -c %s "$file")))
  done
  spilled=$((spilled - before))
  [ "$(cat "/proc/$pid/comm")" = sqlite3 ] || fail "crash $target: pid $pid is $(cat "/proc/$pid/comm"), not the owner's sqlite3"
  kill -9 "$pid"
  exec {feed}>&-
  { wait "$pid"; } 2> /dev/null || true
  local integrity
  integrity=$(sqlite3 "$where" 'PRAGMA integrity_check')
  [ "$integrity" = ok ] || fail "crash $target: integrity_check after the kill: $integrity"
  recovered "$target" "$mutation" "$where" "$session" "$dir"
  echo "crash $target run $run: kill -9 of sqlite3 (pid $pid) inside a transaction of $((crash_turns * 3)) rows that had already spilled $spilled bytes to disk; the reopened database passes integrity_check, holds its 3 durable rows and commits again"
}

start_postgres
sql_targets="native-sqlite server-postgres cloudflare"
for target in $sql_targets; do
  replay "$target" none
  lease "$target" none
  duplicate "$target" none
done
for target in native-sqlite server-postgres; do
  race "$target" none "$race_runs" || fail "race on $target"
done
crash_where=$(fresh_store server-postgres crash-postgres)
schema server-postgres none "$crash_where"
for run in $(seq 1 "$crash_runs"); do
  if [ $((run % 2)) -eq 1 ]; then mode=client; else mode=server; fi
  crash_postgres none "$run" "$mode" "$crash_where"
  crash_sqlite native-sqlite none "$run"
  crash_sqlite cloudflare none "$run"
done
for target in native-sqlite server-postgres; do
  if race "$target" no_fence_predicate "$mutation_race_runs"; then
    fail "mutation no_fence_predicate was not caught on $target"
  fi
  echo "mutation no_fence_predicate caught on $target"
done
for target in $sql_targets; do
  if duplicate "$target" seq_not_unique; then
    fail "mutation seq_not_unique was not caught on $target"
  fi
  echo "mutation seq_not_unique caught on $target"
done
echo "sql-drill: ok"
