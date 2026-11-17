#!/usr/bin/env bash
set -euo pipefail
set -m
cd "$(dirname "$0")/.."

seeds="${SEEDS:-0..2000}"
twice_seeds="${TWICE_SEEDS:-0..300}"
wasm_diff_seeds="${WASM_DIFF_SEEDS:-0..100}"
wasm_native_diff_seeds="${WASM_NATIVE_DIFF_SEEDS:-0..30}"
wasm_platform_diff_seeds="${WASM_PLATFORM_DIFF_SEEDS:-0..30}"
if [ "${HL_LONG:-0}" = 1 ]; then
  wasm_diff_seeds="${WASM_DIFF_SEEDS:-0..2000}"
  wasm_native_diff_seeds="${WASM_NATIVE_DIFF_SEEDS:-0..2000}"
  wasm_platform_diff_seeds="${WASM_PLATFORM_DIFF_SEEDS:-0..300}"
fi
jobs="${CHECK_JOBS:-$(nproc)}"
targets="native-jsonl native-sqlite server-postgres cloudflare vercel"
native_targets="native-jsonl native-sqlite server-postgres"
platform_targets="cloudflare vercel"
platform_pending="
"
examples=$(echo examples/*.hl)
hl=target/release/hl
scratch=output/check
logs=$scratch/jobs
mkdir -p "$scratch"
rm -rf "$logs"
mkdir -p "$logs"

stage_order=()
declare -A stage_jobs=() stage_first=() stage_last=() stage_busy=()
job_stages=()
job_commands=()
declare -A running_index=() running_started=()
check_started=$(date +%s%N)

milliseconds() {
  echo $(( ($(date +%s%N) - check_started) / 1000000 ))
}

seconds() {
  printf '%d.%d' $(( $1 / 1000 )) $(( $1 % 1000 / 100 ))
}

timings() {
  local stage
  echo "check.sh: stage timings (wall: first job start to last job end; busy: the jobs' wall times added up; $jobs jobs at once)"
  for stage in "${stage_order[@]}"; do
    [ -n "${stage_last[$stage]:-}" ] || continue
    printf '  %-16s %3d job(s) %8s s wall %8s s busy\n' "$stage" "${stage_jobs[$stage]}" \
      "$(seconds $(( stage_last[$stage] - stage_first[$stage] )))" "$(seconds "${stage_busy[$stage]}")"
  done
  echo "  total $(seconds "$(milliseconds)") s"
}

stop_running() {
  local pid
  for pid in "${!running_index[@]}"; do
    kill -TERM -- "-$pid" 2> /dev/null || true
  done
  for pid in "${!running_index[@]}"; do
    wait "$pid" 2> /dev/null || true
  done
  running_index=()
}
trap stop_running EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

queue() {
  local stage=$1
  shift
  if [ -z "${stage_jobs[$stage]:-}" ]; then
    stage_order+=("$stage")
    stage_jobs[$stage]=0
    stage_busy[$stage]=0
  fi
  stage_jobs[$stage]=$(( stage_jobs[$stage] + 1 ))
  job_stages+=("$stage")
  job_commands+=("$*")
}

log_of() {
  echo "$logs/$1-${job_stages[$1]}.log"
}

start_job() {
  local index=$1 words stage
  read -ra words <<< "${job_commands[$index]}"
  ( set +m; "${words[@]}" ) > "$(log_of "$index")" 2>&1 &
  running_index[$!]=$index
  running_started[$!]=$(milliseconds)
  stage=${job_stages[$index]}
  [ -n "${stage_first[$stage]:-}" ] || stage_first[$stage]=${running_started[$!]}
}

finish_job() {
  local pid=$1 status=$2 index stage started ended
  index=${running_index[$pid]}
  stage=${job_stages[$index]}
  started=${running_started[$pid]}
  ended=$(milliseconds)
  unset "running_index[$pid]" "running_started[$pid]"
  stage_last[$stage]=$ended
  stage_busy[$stage]=$(( stage_busy[$stage] + ended - started ))
  if [ "$status" -ne 0 ]; then
    stop_running
    cat "$(log_of "$index")" >&2
    echo "check.sh: FAILED in stage $stage: ${job_commands[$index]} exited $status after $(seconds $(( ended - started ))) s (log: $(log_of "$index"))" >&2
    timings >&2
    exit 1
  fi
  cat "$(log_of "$index")"
}

run_queued() {
  local next=0 pid status
  while [ "$next" -lt "${#job_commands[@]}" ] || [ "${#running_index[@]}" -gt 0 ]; do
    while [ "${#running_index[@]}" -lt "$jobs" ] && [ "$next" -lt "${#job_commands[@]}" ]; do
      start_job "$next"
      next=$((next + 1))
    done
    if wait -n -p pid; then
      status=0
    else
      status=$?
    fi
    finish_job "$pid" "$status"
  done
  job_stages=()
  job_commands=()
}

stage() {
  queue "$@"
  run_queued
}

build() {
  cargo build --release --locked
}

rules() {
  local packages
  packages=$(grep -c '^name = ' Cargo.lock)
  if [ "$packages" -ne 1 ] || ! grep -q '^name = "harnesslang"' Cargo.lock; then
    echo "Cargo.lock must list harnesslang alone" >&2
    exit 1
  fi
  if grep -rnE '^\s*//|#\[test\]|#\[cfg\(test\)\]' src; then
    echo "src has comments or test functions" >&2
    exit 1
  fi
  echo "rules: Cargo.lock lists harnesslang alone, and src has no comments or test functions"
}

