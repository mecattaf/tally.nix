#!/bin/sh
set -eu

control=$1
gate=$2
task=${CAMPAIGN_TASK_ID:?CAMPAIGN_TASK_ID is required}

test "$task" = task-1
test ! -e build/one.txt

# The base-safe premise every command gate's probe is entitled to: no gate's
# real argv has run in this lane yet. gate.sh's witness branch drops this marker,
# so interleaving a witness between two gating probes turns the second gate's
# probe red -- which is the defect this assertion exists to catch, not an
# incidental fixture detail. Probes run before any witness; keep it that way.
test ! -e preflight-witness-ran

# The ordinary red probe: a plain non-zero exit well inside the deadline. It is
# the "this host has no toolchain" shape, which reaches the same
# `preflight-failed` refusal by a different path than the timeout below.
if [ "$gate" = first ] && [ ! -e "$control/preflight-failed-once" ]; then
  : >"$control/preflight-failed-once"
  printf '%s\n' 'fixture preflight cannot find the toolchain on this host' >&2
  exit 1
fi

if [ "$gate" = first ] && [ ! -e "$control/preflight-timed-out-once" ]; then
  : >"$control/preflight-timed-out-once"
  printf '%s\n' 'fixture preflight exceeds its bounded deadline before any agent dispatch' >&2
  sleep 5
fi

printf 'preflight:%s:%s\n' "$task" "$gate" >>"$control/preflight-order.log"
