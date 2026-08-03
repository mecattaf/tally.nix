#!/bin/sh
set -eu

control=$1
gate=$2
task=${CAMPAIGN_TASK_ID:?CAMPAIGN_TASK_ID is required}

# The preflight witness runs this exact argv on the pristine campaign base,
# before any agent has built anything. It is deliberately red there, and it
# must say so by its own name rather than by a stray `cat` failure -- and it
# must not consume the one-shot post-change failure below, which belongs to a
# lane that has already run its agent.
if [ ! -d build ]; then
  printf '%s\n' 'fixture gate argv is red on the pristine campaign base' >&2
  exit 3
fi

if [ "$task" = task-1 ] && [ "$gate" = first ] && [ ! -e "$control/post-change-failed-once" ]; then
  : >"$control/post-change-failed-once"
  printf '%s\n' 'fixture post-change gate fails once before publish' >&2
  exit 1
fi

if [ "$task" = task-2 ] && [ "$gate" = first ]; then
  printf '%s:%s\n' "$task" "$gate" >>"$control/gate-order.log"
  printf '%s\n' 'task 2 deterministic gate failure after implementation' >&2
  exit 1
fi

case "$task" in
  task-1)
    test "$(cat build/one.txt)" = one
    ;;
  task-2 | task-2b)
    test "$(cat build/one.txt)" = one
    test "$(cat build/two.txt)" = two
    ;;
  task-3)
    test "$(cat build/three.txt)" = three
    ;;
  task-4)
    test "$(cat build/four.txt)" = four
    test ! -e build/checkpoint-red
    ;;
  task-5)
    test "$(cat build/five.txt)" = five
    ;;
  task-6)
    test "$(cat build/six.txt)" = six
    ;;
  *)
    printf 'unknown fixture task: %s\n' "$task" >&2
    exit 2
    ;;
esac

printf '%s:%s\n' "$task" "$gate" >>"$control/gate-order.log"
