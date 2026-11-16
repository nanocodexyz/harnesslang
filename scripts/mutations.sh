#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

seeds="${MUTATION_SEEDS:-0..200}"
hl=target/release/hl
targets="native-jsonl native-sqlite server-postgres cloudflare vercel"
plans="flush_after_dispatch examples/agent.hl --twice external_once
recovery_redispatches_external examples/agent.hl --twice external_once
vercel_retry_redispatches examples/agent.hl --twice external_once
postgres_no_fence examples/agent.hl --twice external_once
jsonl_no_checksum examples/agent.hl --twice liveness
finish_before_flush examples/agent.hl --twice finish_once
idempotent_reported_unknown examples/agent.hl --twice idempotent_known
gate_releases_early examples/agent.hl --twice external_once
recovery_reports_failed examples/agent.hl --twice unknown_reported
rerun_rewrites_call examples/agent.hl --twice seq_unique
checkpoint_hash_order examples/agent.hl --twice replay_equal
recovery_skipped examples/agent.hl --twice panic
enum_unchecked examples/hooks.hl --twice enum_valid
model_failure_unrecorded examples/claude_agent.hl --twice replay_equal|seq_unique
batch_flush_after_dispatch examples/claude_agent.hl --twice external_once|unknown_reported|panic
results_in_completion_order examples/claude_agent.hl --twice sequential_equal
batch_pushes_reversed examples/claude_agent.hl --twice checkpoint_compacted
listing_unchecked examples/codex_agent.hl --twice tool_admitted
listed_class_dropped examples/codex_agent.hl --twice external_once|unknown_reported
input_unchecked examples/codex_agent.hl --twice tool_admitted
retire_skipped docs/snippets/compaction.hl --twice checkpoint_compacted
seal_skipped docs/snippets/compaction.hl --twice checkpoint_compacted
event_redelivered examples/claude_agent.hl --twice event_once
event_ack_early examples/claude_agent.hl --twice event_kept
event_read_unstamped examples/claude_agent.hl --twice event_before_read
event_drain_on_replay examples/claude_agent.hl --twice event_replay_equal
batch_drains_replayed examples/claude_agent.hl --twice event_replay_equal
loop_past_bound examples/sound/full_loop.hl --strict worst_case
cell_header_rides_past_external examples/code_agent.hl --twice nested_external_once 0..300
nested_flush_after_dispatch examples/code_agent.hl --twice nested_external_once 0..300
cell_recovery_redispatches_external examples/code_agent.hl --twice nested_external_once 0..300
cell_recovery_reports_failed examples/code_agent.hl --twice unknown_nested_reported 0..300
cell_replay_ignores_journal examples/code_agent.hl --twice nested_external_once|seq_unique 0..300
cell_rerun_after_finish examples/code_agent.hl --twice cell_finishes_once 0..300
timeout_rides examples/code_agent.hl --twice header_before_wait 0..300
divergence_dispatches examples/code_agent.hl --twice cell_replay_equal 0..300
nested_ids_restart examples/code_agent.hl --twice seq_unique|unknown_nested_reported 0..300
cell_limit_unchecked examples/code_agent.hl --twice cell_bounded 0..300
vercel_nested_retry_redispatches examples/code_agent.hl --twice nested_external_once 0..300
cell_gate_releases_early examples/code_agent.hl --twice nested_external_once 0..300
postgres_cell_unfenced examples/code_agent.hl --twice seq_unique 0..300
nested_idempotent_reported_unknown examples/code_agent.hl --twice nested_known 0..300
nested_enum_unchecked examples/code_tasks.hl --twice nested_enum_valid 0..300
default_unfilled docs/snippets/defaults.hl --twice tool_admitted
mode_dropped docs/snippets/modes.hl --twice mode_sent
settings_dropped docs/snippets/settings.hl --twice settings_sent
rewrite_live_only docs/snippets/rewrite.hl --twice checkpoint_compacted
lane_seq_shared docs/snippets/lanes.hl --twice liveness
resume_replays_suspend examples/hooks.hl --twice seq_unique
suspend_before_flush examples/hooks.hl --twice suspend_durable
resume_unmatched examples/hooks.hl --twice resume_matched
wait_unjournaled docs/snippets/park.hl --twice idempotent_known|liveness
wait_rerun_skips docs/snippets/park.hl --twice wait_until
wait_oversleeps docs/snippets/park.hl --twice wait_until
deny_dispatches docs/snippets/hooks.hl --twice denied_unrun
spawn_key_unscoped docs/snippets/spawn.hl --twice child_scoped
truncate_uncut docs/snippets/recovery.hl --twice checkpoint_compacted
usage_unchecked docs/snippets/recovery.hl --twice replay_equal
retry_after_unjournaled docs/snippets/recovery.hl --twice mode_sent
notice_dropped docs/snippets/catalog.hl --twice checkpoint_compacted
search_unchecked docs/snippets/catalog.hl --twice search_listed
discovery_unchecked docs/snippets/catalog.hl --twice tool_admitted
blob_unjournaled docs/snippets/images.hl --twice blob_journaled
blobs_unevicted docs/snippets/images.hl --twice checkpoint_compacted
grandchild_unscoped docs/snippets/tree.hl --twice child_scoped
family_one_level docs/snippets/tree.hl --twice family_bounded
side_live_state docs/snippets/side.hl --twice side_isolated
side_journaled docs/snippets/side.hl --twice side_isolated
idle_early docs/snippets/keep.hl --twice idle_due
interrupt_ignored docs/snippets/in_flight.hl --twice interrupt_honored
interrupted_undecoded docs/snippets/in_flight.hl --twice interrupt_decoded
interrupt_reruns docs/snippets/in_flight.hl --twice external_once
recover_trusts_lost docs/snippets/in_flight.hl --twice external_once
job_lost_dropped docs/snippets/in_flight.hl --twice job_reported 0..1000
cap_unchecked docs/snippets/payments.hl --twice spend_within_cap|payment_presented
spent_unaccounted docs/snippets/payments.hl --twice spend_accounted
credential_dropped docs/snippets/payments.hl --twice payment_presented
job_lost_all docs/snippets/in_flight.hl --twice job_once
fork_live_state docs/snippets/fork.hl --twice fork_state_equal
fork_hook_skipped docs/snippets/fork.hl --twice fork_state_equal
cell_leaks docs/snippets/code_wait.hl --twice cell_settled_at_turn_end
cell_cancel_ignored docs/snippets/code_stop.hl --twice interrupt_honored
climb_accept_any docs/snippets/climb_noise.hl --strict climb_kept_valid
climb_revert_skipped docs/snippets/climb_noise.hl --strict climb_state_follows
climb_ledger_drops_losses docs/snippets/climb_noise.hl --strict climb_ledger_complete
climb_budget_unchecked docs/snippets/climb_noise.hl --strict climb_cost_balanced
climb_cost_double_counted docs/snippets/climb_noise.hl --strict climb_cost_balanced
climb_gate_ignored docs/snippets/climb_gate.hl --strict climb_kept_valid"
exempt="none: the unmutated program, which every sweep in scripts/check.sh runs"
diff_seeds="${DIFF_PLANT_SEEDS:-0..100}"
diff_plans="wasm flush_after_dispatch examples/agent.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm crc_skipped examples/agent.hl native-jsonl backend_equal
wasm recovery_skipped examples/agent.hl native-jsonl,native-sqlite,server-postgres,cloudflare backend_equal
wasm recovery_unreported examples/agent.hl native-jsonl,native-sqlite,server-postgres,cloudflare backend_equal
wasm mode_dropped docs/snippets/modes.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm settings_dropped docs/snippets/settings.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm usage_unchecked docs/snippets/recovery.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm model_failure_decoded docs/snippets/settings.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm interrupted_undecoded docs/snippets/in_flight.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm rewrite_kept_lost docs/snippets/rewrite.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm batch_flush_skipped docs/snippets/parallel.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm fork_hook_skipped docs/snippets/fork.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm event_ack_early docs/snippets/events.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm event_redelivered examples/claude_agent.hl native-jsonl,native-sqlite,server-postgres,cloudflare backend_equal
wasm cancel_read_unstamped docs/snippets/events.hl vercel backend_equal
wasm suspend_before_flush docs/snippets/park.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm resume_unmatched docs/snippets/park.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm wait_unjournaled docs/snippets/park.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm side_live_state docs/snippets/side.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm batch_drain_skipped examples/claude_agent.hl native-jsonl,native-sqlite,server-postgres,cloudflare backend_equal
wasm idle_early docs/snippets/keep.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm blob_unjournaled docs/snippets/images.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm blobs_unevicted docs/snippets/images.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm blob_bound_unchecked docs/snippets/images.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm listing_unchecked docs/snippets/listing.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm discovery_unchecked docs/snippets/catalog.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm search_unchecked docs/snippets/catalog.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm listing_bound_unchecked docs/snippets/listing.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm interrupt_ignored docs/snippets/in_flight.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm recover_reply_ignored docs/snippets/in_flight.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm job_unopened docs/snippets/in_flight.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm job_lost_dropped docs/snippets/in_flight.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm grandchild_unscoped docs/snippets/tree_planner.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal 1.1
wasm cell_leaks docs/snippets/code_wait.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm cell_cancel_ignored docs/snippets/code_stop.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm nested_recovery_skipped examples/code_agent.hl native-jsonl,native-sqlite,server-postgres,cloudflare backend_equal
wasm nested_result_unjournaled examples/code_agent.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal
wasm fold_arms_flipped examples/minimal.hl native-jsonl,native-sqlite,server-postgres,cloudflare,vercel backend_equal"
diff_exempt="wasm crc_skipped native-sqlite: only native-jsonl writes a checksum per line, so the planted change runs on no other target
wasm crc_skipped server-postgres: only native-jsonl writes a checksum per line, so the planted change runs on no other target
wasm crc_skipped cloudflare: only native-jsonl writes a checksum per line, so the planted change runs on no other target
wasm crc_skipped vercel: only native-jsonl writes a checksum per line, so the planted change runs on no other target
wasm nested_recovery_skipped vercel: Vercel recovers an open nested external call inside its step, by its attempt count, so the skipped recovery at load is never reached
wasm recovery_skipped vercel: Vercel recovers an open external call inside its step, by its attempt count, so the skipped recovery record is never reached
wasm recovery_unreported vercel: Vercel recovers an open external call inside its step, by its attempt count, so the unreported recovery record is never reached
wasm event_redelivered vercel: a Vercel hook delivers each payload once, so no delivery repeats an id the turn recorded and the skipped duplicate check is never reached
wasm batch_drain_skipped vercel: Vercel drains its inbox after a flush, not after a run, so the skipped drain after a batch runs on no Vercel turn
wasm cancel_read_unstamped native-jsonl: a cancel read before its stamp needs a rerun past a crash after the drain; seeds 0..2000 catch it first at seed 118
wasm cancel_read_unstamped native-sqlite: a cancel read before its stamp needs a rerun past a crash after the drain; seeds 0..2000 catch it first at seed 206
wasm cancel_read_unstamped server-postgres: a cancel read before its stamp needs a rerun past a crash after the drain; seeds 0..2000 catch it first at seed 401
wasm cancel_read_unstamped cloudflare: a cancel read before its stamp needs a rerun past a crash after the drain; seeds 0..2000 catch it first at seed 206"
caught_elsewhere=scripts/strict.sh

