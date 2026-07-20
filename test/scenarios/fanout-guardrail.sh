#!/usr/bin/env bash

set -euo pipefail

source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

SCENARIO_NAME="fanout-guardrail"
require_command jq
resolve_tally_bin
new_scenario_root fanout

cleanup() {
  stop_daemon
  remove_scenario_root "$SCENARIO_ROOT"
}
trap cleanup EXIT INT TERM

config="$SCENARIO_ROOT/config.json"
jq -n '{
  enqueue: {depthCap: 1, fanoutCap: 3, requireDedupKey: true},
  pools: {slot: {resource: "build-slot", capacity: 1, enforce: "cooperative"}},
  adapters: {shell: {}},
  journald: {native: false}
}' >"$config"

"$TALLY_BIN" --mode check-config --config "$config" >/dev/null
start_daemon "$config" "$SCENARIO_ROOT"

# Keep every admitted row paused. The scenario exercises admission through the
# real daemon without introducing executor behavior already covered elsewhere.
"$TALLY_BIN" --socket "$SCENARIO_SOCKET" queue pause --all >/dev/null
parent_json="$($TALLY_BIN \
  --socket "$SCENARIO_SOCKET" \
  enqueue \
  --pool slot \
  --dedup-key scenario-parent \
  -- /bin/false)"
parent_uuid="$(jq -er '.task_uuid' <<<"$parent_json")"

child_count=8
child_pids=()
for ((index = 1; index <= child_count; index++)); do
  (
    output="$SCENARIO_ROOT/child-$index.json"
    error="$SCENARIO_ROOT/child-$index.err"
    if env TALLY_JOB_ID="$parent_uuid" "$TALLY_BIN" \
      --socket "$SCENARIO_SOCKET" \
      enqueue \
      --pool slot \
      --dedup-key "scenario-child-$index" \
      -- /bin/false \
      >"$output" 2>"$error"; then
      printf '0\n' >"$SCENARIO_ROOT/child-$index.rc"
    else
      printf '%s\n' "$?" >"$SCENARIO_ROOT/child-$index.rc"
    fi
  ) &
  child_pids+=("$!")
done
for child_pid in "${child_pids[@]}"; do
  wait "$child_pid"
done

accepted=()
rejected=0
for ((index = 1; index <= child_count; index++)); do
  rc="$(<"$SCENARIO_ROOT/child-$index.rc")"
  case "$rc" in
    0)
      accepted+=("$(jq -er '.task_uuid' "$SCENARIO_ROOT/child-$index.json")")
      ;;
    2)
      grep -F 'fanoutCap' "$SCENARIO_ROOT/child-$index.err" >/dev/null \
        || scenario_fail "child $index was rejected for the wrong reason"
      ((rejected += 1))
      ;;
    *)
      sed -n '1,80p' "$SCENARIO_ROOT/child-$index.err" >&2 || true
      scenario_fail "child $index returned unexpected exit code $rc"
      ;;
  esac
done

[[ "${#accepted[@]}" -eq 3 ]] \
  || scenario_fail "fanoutCap=3 admitted ${#accepted[@]} of $child_count children"
[[ "$rejected" -eq 5 ]] \
  || scenario_fail "fanoutCap=3 rejected $rejected of $child_count children"

set +e
env TALLY_JOB_ID="${accepted[0]}" "$TALLY_BIN" \
  --socket "$SCENARIO_SOCKET" \
  enqueue \
  --pool slot \
  --dedup-key scenario-grandchild \
  -- /bin/false \
  >"$SCENARIO_ROOT/grandchild.json" 2>"$SCENARIO_ROOT/grandchild.err"
grandchild_rc=$?
set -e
[[ "$grandchild_rc" -eq 2 ]] \
  || scenario_fail "depthCap=1 grandchild returned exit code $grandchild_rc instead of 2"
grep -F 'depthCap' "$SCENARIO_ROOT/grandchild.err" >/dev/null \
  || scenario_fail "grandchild was rejected for the wrong reason"

status="$($TALLY_BIN --socket "$SCENARIO_SOCKET" query status --pool slot)"
accepted_json="$(printf '%s\n' "${accepted[@]}" | jq -R . | jq -s .)"
jq -e --arg parent "$parent_uuid" --argjson children "$accepted_json" '
  [.jobs[].taskUuid] as $seen |
  ($seen | index($parent)) != null and
  ($children - $seen | length) == 0
' <<<"$status" >/dev/null \
  || scenario_fail "real-daemon status did not retain the parent and three accepted children"

printf 'PASS fanout-guardrail: parent=%s admitted=3 rejected=5 depthCap=1 fanoutCap=3\n' "$parent_uuid"
