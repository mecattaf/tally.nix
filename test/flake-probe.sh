#!/usr/bin/env bash
# Measure the parallel-execution flake rate of tally-core's lib test binary
# (#419).
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
#   test/flake-probe.sh [seconds] [concurrent-suites]
#   test/flake-probe.sh <test-binary> <seconds> <concurrent-suites>
#
# The probe builds the package-scoped lib target itself, extracts the exact
# executable from that Cargo invocation's JSON, and reuses it across every run
# of the wave. This keeps rustc out of the measurement and prevents a caller
# from silently selecting a stale hash left by `--workspace` or another build:
#
#   test/flake-probe.sh 480 3
#
# A caller that has already identified the exact artifact under test may pass
# that executable explicitly. The duration and concurrency are required in
# this form so it remains unambiguous with the self-building form above:
#
#   test/flake-probe.sh target/debug/deps/tally_core-<hash> 480 3
#
# It prints one summary line per concurrent suite and a total, and appends the
# WHOLE OUTPUT of every failing run to <binary>.flake-probe.failures in the
# working directory. Each lane writes separately until the wave ends, so whole
# failing runs cannot interleave. The whole output, not a grep for the test
# name: the expensive part of a wave is catching a failure, and a name without its
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

usage() {
  echo "usage: flake-probe.sh [positive-seconds] [positive-concurrent-suites]" >&2
  echo "       flake-probe.sh <test-binary> <positive-seconds> <positive-concurrent-suites>" >&2
  exit 2
}

binary=""
case "$#" in
  0 | 1 | 2)
    budget="${1:-480}"
    suites="${2:-3}"
    ;;
  3)
    binary="$1"
    budget="$2"
    suites="$3"
    ;;
  *) usage ;;
esac

if ! [[ "$budget" =~ ^[1-9][0-9]*$ ]]; then
  usage
fi
if ! [[ "$suites" =~ ^[1-9][0-9]*$ ]]; then
  usage
fi

scratch_dir="$(mktemp -d)"
tally_file="$scratch_dir/tally"
failures_dir="$scratch_dir/failures"
mkdir "$failures_dir"
trap 'rm -rf -- "$scratch_dir"' EXIT

if [ -z "$binary" ]; then
  build_json="$scratch_dir/cargo-build.json"
  echo "flake-probe: building fresh tally-core lib test executable"
  if ! env -u TALLY_TEST_REMOTE_HOST cargo test -p tally-core --lib --no-run \
    --message-format=json >"$build_json"; then
    echo "flake-probe: cargo test --no-run failed" >&2
    jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' \
      "$build_json" >&2 || true
    exit 2
  fi

  mapfile -t binaries < <(
    jq -r '
      select(.reason == "compiler-artifact")
      | select(.target.name == "tally_core")
      | select(.profile.test == true)
      | select((.target.kind | index("lib")) != null)
      | .executable // empty
    ' "$build_json" | sort -u
  )
  if [ "${#binaries[@]}" -ne 1 ]; then
    echo "flake-probe: expected one fresh tally_core lib test executable, found ${#binaries[@]}" >&2
    printf 'flake-probe: candidate %s\n' "${binaries[@]}" >&2
    exit 2
  fi
  binary="${binaries[0]}"
fi
if [ ! -x "$binary" ]; then
  echo "flake-probe: test binary is not executable: $binary" >&2
  exit 2
fi

failures_file="$(basename "$binary").flake-probe.failures"
: >"$failures_file"

run_suite() {
  local lane="$1"
  local runs=0 fails=0 output
  local end
  end=$(($(date +%s) + budget))
  while [ "$(date +%s)" -lt "$end" ]; do
    runs=$((runs + 1))
    if ! output=$(env -u TALLY_TEST_REMOTE_HOST "$binary" 2>&1); then
      fails=$((fails + 1))
      {
        echo "=== suite $lane run $runs failed ==="
        printf '%s\n' "$output"
        echo "=== end suite $lane run $runs ==="
      } >>"$failures_dir/$lane"
    fi
  done
  echo "$lane $runs $fails" >>"$tally_file"
}

echo "flake-probe: $suites concurrent suites of $binary for ${budget}s"
for lane in $(seq 1 "$suites"); do
  run_suite "$lane" &
done
wait

for lane in $(seq 1 "$suites"); do
  if [ -s "$failures_dir/$lane" ]; then
    cat "$failures_dir/$lane" >>"$failures_file"
  fi
done

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