[ -x "$hl" ] || cargo build --release --locked

snake() {
  sed -E 's/([a-z0-9])([A-Z])/\1_\2/g' | tr '[:upper:]' '[:lower:]'
}

variants() {
  sed -n "/^pub enum $2 {/,/^}/s/^ *\([A-Z][A-Za-z0-9]*\),$/\1/p" "$1" | snake | sort
}

census_fail() {
  echo "mutations census: $1" >&2
  exit 1
}

census() {
  local usage listed planned exempted named variant names
  usage=$("$hl" 2>&1 || true)
  listed=$(sed -n 's/.*hl sim --target .*\[--mutation \([a-z_|]*\)\].*/\1/p' <<< "$usage" | tr '|' '\n' | sort)
  [ -n "$listed" ] || census_fail "hl's usage lists no simulator mutations"
  if [ "$(variants src/run/mutation.rs Mutation)" != "$listed" ]; then
    census_fail "the Mutation enum in src/run/mutation.rs and the mutations hl sim accepts differ (is the variant missing from Mutation::ALL?)"
  fi
  planned=$(cut -d' ' -f1 <<< "$plans" | sort)
  exempted=$(cut -d: -f1 <<< "$exempt" | sort)
  named=$(sort <<< "$planned"$'\n'"$exempted")
  [ -z "$(uniq -d <<< "$named")" ] || census_fail "planned or exempted twice: $(uniq -d <<< "$named" | tr '\n' ' ')"
  for variant in $listed; do
    grep -qx "$variant" <<< "$named" || census_fail "mutation $variant is in neither the plans of scripts/mutations.sh nor its exemptions with a reason"
  done
  for variant in $named; do
    grep -qx "$variant" <<< "$listed" || census_fail "scripts/mutations.sh names $variant, which hl sim does not accept"
  done
  names=$(sed -n 's/^ *Self::[A-Za-z0-9]* => "\([a-z_]*\)",$/\1/p' src/sim/invariants.rs | sort)
  if [ "$(variants src/sim/invariants.rs Invariant)" != "$names" ]; then
    census_fail "the Invariant enum in src/sim/invariants.rs and the names it reports differ"
  fi
  local exercised
  exercised=$( (cut -d' ' -f4 <<< "$plans" | tr '|' '\n'; sed -n 's/^ *expect_caught [^ ]* \([a-z_]*\) .*/\1/p' "$caught_elsewhere") | sort -u)
  for variant in $names; do
    grep -qx "$variant" <<< "$exercised" || census_fail "invariant $variant is exercised by no mutation in scripts/mutations.sh and no planted program in $caught_elsewhere"
  done
  for variant in $exercised; do
    grep -qx "$variant" <<< "$names" || census_fail "invariant $variant is expected but src/sim/invariants.rs has no such invariant"
  done
  echo "mutations census: $(wc -w <<< "$planned") mutations planned, $(wc -w <<< "$exempted") exempt with a reason; all $(wc -w <<< "$names") invariants exercised"
}

