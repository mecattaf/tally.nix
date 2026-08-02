#!/usr/bin/env bash

set -euo pipefail

source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

SCENARIO_NAME="pool-vanished/return"
worker="${TALLY_TEST_REMOTE_HOST:-}"
if [[ -z "$worker" ]]; then
  printf '%s\n' \
    'SKIP pool-vanished/return: TALLY_TEST_REMOTE_HOST is unset; no remote host was selected'
  exit 0
fi
require_command jq
require_command nix
require_command ssh
require_command scp

ssh_options=(
  -o BatchMode=yes
  -o ConnectTimeout=3
  -o ConnectionAttempts=1
)

ssh_worker() {
  ssh "${ssh_options[@]}" "$worker" "$@"
}

worker_is_up() {
  ssh_worker true >/dev/null 2>&1
}

wait_for_worker_reboot() {
  local old_boot_id="$1"
  local went_down=false
  for ((index = 0; index < 90; index++)); do
    if ! worker_is_up; then
      went_down=true
      break
    fi
    sleep 1
  done
  [[ "$went_down" == true ]] || scenario_fail "$worker never became unreachable for reboot"

  for ((index = 0; index < 240; index++)); do
    if worker_is_up; then
      new_boot_id="$(ssh_worker cat /proc/sys/kernel/random/boot_id 2>/dev/null || true)"
      if [[ -n "$new_boot_id" && "$new_boot_id" != "$old_boot_id" ]]; then
        return 0
      fi
    fi
    sleep 1
  done
  scenario_fail "$worker did not return with a new boot ID"
}

TALLY_SCENARIO_TMPDIR=/var/tmp
new_scenario_root pool-return
root="$SCENARIO_ROOT"
wrapper_dir="$root/wrappers"
mkdir -p "$wrapper_dir"
cp "$scenario_dir/helpers/systemctl-worker" "$wrapper_dir/systemctl"
cp "$scenario_dir/helpers/systemd-run-worker" "$wrapper_dir/systemd-run"
chmod 0700 "$wrapper_dir/systemctl" "$wrapper_dir/systemd-run"

task_uuid=""

stop_worker_launchers() {
  local marker pid
  for marker in "$root"/worker-launch/tally-job-*.service; do
    [[ -f "$marker" ]] || continue
    pid="$(sed -n '1p' "$marker")"
    [[ "$pid" =~ ^[0-9]+$ ]] && kill -TERM "$pid" 2>/dev/null || true
  done
}

worker_cleanup() {
  [[ -n "$task_uuid" ]] && ssh_worker systemctl --user stop -- "tally-job-$task_uuid.service" >/dev/null 2>&1 || true
  case "$root" in
    /var/tmp/tally-scenario-pool-return.*)
      ssh_worker rm -rf -- "$root" >/dev/null 2>&1 || true
      ;;
  esac
}

cleanup() {
  stop_daemon
  stop_worker_launchers
  worker_cleanup
  remove_scenario_root "$root"
}
trap cleanup EXIT INT TERM

ssh_worker sudo -n true >/dev/null \
  || scenario_fail "$worker does not permit the explicitly required unattended reboot"
ssh_worker mkdir -p \
  "$root/state/capture" \
  "$root/state/unit-exit"