examples_pass() {
  local file target
  for file in $examples; do
    "$hl" check "$file" > /dev/null
    for target in $targets; do
      "$hl" check --target "$target" "$file" > /dev/null
    done
  done
  echo "check: every example passes on every target"
}

expect_error() {
  local file=$1 target=$2 expected=$3
  local args=(check)
  [ "$target" != any ] && args+=(--target "$target")
  if "$hl" "${args[@]}" "$file" > /dev/null 2> "$scratch/stderr"; then
    echo "$file on $target: expected an error, the check passed" >&2
    exit 1
  fi
  if ! grep -qF -- "$expected" "$scratch/stderr"; then
    echo "$file on $target: expected error text: $expected" >&2
    cat "$scratch/stderr" >&2
    exit 1
  fi
}

invalid_fail() {
  expect_error examples/invalid/missing_unknown.hl any "missing_unknown.hl:21:11: error: match on maybe<text> is missing 'unknown'"
  expect_error examples/invalid/unchecked_external.hl any "unchecked_external.hl:10:3: error: match on maybe<text> is missing 'unknown'"
  expect_error examples/invalid/unbounded_list.hl any "unbounded_list.hl:8:22: error: a list needs a capacity"
  expect_error examples/invalid/literal_bound.hl any "literal_bound.hl:13:19: error: the bound of loop 'step' must be a named constant, found number 64"
  expect_error examples/invalid/unknown_name.hl any "unknown_name.hl:13:21: error: unknown name 'inpt'"
  expect_error examples/invalid/code_no_model.hl any "code_no_model.hl:14:6: error: code 'exec' needs a model declaration"
  expect_error examples/invalid/code_unknown_tool.hl any "code_unknown_tool.hl:17:15: error: code 'exec' lists 'bash', which is not a declared tool"
  expect_error examples/invalid/code_nested_code.hl any "code_nested_code.hl:27:15: error: code 'outer' lists code tool 'exec'"
  expect_error examples/invalid/code_literal_bound.hl any "code_literal_bound.hl:18:9: error: field 'calls' of code 'exec' must be a named constant, found number 8"
  expect_error examples/invalid/code_parallel_over_calls.hl any "code_parallel_over_calls.hl:18:6: error: code 'exec': parallel (cell_wide = 16) exceeds calls (cell_calls_max = 8)"
  expect_error examples/invalid/code_parallel_for.hl any "code_parallel_for.hl:39:7: error: a parallel for dispatches tool calls at once, but code 'exec' makes a dispatch able to run a cell"
  expect_error examples/invalid/code_missing_field.hl any "code_missing_field.hl:16:6: error: code 'exec' lacks 'timers'"
  expect_error examples/invalid/code_no_tools.hl any "code_no_tools.hl:16:6: error: code 'exec' lists no tools"
  expect_error examples/invalid/code_unknown_field.hl any "code_unknown_field.hl:24:3: error: code 'exec' has no field 'memory'"
  expect_error examples/invalid/enum_missing_value.hl any 'enum_missing_value.hl:6:3: error: match on status is missing "interrupted"'
  expect_error examples/invalid/enum_unknown_value.hl any 'enum_unknown_value.hl:10:10: error: "cancelled" is not a value of status'
  expect_error examples/invalid/reply_two_call_lists.hl any "reply_two_call_lists.hl:7:73: error: reply type 'split_reply' carries list<call, calls_max> in 'reads' and 'writes': exactly one variant carries the model's calls"
  expect_error examples/invalid/reply_no_call_list.hl any "reply_no_call_list.hl:6:6: error: reply type 'text_reply' has no variant carrying list<call, calls_max>"
  expect_error examples/invalid/reply_missing_variant.hl any "reply_missing_variant.hl:21:7: error: match on turn_reply is missing 'paused'"
  expect_error examples/invalid/reply_unmatched_failure.hl any "reply_unmatched_failure.hl:20:5: error: 'answer' is not a variant of known<turn_reply>"
  expect_error examples/invalid/over_cloudflare.hl cloudflare "handler 'prompt': worst case 17408 outbound requests exceeds cloudflare's limit of 10000 outbound requests per invocation; loops: step below turn_steps_max (1024) x call below calls_per_step_max (16)"
  expect_error examples/invalid/fn_recursion.hl any "fn_recursion.hl:33:3: error: fn 'ask' is recursive: ask -> run_tools -> ask"
  expect_error examples/invalid/fn_arity.hl any "fn_arity.hl:16:3: error: fn 'note' takes 2 argument(s), found 1"
  expect_error examples/invalid/fn_hidden_name.hl any "fn_hidden_name.hl:10:14: error: fn 'remember' cannot see 'input'"
  expect_error examples/invalid/event_unknown.hl any "event_unknown.hl:3:4: error: unknown event 'pause': a harness takes 'cancel', 'steer', 'job' and 'resume'"
  expect_error examples/invalid/event_take_undeclared.hl any "event_take_undeclared.hl:6:16: error: 'take steers' needs 'on steer(message: text) bound N' on the harness"
  expect_error examples/invalid/listing_tool_name.hl any "listing_tool_name.hl:5:6: error: tool 'docs__search' is named like a tool of listing 'docs'"
  expect_error examples/invalid/listing_unknown.hl any "listing_unknown.hl:11:5: error: 'unknown' is not a variant of known<u64>"
  expect_error examples/invalid/suspend_undeclared.hl any "suspend_undeclared.hl:6:16: error: 'suspend' needs 'on resume' on the harness"
  expect_error examples/invalid/wait_literal.hl any "wait_literal.hl:6:19: error: the duration of 'wait' must be a named constant in milliseconds"
  expect_error examples/invalid/long_wait.hl cloudflare "handler 'nap': wait(hour_ms) of 3600000 milliseconds exceeds cloudflare's limit of 900000 milliseconds per wait"
  expect_error examples/invalid/wait_uncapped.hl any "wait_bound: wait(after_ms) may sleep past its cap retry_cap_ms and trap with bound(retry_cap_ms)"
  expect_error examples/invalid/with_outside_model.hl any "with_outside_model.hl:13:23: error: 'with' adds an entry to the context a model call reads: it is only a model call's argument"
  expect_error examples/invalid/long_wait.hl vercel "handler 'nap': wait(hour_ms) of 3600000 milliseconds exceeds vercel's limit of 800000 milliseconds per wait"
  expect_error examples/invalid/family_sessions.hl any "family_sessions.hl:7:37: error: spawn tool 'planner' starts up to 3 sessions per call (planner.plan_change and 2 descendant sessions, part below parts_max (2)), over sessions at most two_sessions (2)"
  expect_error examples/invalid/blob_row.hl cloudflare "tool 'photo': a result record of blob<photo_bytes_max> (2097152 bytes) holds up to 2796399 bytes, over cloudflare's limit of 2000000 bytes per journal row"
  local target
  for target in native-jsonl native-sqlite server-postgres vercel; do
    "$hl" check --target "$target" examples/invalid/over_cloudflare.hl > /dev/null
  done
  for target in native-jsonl native-sqlite server-postgres vercel; do
    "$hl" check --target "$target" examples/invalid/blob_row.hl > /dev/null
  done
  for target in native-jsonl native-sqlite server-postgres cloudflare vercel; do
    "$hl" check --target "$target" examples/invalid/over_wait.hl > /dev/null
  done
  for target in native-jsonl native-sqlite server-postgres; do
    "$hl" check --target "$target" examples/invalid/long_wait.hl > /dev/null
  done
  echo "check: every invalid example fails with its expected error"
}

