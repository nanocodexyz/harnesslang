#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$#" -lt 4 ]; then
  echo "usage: $0 BEFORE_HL AFTER_HL RUNS FILE..." >&2
  exit 2
fi
before=$1
after=$2
runs=$3
shift 3
nanoseconds_per_millisecond=1000000

elapsed_ns() {
  local start end
  start=$(date +%s%N)
  "$@" > /dev/null 2>&1 || true
  end=$(date +%s%N)
  echo $((end - start))
}

for file in "$@"; do
  before_total=0
  after_total=0
  for _ in $(seq "$runs"); do
    before_total=$((before_total + $(elapsed_ns "$before" prove "$file")))
    after_total=$((after_total + $(elapsed_ns "$after" prove "$file")))
  done
  before_mean=$((before_total / runs))
  after_mean=$((after_total / runs))
  awk -v file="$file" -v runs="$runs" -v b="$before_mean" -v a="$after_mean" -v unit="$nanoseconds_per_millisecond" \
    'BEGIN { printf "%s: before %.2f ms, after %.2f ms, %.1fx (mean of %d interleaved runs)\n", file, b / unit, a / unit, b / a, runs }'
done
