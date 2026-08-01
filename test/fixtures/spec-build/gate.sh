#!/bin/sh
set -eu

control=$1
gate=$2
task=${CAMPAIGN_TASK_ID:?CAMPAIGN_TASK_ID is required}

if [ "$task" = task-1 ] && [ "$gate" = first ] && [ ! -e "$control/post-change-failed-once" ]; then
  : >"$control/post-change-failed-once"
  printf '%s\n' 'fixture post-change gate fails once before publish' >&2
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

printf '%s:%s\n' "$task" "$gate" >>"$control/gate-order.log"
