#!/usr/bin/env bash
# Measure the parallel-execution flake rate of one cargo test binary (#419).
#
# #419's exit criterion is a measured rate over a wave of runs under load, not a
# green run of the tests that were last seen failing. Every increment to that
# population so far had an unrelated cause, and the discovery rate tracked how
# hard anyone looked -- so the number, not the list, is what says whether the
# population is bounded. This script is that measurement, so the next person to
# need it does not hand-roll a loop whose load condition nobody can compare
# against the last one.
#
# The load condition is the concurrent suites themselves. Never a spinner or a
# stress tool: those change the shape of the contention and, on a host running
# other work, corrupt somebody else's measurement.
#
#   test/flake-probe.sh <test-binary> [seconds] [concurrent-suites]
#
# Build the binary first, and reuse it across every run of the wave, so the
# measurement is of the suite and not of rustc competing with it:
#
#   env -u TALLY_TEST_REMOTE_HOST cargo test -p tally-core --lib --no-run
#   test/flake-probe.sh target/debug/deps/tally_core-<hash> 480 2
#
# It prints one summary line per concurrent suite and a total, and appends the
# WHOLE OUTPUT of every failing run to <binary>.flake-probe.failures in the
# working directory. The whole output, not a grep for the test name: the
# expensive part of a wave is catching a failure, and a name without its
# `panicked at` line and its left/right values costs the next wave a full
# re-reproduction to learn the mechanism -- which it did cost one. Exit status
# is 0 when the wave completed, whatever the rate was: this measures, it does
# not gate.
#
# Deliberately no `--test-threads` override. The races this counts are between
# sibling tests inside one process, so capping the suite's own parallelism
# suppresses the thing being measured -- an early wave of this measurement ran
# at `--test-threads 4` and found nothing, which said more about the flag than
# about the tree.
set -uo pipefail

binary="${1:?usage: flake-probe.sh <test-binary> [seconds] [concurrent-suites]}"
budget="${2:-480}"
suites="${3:-2}"

if [ ! -x "$binary" ]; then
  echo "flake-probe: $binary is not an executable test binary" >&2
  exit 2
fi

failures_file="$(basename "$binary").flake-probe.failures"
: >"$failures_file"
tally_file="$(mktemp)"
trap 'rm -f "$tally_file"' EXIT

run_suite() {
  local lane="$1"
  local runs=0 fails=0 output
  local end=$(($(date +%s) + budget))
  while [ "$(date +%s)" -lt "$end" ]; do
    runs=$((runs + 1))
    if ! output=$(env -u TALLY_TEST_REMOTE_HOST "$binary" 2>&1); then
      fails=$((fails + 1))
      {
        echo "=== suite $lane run $runs failed ==="
        printf '%s\n' "$output"
        echo "=== end suite $lane run $runs ==="
      } >>"$failures_file"
    fi
  done
  echo "$lane $runs $fails" >>"$tally_file"
}

echo "flake-probe: $suites concurrent suites of $binary for ${budget}s"
for lane in $(seq 1 "$suites"); do
  run_suite "$lane" &
done
wait

total_runs=0
total_fails=0
while read -r lane runs fails; do
  echo "flake-probe: suite $lane runs=$runs failed=$fails"
  total_runs=$((total_runs + runs))
  total_fails=$((total_fails + fails))
done <"$tally_file"
echo "flake-probe: $total_fails / $total_runs runs had at least one failing test"
if [ "$total_fails" -gt 0 ]; then
  echo "flake-probe: full output of every failing run in $failures_file"
fi