run_one() {
  local mutation=$1 file mode expected range target status output invariant applied=0 seen=""
  read -r _ file mode expected range <<< "$(grep "^$mutation " <<< "$plans")"
  range=${range:-$seeds}
  for target in $targets; do
    set +e
    output=$("$hl" sim --target "$target" --seeds "$range" "$mode" --mutation "$mutation" "$file" 2>&1)
    status=$?
    set -e
    if [ "$status" -eq 2 ]; then
      continue
    fi
    applied=$((applied + 1))
    invariant=$(sed -n 's/^FAIL invariant=\([a-z_]*\) .*/\1/p' <<< "$output" | head -n 1)
    if [ "$status" -ne 1 ] || [ -z "$invariant" ]; then
      echo "mutation $mutation on $target was not caught in seeds $range:" >&2
      echo "$output" >&2
      exit 1
    fi
    if ! tr '|' '\n' <<< "$expected" | grep -qx "$invariant"; then
      echo "mutation $mutation on $target was caught by $invariant, not by $expected:" >&2
      echo "$output" >&2
      exit 1
    fi
    seen="$seen $invariant"
    echo "caught $mutation on $target: $(grep -m1 '^FAIL invariant=' <<< "$output" | cut -d: -f1)"
  done
  if [ "$applied" -eq 0 ]; then
    echo "mutation $mutation applies to no target" >&2
    exit 1
  fi
  for invariant in $(tr '|' ' ' <<< "$expected"); do
    if ! grep -qw "$invariant" <<< "$seen"; then
      echo "mutation $mutation lists $invariant, which caught it on no target" >&2
      exit 1
    fi
  done
}