same_program() {
  local with_fn=$1 written_out=$2 target
  for target in $targets; do
    "$hl" emit --format wasm --target "$target" "$written_out" > "$scratch/written_out.wasm"
    if ! "$hl" emit --format wasm --target "$target" "$with_fn" | cmp -s - "$scratch/written_out.wasm"; then
      echo "fn: $with_fn and $written_out compile to different wasm for $target" >&2
      return 1
    fi
  done
  "$hl" check "$written_out" | sed "s#$written_out#FILE#" > "$scratch/written_out.check"
  if ! "$hl" check "$with_fn" | sed "s#$with_fn#FILE#" | cmp -s - "$scratch/written_out.check"; then
    echo "fn: $with_fn and $written_out check and prove differently" >&2
    return 1
  fi
}

fn_equal() {
  same_program examples/subagents.hl examples/inlined/subagents.hl
  same_program docs/snippets/fn.hl docs/snippets/fn_inline.hl
  same_program examples/code_tasks.hl examples/inlined/code_tasks.hl
  sed -e '/push context user("Summarize/{N;s/\(.*\)\n\(.*\)/\2\n\1/}' -e 's#"../subagent_child.hl"#"../../examples/subagent_child.hl"#' examples/inlined/subagents.hl > "$scratch/planted.hl"
  "$hl" check "$scratch/planted.hl" > /dev/null
  if same_program examples/subagents.hl "$scratch/planted.hl" 2> /dev/null; then
    echo "fn: a written-out program with two pushes swapped still compares equal" >&2
    exit 1
  fi
  echo "check: every fn program compiles, checks and proves exactly as its written-out copy"
}

