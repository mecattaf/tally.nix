#!/bin/sh
# The fixture judge candidate. It answers canned verdicts and calls no model:
# the lane proves corpus assembly, the replay plumbing, and the disagreement
# table, and the live run against a real candidate is a seam act performed by
# the operator side, never by a test and never by a gate.
#
# It asserts the two halves of the dispatch contract before answering:
#
#   - the workload argv is the brief sentinel, so the harness composed the
#     adapter's own argv with the sentinel the production diagnosis node uses;
#   - the brief itself arrives as the TALLY_BRIEF file and is readable, which
#     is the shape a diagnosis dispatch has (a job unit has no stdin, so a
#     candidate that read stdin would answer from an empty read).
#
# Which canned verdict comes back is keyed off the case directory, which the
# harness makes the process's working directory. That keeps the fixture free of
# a JSON parser without making it blind to which case it was asked about.
set -eu

case "${1:-}" in
  *TALLY_BRIEF*) ;;
  *) echo "fixture candidate: workload argv is not the brief sentinel" >&2; exit 64 ;;
esac
: "${TALLY_BRIEF:?fixture candidate: TALLY_BRIEF is unset}"
test -r "$TALLY_BRIEF" || {
  echo "fixture candidate: TALLY_BRIEF names no readable file" >&2
  exit 65
}
grep -q '"role"' "$TALLY_BRIEF" || {
  echo "fixture candidate: the brief carries no role" >&2
  exit 66
}

case "$(basename "$(pwd)")" in
  *agrees*)
    answer='{"verdict":"retry","diagnosis":"fixture candidate agrees with the seat"}'
    ;;
  *dissents*)
    answer='{"verdict":"blocked","diagnosis":"fixture candidate reads the same failure as terminal"}'
    ;;
  *off-schema*)
    answer='{"verdict":"perhaps","diagnosis":"fixture candidate answers outside the schema"}'
    ;;
  *)
    answer='{"verdict":"transient","diagnosis":"fixture candidate has no canned answer for this case"}'
    ;;
esac
printf 'TALLY_FINAL_MESSAGE=%s\n' "$answer"