diff_script() {
  case $1 in
    wasm) echo scripts/wasm-diff.sh ;;
  esac
}

diff_targets() {
  case $1 in
    wasm) echo "native-jsonl native-sqlite server-postgres cloudflare vercel" ;;
  esac
}

diff_run() {
  local backend=$1 plant=$2 file=$3 target=$4 seeds=$5 scope=$6 prefix
  prefix=$(tr '[:lower:]' '[:upper:]' <<< "$backend")_DIFF
  env "${prefix}_PLANT=$plant" "${prefix}_TARGETS=$target" "${prefix}_FILES=$file" "${prefix}_SEEDS=$seeds" "${prefix}_SCOPE=$scope" "$(diff_script "$backend")" 2>&1
}

diff_plants_of() {
  if [ "$1" = wasm ]; then
    awk '/^pub const PLANTS/ { inside = 1 } inside && /^\];/ { inside = 0 } inside && previous ~ /^    \($/ { gsub(/[",[:space:]]/, ""); print } inside && /^    \("/ { split($0, parts, "\""); print parts[2] } { previous = $0 }' src/emit/wasm/mod.rs | sort
    return
  fi
  sed -n '/^plant_expression() {/,/^}/s/^    \([a-z_]*\)) echo .*/\1/p' "$(diff_script "$1")" | grep -vx none | sort
}