sim_one() {
  local target=$1 file=$2
  "$hl" sim --target "$target" --seeds "$seeds" "$file" | sed "s#^#sim file=$file target=$target: #"
}

sim_twice() {
  "$hl" sim --target "$1" --seeds "$twice_seeds" --twice examples/agent.hl
}

wasm_diff_one() {
  WASM_DIFF_SEEDS=$wasm_diff_seeds WASM_DIFF_TARGETS=$1 WASM_DIFF_FILES=$2 scripts/wasm-diff.sh
}

wasm_native_diff_one() {
  WASM_DIFF_HOST=native WASM_DIFF_SEEDS=$wasm_native_diff_seeds WASM_DIFF_TARGETS=$1 WASM_DIFF_FILES=$2 scripts/wasm-diff.sh
}

wasm_platform_diff_one() {
  WASM_DIFF_HOST=platform WASM_DIFF_SEEDS=$wasm_platform_diff_seeds WASM_DIFF_TARGETS=$1 WASM_DIFF_FILES=$2 scripts/wasm-diff.sh
}

platform_ready() {
  ! grep -qw "$1:$2" <<< "$platform_pending"
}

wasm_lowered() {
  "$hl" emit --target "$1" --format wasm "$2" > /dev/null 2>&1
}

scoped_diff_one() {
  local file=docs/snippets/tree_planner.hl scope=1.1
  WASM_DIFF_SEEDS=$wasm_diff_seeds WASM_DIFF_TARGETS=$1 WASM_DIFF_FILES=$file WASM_DIFF_SCOPE=$scope scripts/wasm-diff.sh
  case $1 in
    cloudflare | vercel) ;;
    *) WASM_DIFF_HOST=native WASM_DIFF_SEEDS=$wasm_native_diff_seeds WASM_DIFF_TARGETS=$1 WASM_DIFF_FILES=$file WASM_DIFF_SCOPE=$scope scripts/wasm-diff.sh ;;
  esac
}

mutation_one() {
  MUTATIONS=$1 scripts/mutations.sh
}

