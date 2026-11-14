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
model_failure_unrecorded examples/claude_agent.hl --twice replay_equal|liveness|seq_unique
batch_flush_after_dispatch examples/claude_agent.hl --twice external_once|unknown_reported
results_in_completion_order examples/claude_agent.hl --twice sequential_equal
batch_pushes_reversed examples/claude_agent.hl --twice replay_equal
listing_unchecked examples/codex_agent.hl --twice tool_admitted
listed_class_dropped examples/codex_agent.hl --twice external_once
input_unchecked examples/codex_agent.hl --twice tool_admitted
loop_past_bound examples/sound/full_loop.hl --strict worst_case"
exempt="none: the unmutated program, which every sweep in scripts/check.sh runs"
diff_seeds="${DIFF_PLANT_SEEDS:-0..100}"
diff_plans="rust flush_after_dispatch examples/agent.hl native-jsonl,native-sqlite,server-postgres backend_equal
rust bound_message examples/agent.hl native-jsonl,native-sqlite,server-postgres backend_equal
rust double_fsync examples/agent.hl native-jsonl backend_equal
rust batch_seq_from_zero examples/agent.hl native-sqlite,server-postgres backend_equal
rust recovery_skipped examples/agent.hl native-jsonl,native-sqlite,server-postgres panic
rust enum_unchecked examples/hooks.hl native-jsonl,native-sqlite,server-postgres backend_equal
js flush_after_dispatch examples/agent.hl vercel backend_equal
js bound_message examples/agent.hl cloudflare,vercel backend_equal
js enum_unchecked examples/hooks.hl cloudflare,vercel backend_equal|panic"
diff_exempt="js flush_after_dispatch cloudflare: the output gate flushes before every outbound request, so the planted skip changes no flush the trace can see"
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
  local mutation=$1 file mode expected target status output invariant applied=0 seen=""
  read -r _ file mode expected <<< "$(grep "^$mutation " <<< "$plans")"
  for target in $targets; do
    set +e
    output=$("$hl" sim --target "$target" --seeds "$seeds" "$mode" --mutation "$mutation" "$file" 2>&1)
    status=$?
    set -e
    if [ "$status" -eq 2 ]; then
      continue
    fi
    applied=$((applied + 1))
    invariant=$(sed -n 's/^FAIL invariant=\([a-z_]*\) .*/\1/p' <<< "$output" | head -n 1)
    if [ "$status" -ne 1 ] || [ -z "$invariant" ]; then
      echo "mutation $mutation on $target was not caught in seeds $seeds:" >&2
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
    rust) echo scripts/rust-diff.sh ;;
    js) echo scripts/js-diff.sh ;;
  esac
}

diff_targets() {
  case $1 in
    rust) echo "native-jsonl native-sqlite server-postgres" ;;
    js) echo "cloudflare vercel" ;;
  esac
}

diff_run() {
  local backend=$1 plant=$2 file=$3 target=$4 seeds=$5 prefix
  prefix=$(tr '[:lower:]' '[:upper:]' <<< "$backend")_DIFF
  env "${prefix}_PLANT=$plant" "${prefix}_TARGETS=$target" "${prefix}_FILES=$file" "${prefix}_SEEDS=$seeds" "$(diff_script "$backend")" 2>&1
}

diff_plants_of() {
  sed -n '/^plant_expression() {/,/^}/s/^    \([a-z_]*\)) echo .*/\1/p' "$(diff_script "$1")" | grep -vx none | sort
}

diff_census() {
  local backend planted plant file targets target output
  for backend in rust js; do
    planted=$(awk -v b="$backend" '$1 == b { print $2 }' <<< "$diff_plans" | sort)
    [ -n "$(diff_plants_of "$backend")" ] || census_fail "$(diff_script "$backend") lists no plants"
    if [ "$(diff_plants_of "$backend")" != "$planted" ]; then
      census_fail "the plants of $(diff_script "$backend") and the $backend plans of scripts/mutations.sh differ"
    fi
    while read -r _ plant file targets _; do
      for target in $(diff_targets "$backend"); do
        grep -qw "$target" <<< "${targets//,/ }" && continue
        grep -q "^$backend $plant $target: " <<< "$diff_exempt" && continue
        set +e
        output=$(diff_run "$backend" "$plant" "$file" "$target" 0..0)
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
  local backend plant target file targets expected output status invariant
  IFS=: read -r backend plant target <<< "$1"
  read -r _ _ file targets expected <<< "$(awk -v b="$backend" -v p="$plant" '$1 == b && $2 == p' <<< "$diff_plans")"
  [ -n "$file" ] || { echo "mutations: $backend plant $plant has no plan" >&2; exit 1; }
  grep -qw "$target" <<< "${targets//,/ }" || { echo "mutations: $backend plant $plant plans no $target" >&2; exit 1; }
  set +e
  output=$(diff_run "$backend" "$plant" "$file" "$target" "$diff_seeds")
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
  exit 0
fi
selected="${MUTATIONS:-$(cut -d' ' -f1 <<< "$plans")}"
for mutation in $selected; do
  grep -q "^$mutation " <<< "$plans" || { echo "mutations: $mutation has no plan" >&2; exit 1; }
  run_one "$mutation"
done
echo "mutations: $(wc -w <<< "$selected") mutations caught by the invariants they plan, in seeds $seeds"