diff_census() {
  local backend planted plant file targets target output
  for backend in wasm; do
    planted=$(awk -v b="$backend" '$1 == b { print $2 }' <<< "$diff_plans" | sort)
    [ -n "$(diff_plants_of "$backend")" ] || census_fail "$(diff_script "$backend") lists no plants"
    if [ "$(diff_plants_of "$backend")" != "$planted" ]; then
      census_fail "the plants of $(diff_script "$backend") and the $backend plans of scripts/mutations.sh differ"
    fi
    while read -r _ plant file targets _ scope; do
      for target in $(diff_targets "$backend"); do
        grep -qw "$target" <<< "${targets//,/ }" && continue
        grep -q "^$backend $plant $target: " <<< "$diff_exempt" && continue
        set +e
        output=$(diff_run "$backend" "$plant" "$file" "$target" 0..0 "$scope")
        set -e
        grep -q "plant $plant changes nothing in $file for $target" <<< "$output" ||
          census_fail "$backend plant $plant changes $file on $target, which its plan leaves out without an exemption"
      done
    done < <(awk -v b="$backend" '$1 == b' <<< "$diff_plans")
  done
  echo "mutations census: $(wc -l <<< "$diff_plans") differential plants planned, each target left out changes nothing or has a reason"
}

diff_caught_by() {
  if grep -q 'FAIL invariant=' <<< "$1"; then
    sed -n 's/.*FAIL invariant=\([a-z_]*\) .*/\1/p' <<< "$1" | head -n 1
  elif grep -q "^thread 'main' .* panicked at " <<< "$1"; then
    echo panic
  fi
}

diff_one() {
  local backend plant target file targets expected scope output status invariant
  IFS=: read -r backend plant target <<< "$1"
  read -r _ _ file targets expected scope <<< "$(awk -v b="$backend" -v p="$plant" '$1 == b && $2 == p' <<< "$diff_plans")"
  [ -n "$file" ] || { echo "mutations: $backend plant $plant has no plan" >&2; exit 1; }
  grep -qw "$target" <<< "${targets//,/ }" || { echo "mutations: $backend plant $plant plans no $target" >&2; exit 1; }
  set +e
  output=$(diff_run "$backend" "$plant" "$file" "$target" "$diff_seeds" "$scope")
  status=$?
  set -e
  invariant=$(diff_caught_by "$output")
  if [ "$status" -eq 0 ] || [ -z "$invariant" ]; then
    echo "$backend plant $plant on $target was not caught by the differential run in seeds $diff_seeds:" >&2
    echo "$output" >&2
    exit 1
  fi
  if ! tr '|' '\n' <<< "$expected" | grep -qx "$invariant"; then
    echo "$backend plant $plant on $target was caught by $invariant, not by $expected:" >&2
    echo "$output" >&2
    exit 1
  fi
  echo "caught $backend plant $plant on $target by $invariant in seeds $diff_seeds"
}