census_examples() {
  local file name target missing=0
  cat "$logs"/*.log > "$scratch/sweeps.log"
  for file in $examples; do
    name=$(basename "$file" .hl)
    for target in $targets; do
      if ! grep -q "^sim file=$file target=$target: ok target=$target mutation=none seeds=$seeds " "$scratch/sweeps.log"; then
        echo "census: $file was not swept by hl sim on $target" >&2
        missing=1
      fi
    done
    for target in $native_targets; do
      if ! grep -q "^ok backend=wasm host=native file=$file target=$target " "$scratch/sweeps.log"; then
        echo "census: $file had no differential run on the native wasm host on $target" >&2
        missing=1
      fi
    done
    for target in $platform_targets; do
      platform_ready "$target" "$file" || continue
      if ! grep -q "^ok backend=wasm host=platform file=$file target=$target " "$scratch/sweeps.log"; then
        echo "census: $file had no differential run on the fixed $target host" >&2
        missing=1
      fi
    done
  done
  local pending=0
  for file in $examples; do
    for target in $targets; do
      grep -q "^ok backend=wasm host=node file=$file target=$target " "$scratch/sweeps.log" && continue
      if wasm_lowered "$target" "$file"; then
        echo "census: $file is lowered to wasm for $target but had no wasm differential run" >&2
        missing=1
      elif ! "$hl" emit --target "$target" --format wasm "$file" 2>&1 | grep -q 'not yet in wasm'; then
        echo "census: $file fails to lower to wasm for $target for a reason other than a feature not yet lowered" >&2
        missing=1
      else
        pending=$((pending + 1))
      fi
    done
  done
  [ "$missing" -eq 0 ] || exit 1
  echo "census: all $(wc -w <<< "$examples") examples are swept by hl sim on every target and by the differential runs on the native wasm host on every native target and on the fixed Cloudflare and Vercel hosts, except $platform_pending"
  echo "census: every example lowered to wasm had a wasm differential run on every target; $pending example-target pairs use a feature not yet lowered"
}

drill_mutation_caught() {
  local log=$1 pattern=$2 what=$3
  shift 3
  if "$@" > "$log" 2>&1; then
    echo "drill: $what survived" >&2
    exit 1
  fi
  if ! grep -qE "$pattern" "$log"; then
    echo "drill: $what failed for another reason:" >&2
    cat "$log" >&2
    exit 1
  fi
}

drill() {
  local dispatched='external [0-9]+/[0-9]+ ran (before its call record was durable|as its recorded tool)'
  local fenced='fence: external [0-9]+/[0-9]+ dispatched by run a\.|taken_over: only a stale owner writes the marker'
  local target
  DRILL_SESSIONS=4 scripts/rust-crash-drill.sh
  drill_mutation_caught "$scratch/drill-mutation.log" "$dispatched" "flush_after_dispatch in the crash drill" \
    env DRILL_SESSIONS=8 DRILL_MUTATION=flush_after_dispatch scripts/rust-crash-drill.sh
  echo "drill: flush_after_dispatch is caught on the real files"
  SQL_DRILL_SESSIONS=2 scripts/rust-sql-drill.sh
  for target in native-sqlite server-postgres; do
    drill_mutation_caught "$scratch/sql-drill-flush_after_dispatch-$target.log" "$dispatched" "flush_after_dispatch in the SQL crash drill on $target" \
      env SQL_DRILL_TARGETS=$target SQL_DRILL_MODE=crash SQL_DRILL_MUTATION=flush_after_dispatch scripts/rust-sql-drill.sh
    drill_mutation_caught "$scratch/sql-drill-no_fence-$target.log" "$fenced" "no_fence in the takeover drill on $target" \
      env SQL_DRILL_TARGETS=$target SQL_DRILL_MODE=takeover SQL_DRILL_MUTATION=no_fence scripts/rust-sql-drill.sh
  done
  echo "drill: flush_after_dispatch and no_fence are caught in the real SQLite and Postgres databases"
}

agent() {
  if grep -rnE '^\s*//|#\[test\]|#\[cfg\(test\)\]' hosts/native/src hosts/native/build.rs scripts/fixtures/src; then
    echo "hosts/native or scripts/fixtures has comments or test functions" >&2
    exit 1
  fi
  rustfmt --edition 2024 --check hosts/native/build.rs hosts/native/src/*.rs scripts/fixtures/src/*.rs scripts/fixtures/src/bin/*.rs
  scripts/fixtures/host-probe.sh
  scripts/fixtures/wire-probe.sh
  AGENT_DRILL_SEED=1 scripts/agent-drill.sh
  if AGENT_DRILL_SEED=1 AGENT_DRILL_PLANT=batch_flush_after_dispatch scripts/agent-drill.sh > "$scratch/agent-drill-plant.log" 2>&1; then
    echo "agent: batch_flush_after_dispatch survived the hl-host kill -9 drill" >&2
    exit 1
  fi
  if ! grep -q 'external calls dispatched more than once' "$scratch/agent-drill-plant.log"; then
    echo "agent: the planted drill failed for another reason:" >&2
    cat "$scratch/agent-drill-plant.log" >&2
    exit 1
  fi
  CODEX_DRILL_SEED=1 scripts/codex-drill.sh
  if CODEX_DRILL_SEED=1 CODEX_DRILL_PLANT=batch_flush_after_dispatch scripts/codex-drill.sh > "$scratch/codex-drill-plant.log" 2>&1; then
    echo "agent: batch_flush_after_dispatch survived the codex_agent.hl kill -9 drill" >&2
    exit 1
  fi
  if ! grep -q 'external calls dispatched more than once' "$scratch/codex-drill-plant.log"; then
    echo "agent: the planted codex drill failed for another reason:" >&2
    cat "$scratch/codex-drill-plant.log" >&2
    exit 1
  fi
  echo "agent: hl-host survives kill -9 against the local mock without repeating an external call, and batch_flush_after_dispatch is caught"
  echo "agent: codex_agent.hl on hl-host survives kill -9 against the fake Responses API, steers and cancels, and batch_flush_after_dispatch is caught"
}

stage build build
stage census scripts/mutations.sh --census
stage selfhost-prepare scripts/selfhost-encode.sh --prepare
queue rules rules
queue examples examples_pass
queue invalid invalid_fail
queue fn fn_equal
queue docs scripts/docs-check.sh
queue strict scripts/strict.sh
for file in $examples docs/snippets/compaction.hl docs/snippets/recovery.hl docs/snippets/catalog.hl docs/snippets/images.hl docs/snippets/tree.hl docs/snippets/side.hl docs/snippets/keep.hl docs/snippets/in_flight.hl docs/snippets/fork.hl; do
  for target in $targets; do
    queue sim sim_one "$target" "$file"
  done
done
for target in $targets; do
  queue sim-twice sim_twice "$target"
done
for target in $targets; do
  queue scoped-diff scoped_diff_one "$target"
done
for file in $examples docs/snippets/*.hl; do
  for target in $targets; do
    wasm_lowered "$target" "$file" && queue wasm-diff wasm_diff_one "$target" "$file"
    wasm_lowered "$target" "$file" && queue wasm-diff-native wasm_native_diff_one "$target" "$file"
  done
  for target in $platform_targets; do
    platform_ready "$target" "$file" && wasm_lowered "$target" "$file" && queue wasm-diff-platform wasm_platform_diff_one "$target" "$file"
  done
done
for file in $examples docs/snippets/*.hl; do
  queue selfhost-encode scripts/selfhost-encode.sh "$file"
done
for mutation in $(scripts/mutations.sh --plans); do
  queue mutations mutation_one "$mutation"
done
for plan in $(scripts/mutations.sh --diff-plans); do
  queue diff-plants scripts/mutations.sh --diff "$plan"
done
queue deploy scripts/deploy-drill.sh
queue host-mutations scripts/host-mutations.sh
for plan in $(scripts/mutations.sh --deploy-plans); do
  queue deploy-plants scripts/mutations.sh --deploy "$plan"
done
queue deploy-real scripts/deploy-real.sh
queue exhaustive scripts/exhaustive.sh
queue fuzz-regress scripts/fuzz-regress.sh
queue prove scripts/prove-mutations.sh
queue agent agent
run_queued
stage census-examples census_examples
if [ "${HL_ORACLE:-0}" = 1 ]; then
  stage oracle scripts/oracle.sh
fi
if [ "${HL_DRILL:-0}" = 1 ]; then
  stage drill drill
fi
if [ "${HL_CLOUDFLARE:-0}" = 1 ]; then
  stage cloudflare scripts/cloudflare-drill.sh
fi
timings
echo "check.sh: ok"
