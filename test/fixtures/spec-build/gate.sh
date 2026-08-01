#!/bin/sh
set -eu

control=$1
gate=$2
task=${CAMPAIGN_TASK_ID:-preflight}

if [ "$task" = preflight ] && [ "$gate" = first ] && [ ! -e "$control/preflight-failed-once" ]; then
  : >"$control/preflight-failed-once"
  printf '%s\n' 'fixture preflight gate fails once before any agent dispatch' >&2
  exit 1
fi

case "$task" in
  preflight)
    test ! -e build/one.txt
    ;;
  task-1)
    test "$(cat build/one.txt)" = one
    ;;
  task-2)
    test "$(cat build/one.txt)" = one
    test "$(cat build/two.txt)" = two
    ;;
  *)
    printf 'unknown fixture task: %s\n' "$task" >&2
    exit 2
    ;;
esac

printf '%s:%s\n' "$task" "$gate" >>"$control/gate-order.log"