package="${TALLY_PACKAGE:-$(nix build --no-link --print-out-paths "$repo_root#tally")}"
[[ "$package" == /nix/store/* ]] || scenario_fail "live scenario requires a Nix store package"
nix copy --no-check-sigs --to "ssh-ng://$worker" "$package"
TALLY_BIN="$package/bin/tally"
export TALLY_BIN

agent="$root/resumable-agent"
worker_bash="$(ssh_worker command -v bash)"
worker_sleep="$(ssh_worker command -v sleep)"
cat >"$agent" <<'AGENT'
#!/bin/sh
set -euo pipefail
root="$(cd -- "$(dirname -- "$0")" && pwd)"
if [[ "${1:-}" == "--resume" ]]; then
  printf 'resumed\n' >"$root/resumed"
  exit 0
fi
printf 'started\n' >"$root/started"
exec sleep 600
AGENT
sed -i \
  -e "1c#!$worker_bash" \
  -e "s|exec sleep 600|exec $worker_sleep 600|" \
  "$agent"
chmod 0700 "$agent"
scp -q "${ssh_options[@]}" "$agent" "$worker:$agent"

config="$root/config.json"
jq -n --arg agent "$agent" '{
  pools: {
    worker: {
      resource: "build-slot",
      capacity: 1,
      enforce: "cooperative",
      autoResume: true
    }
  },
  adapters: {
    resumable: {
      argv: [$agent],
      resume: [$agent, "--resume"]
    }
  },
  producers: {
    health: {
      kind: "pool-reachability",
      probePool: "worker",
      hysteresis: 1
    }
  },
  journald: {native: false}
}' >"$config"
"$TALLY_BIN" --mode check-config --config "$config" >/dev/null

export TALLY_SCENARIO_ROOT="$root"
export PATH="$wrapper_dir:$PATH"
start_daemon "$config" "$root"

admitted="$($TALLY_BIN \
  --socket "$SCENARIO_SOCKET" \
  enqueue \
  --pool worker \
  --adapter resumable \
  --dedup-key scenario-worker-reboot \
  -- initial)"
task_uuid="$(jq -er '.task_uuid' <<<"$admitted")"

for ((index = 0; index < 200; index++)); do
  if ssh_worker test -f "$root/started" \
    && ssh_worker systemctl --user is-active --quiet "tally-job-$task_uuid.service"; then
    break
  fi
  sleep 0.05
done
ssh_worker systemctl --user is-active --quiet "tally-job-$task_uuid.service" \
  || scenario_fail "worker unit never became active"
launcher_marker="$root/worker-launch/tally-job-$task_uuid.service"
[[ -f "$launcher_marker" ]] \
  || scenario_fail "worker launcher did not pin the first-attempt reservation"

old_boot_id="$(ssh_worker cat /proc/sys/kernel/random/boot_id)"
set +e
ssh_worker sudo -n systemctl reboot >/dev/null 2>&1
set -e
wait_for_worker_reboot "$old_boot_id"
ssh_worker systemctl --user is-system-running >/dev/null \
  || scenario_fail "$worker user manager did not return"
ssh_worker systemctl --user show --property=LoadState --value "tally-job-$task_uuid.service" \
  | grep -Fx not-found >/dev/null \
  || scenario_fail "pre-reboot worker unit survived unexpectedly"
[[ -f "$launcher_marker" ]] \
  || scenario_fail "rebooted worker did not sever the live systemd-run transport"

lost="$($TALLY_BIN \
  --config "$config" \
  --socket "$SCENARIO_SOCKET" \
  __producer-dispatch health \
  --state-dir "$root/state" \
  --data-dir "$root/data" \
  --event '{"kind":"pool-reachability","reachable":false}')"
jq -e '.transition == "lost"' <<<"$lost" >/dev/null \
  || scenario_fail "worker reboot did not produce a confirmed lost transition"

vanished="$($TALLY_BIN --socket "$SCENARIO_SOCKET" queue await-job "$task_uuid")"
jq -e --arg uuid "$task_uuid" '
  .task_uuid == $uuid and .verdict == "pool-vanished" and .attempt == 1
' <<<"$vanished" >/dev/null \
  || scenario_fail "worker reboot did not produce the exact row pool-vanished verdict"

for ((index = 0; index < 100; index++)); do
  [[ ! -e "$launcher_marker" ]] && break
  sleep 0.02
done
[[ ! -e "$launcher_marker" ]] \
  || scenario_fail "daemon did not release the lost worker launch reservation"

returned="$($TALLY_BIN \
  --config "$config" \
  --socket "$SCENARIO_SOCKET" \
  __producer-dispatch health \
  --state-dir "$root/state" \
  --data-dir "$root/data" \
  --event '{"kind":"pool-reachability","reachable":true}')"
jq -e '.transition == "returned"' <<<"$returned" >/dev/null \
  || scenario_fail "worker return did not produce a confirmed returned transition"

terminal=""
for ((index = 0; index < 400; index++)); do
  terminal="$($TALLY_BIN --socket "$SCENARIO_SOCKET" queue await-job "$task_uuid" 2>/dev/null || true)"
  if jq -e '.verdict == "pass" and .attempt == 2' <<<"$terminal" >/dev/null 2>&1; then
    break
  fi
  sleep 0.05
done
jq -e --arg uuid "$task_uuid" '
  .task_uuid == $uuid and .verdict == "pass" and .attempt == 2
' <<<"$terminal" >/dev/null \
  || scenario_fail "returned worker did not complete the represented exact row"
ssh_worker grep -Fx resumed "$root/resumed" >/dev/null \
  || scenario_fail "attempt 2 did not use the configured resume argv"

jq -s -e --arg uuid "$task_uuid" '
  [ .[] | select(.task_uuid == $uuid) ] as $records |
  ($records | length) == 2 and
  $records[0].verdict == "pool-vanished" and
  $records[0].attempt == 1 and
  $records[1].verdict == "pass" and
  $records[1].attempt == 2 and
  $records[1].labor_class == "recovered"
' "$root/data/witness.jsonl" >/dev/null \
  || scenario_fail "witness chain does not prove pool loss and exact-row recovery"

printf 'PASS pool-vanished/return: worker=%s boot=%s task=%s verdicts=pool-vanished,pass attempts=1,2\n' \
  "$worker" "$old_boot_id->$new_boot_id" "$task_uuid"
