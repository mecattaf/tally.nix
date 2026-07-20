#!/usr/bin/env bash

set -euo pipefail

source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

SCENARIO_NAME="slow-sqlite"
require_command jq
require_command sqlite3
require_command timeout
resolve_tally_bin
new_scenario_root slow-sqlite

locker_pid=""
lock_fd=""

release_sqlite_lock() {
  if [[ -n "$lock_fd" ]]; then
    printf 'ROLLBACK;\n.quit\n' >&"$lock_fd" 2>/dev/null || true
    exec {lock_fd}>&-
    lock_fd=""
  fi
  if [[ -n "$locker_pid" ]]; then
    wait "$locker_pid" 2>/dev/null || true
    locker_pid=""
  fi
}

cleanup() {
  release_sqlite_lock
  stop_daemon
  remove_scenario_root "$SCENARIO_ROOT"
}
trap cleanup EXIT INT TERM

config="$SCENARIO_ROOT/config.json"
jq -n '{
  pools: {
    slow: {
      resource: "budget",
      capacity: 1,
      predicate: {"windowed-consumption": {windowSec: 3600, consumptionCap: 1}},
      enforce: "cooperative"
    }
  },
  adapters: {shell: {}},
  journald: {native: false}
}' >"$config"

"$TALLY_BIN" --mode check-config --config "$config" >/dev/null
start_daemon "$config" "$SCENARIO_ROOT"

database="$SCENARIO_ROOT/data/taskdata/taskchampion.sqlite3"
[[ -f "$database" ]] || scenario_fail "TaskChampion SQLite database was not created"

control="$SCENARIO_ROOT/sqlite-control"
mkfifo "$control"
exec {lock_fd}<>"$control"
sqlite3 -batch -bail "$database" <"$control" >"$SCENARIO_ROOT/sqlite-lock.out" 2>"$SCENARIO_ROOT/sqlite-lock.err" &
locker_pid=$!
printf 'BEGIN IMMEDIATE;\n.print LOCKED\n' >&"$lock_fd"

for ((index = 0; index < 100; index++)); do
  if grep -Fx 'LOCKED' "$SCENARIO_ROOT/sqlite-lock.out" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$locker_pid" 2>/dev/null; then
    sed -n '1,80p' "$SCENARIO_ROOT/sqlite-lock.err" >&2 || true
    scenario_fail "external SQLite writer exited before acquiring its lock"
  fi
  sleep 0.02
done
grep -Fx 'LOCKED' "$SCENARIO_ROOT/sqlite-lock.out" >/dev/null 2>&1 \
  || scenario_fail "external SQLite writer did not confirm its transaction"

lock_observed=false
for ((index = 0; index < 100; index++)); do
  if ! sqlite3 -batch -bail "$database" 'BEGIN IMMEDIATE;' \
    >"$SCENARIO_ROOT/sqlite-probe.out" 2>"$SCENARIO_ROOT/sqlite-probe.err"; then
    if grep -F 'database is locked' "$SCENARIO_ROOT/sqlite-probe.err" >/dev/null; then
      lock_observed=true
      break
    fi
  fi
  sleep 0.02
done
if [[ "$lock_observed" != true ]]; then
  sed -n '1,80p' "$SCENARIO_ROOT/sqlite-lock.err" >&2 || true
  sed -n '1,80p' "$SCENARIO_ROOT/sqlite-probe.err" >&2 || true
  scenario_fail "external SQLite writer lock was not acquired"
fi

admitted="$(timeout 5 "$TALLY_BIN" \
  --socket "$SCENARIO_SOCKET" \
  enqueue \
  --pool slow \
  --dedup-key scenario-slow-sqlite \
  --consumption-estimate 2 \
  -- /bin/false)"
task_uuid="$(jq -er '.task_uuid' <<<"$admitted")"

# The writer is blocked, but a new connection and RPC still complete.
pools="$(timeout 2 "$TALLY_BIN" --socket "$SCENARIO_SOCKET" query pools)"
jq -e '.pools[] | select(.pool == "slow")' <<<"$pools" >/dev/null \
  || scenario_fail "socket response omitted the configured pool while SQLite was locked"

event_count="$(grep -rl -- "$task_uuid" "$SCENARIO_ROOT/state/events" | wc -l)"
[[ "$event_count" -eq 1 ]] || scenario_fail "durable acknowledgement event is missing"
row_count="$(sqlite3 "$database" "SELECT count(*) FROM tasks WHERE uuid = '$task_uuid';")"
[[ "$row_count" -eq 0 ]] \
  || scenario_fail "row reached SQLite before the injected ack-to-commit crash"

# Crash the real daemon while its real TaskChampion writer is blocked.
kill -KILL "$DAEMON_PID"
wait "$DAEMON_PID" 2>/dev/null || true
DAEMON_PID=""
export DAEMON_PID
release_sqlite_lock

row_count="$(sqlite3 "$database" "SELECT count(*) FROM tasks WHERE uuid = '$task_uuid';")"
[[ "$row_count" -eq 0 ]] || scenario_fail "crash did not lose the post-ack SQLite row"

start_daemon "$config" "$SCENARIO_ROOT"
row_count="$(sqlite3 "$database" "SELECT count(*) FROM tasks WHERE uuid = '$task_uuid';")"
[[ "$row_count" -eq 1 ]] || scenario_fail "restart did not rebuild the exact durable row"
status="$($TALLY_BIN --socket "$SCENARIO_SOCKET" query status --pool slow)"
jq -e --arg uuid "$task_uuid" 'any(.jobs[]; .taskUuid == $uuid and .state == "queued")' \
  <<<"$status" >/dev/null \
  || scenario_fail "rebuilt row is not queryable as the same queued task"

printf 'PASS slow-sqlite: task=%s socket-responsive=true pre-restart-row=0 rebuilt-row=1\n' "$task_uuid"