deploy_plans="migration_every_deploy cloudflare Migration tag precondition failed
secrets_dropped cloudflare,native-jsonl HL_TOKEN is gone after the second deploy
digest_wrong vercel does not match the file's SHA-1
rollback_current cloudflare,native-jsonl the rollback does not serve the first deploy's digest
rollback_current vercel The deployment is already the production deployment
environment_type_unknown vercel requests or fake answers broke the pinned specs
fingerprint_partial cloudflare,vercel a changed variable deployed as unchanged
auth_unchecked cloudflare,vercel an unauthenticated request was served"

deploy_plants_of_hl() {
  "$hl" deploy 2>&1 | sed -n 's/.*deploy plants: --plant //p' | tr '|' '\n' | grep -vx none | sort
}

deploy_census() {
  local planted
  planted=$(cut -d' ' -f1 <<< "$deploy_plans" | sort -u)
  [ -n "$(deploy_plants_of_hl)" ] || census_fail "hl deploy lists no deploy plants"
  if [ "$(deploy_plants_of_hl)" != "$planted" ]; then
    census_fail "the deploy plants hl accepts and the deploy plans of scripts/mutations.sh differ"
  fi
  echo "mutations census: $(wc -l <<< "$planted") deploy plants planned"
}

deploy_one() {
  local plant target expected output status
  IFS=: read -r plant target <<< "$1"
  expected=$(awk -v p="$plant" -v t="$target" '$1 == p && index("," $2 ",", "," t ",") { $1 = ""; $2 = ""; sub(/^  /, ""); print }' <<< "$deploy_plans")
  [ -n "$expected" ] || { echo "mutations: deploy plant $plant plans no $target" >&2; exit 1; }
  set +e
  output=$(DEPLOY_DRILL_PLANT=$plant DEPLOY_DRILL_TARGETS=$target DEPLOY_DRILL_SCRATCH=output/deploy-drill/$plant-$target scripts/deploy-drill.sh 2>&1)
  status=$?
  set -e
  if [ "$status" -eq 0 ] || ! grep -qF -- "$expected" <<< "$output"; then
    echo "deploy plant $plant on $target was not caught by the deploy drill ($expected):" >&2
    echo "$output" >&2
    exit 1
  fi
  echo "caught deploy plant $plant on $target: $expected"
}

if [ "${1:-}" = --deploy-plans ]; then
  while read -r plant targets _; do
    for target in ${targets//,/ }; do
      echo "$plant:$target"
    done
  done <<< "$deploy_plans"
  exit 0
fi
if [ "${1:-}" = --deploy ]; then
  deploy_one "$2"
  exit 0
fi
if [ "${1:-}" = --diff-plans ]; then
  while read -r backend plant _ targets _; do
    for target in ${targets//,/ }; do
      echo "$backend:$plant:$target"
    done
  done <<< "$diff_plans"
  exit 0
fi
if [ "${1:-}" = --diff ]; then
  diff_one "$2"
  exit 0
fi
if [ "${1:-}" = --plans ]; then
  cut -d' ' -f1 <<< "$plans"
  exit 0
fi
census
if [ "${1:-}" = --census ]; then
  diff_census
  deploy_census
  exit 0
fi
selected="${MUTATIONS:-$(cut -d' ' -f1 <<< "$plans")}"
for mutation in $selected; do
  grep -q "^$mutation " <<< "$plans" || { echo "mutations: $mutation has no plan" >&2; exit 1; }
  run_one "$mutation"
done
echo "mutations: $(wc -w <<< "$selected") mutations caught by the invariants they plan, in seeds $seeds"
