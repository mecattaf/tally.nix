#!/bin/sh
set -eu

control=$1
task=${CAMPAIGN_TASK_ID:?CAMPAIGN_TASK_ID is required}

if [ "$task" = task-1 ] && [ ! -e "$control/failed-once" ]; then
  : >"$control/failed-once"
  printf '%s\n' 'fixture gate fails once before any publish or later-task prep' >&2
  exit 1
fi

case "$task" in
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

printf '%s\n' "$task" >>"$control/gate-order.log"
