#!/bin/sh
set -eu

control=$1
gate=$2
task=${CAMPAIGN_TASK_ID:?CAMPAIGN_TASK_ID is required}

test "$task" = task-1
test ! -e build/one.txt

if [ "$gate" = first ] && [ ! -e "$control/preflight-timed-out-once" ]; then
  : >"$control/preflight-timed-out-once"
  printf '%s\n' 'fixture preflight exceeds its bounded deadline before any agent dispatch' >&2
  sleep 5
fi

printf 'preflight:%s:%s\n' "$task" "$gate" >>"$control/preflight-order.log"
