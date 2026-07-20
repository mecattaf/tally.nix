#!/usr/bin/env bash

set -euo pipefail

scenario_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$scenario_dir/../.." && pwd)"

scenario_fail() {
  printf 'FAIL %s: %s\n' "${SCENARIO_NAME:-scenario}" "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || scenario_fail "required command is unavailable: $1"
}

resolve_tally_bin() {
  if [[ -n "${TALLY_BIN:-}" ]]; then
    [[ -x "$TALLY_BIN" ]] || scenario_fail "TALLY_BIN is not executable: $TALLY_BIN"
    return
  fi
  require_command cargo
  cargo build --quiet --manifest-path "$repo_root/Cargo.toml" --bin tally
  TALLY_BIN="$repo_root/target/debug/tally"
  export TALLY_BIN
}

new_scenario_root() {
  local name="$1"
  local base="${TALLY_SCENARIO_TMPDIR:-${TMPDIR:-/tmp}}"
  SCENARIO_ROOT="$(mktemp -d "$base/tally-scenario-${name}.XXXXXX")"
  chmod 0700 "$SCENARIO_ROOT"
  export SCENARIO_ROOT
}

wait_for_daemon() {
  local attempts=200
  local output
  for ((index = 0; index < attempts; index++)); do
    if [[ -S "$SCENARIO_SOCKET" ]] && output="$($TALLY_BIN --socket "$SCENARIO_SOCKET" query pools 2>/dev/null)"; then
      jq -e '.protocolVersion == 1' <<<"$output" >/dev/null
      return 0
    fi
    if [[ -n "${DAEMON_PID:-}" ]] && ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      wait "$DAEMON_PID" 2>/dev/null || true
      printf '%s\n' 'daemon exited before its socket became ready:' >&2
      sed -n '1,200p' "$DAEMON_LOG" >&2 || true
      return 1
    fi
    sleep 0.05
  done
  printf '%s\n' 'daemon did not become ready:' >&2
  sed -n '1,200p' "$DAEMON_LOG" >&2 || true
  return 1
}

start_daemon() {
  local config="$1"
  local root="$2"
  mkdir -p "$root/run"
  SCENARIO_SOCKET="$root/run/tally.sock"
  DAEMON_LOG="$root/daemon.log"
  export SCENARIO_SOCKET DAEMON_LOG
  "$TALLY_BIN" \
    --config "$config" \
    --socket "$SCENARIO_SOCKET" \
    daemon run \
    --cpu-weight 100 \
    --memory-max-bytes 67108864 \
    --state-dir "$root/state" \
    --data-dir "$root/data" \
    --yield-grace-sec 1 \
    >"$DAEMON_LOG" 2>&1 &
  DAEMON_PID=$!
  export DAEMON_PID
  wait_for_daemon || scenario_fail "real daemon failed to start"
}

stop_daemon() {
  local pid="${DAEMON_PID:-}"
  [[ -n "$pid" ]] || return 0
  if kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null || true
    for ((index = 0; index < 200; index++)); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.05
    done
  fi
  if kill -0 "$pid" 2>/dev/null; then
    kill -KILL "$pid" 2>/dev/null || true
  fi
  wait "$pid" 2>/dev/null || true
  DAEMON_PID=""
  export DAEMON_PID
}

remove_scenario_root() {
  local root="${1:-}"
  local temporary_base="${TMPDIR:-/tmp}"
  [[ -n "$root" ]] || return 0
  case "$root" in
    /tmp/tally-scenario-* | /var/tmp/tally-scenario-* | "$temporary_base"/tally-scenario-*)
      rm -rf -- "$root"
      ;;
    *)
      printf 'refusing to remove unexpected scenario root: %s\n' "$root" >&2
      return 1
      ;;
  esac
}
