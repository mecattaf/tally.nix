#!/usr/bin/env bash

set -euo pipefail

readonly gate_context="fleet/gate-ladder"
readonly default_repo="mecattaf/tally.nix"

fail() {
  printf 'fleet gate poller: %s\n' "$*" >&2
  exit 2
}

load_github_token() {
  if [[ -n "${GH_TOKEN:-}" ]]; then
    return
  fi

  local token_file="${TALLY_GATE_TOKEN_FILE:-${XDG_CONFIG_HOME:-$HOME/.config}/tally-fleet-gate/github-token}"
  [[ -f "$token_file" ]] || fail "GitHub token file does not exist: $token_file"

  local token_mode
  token_mode="$(stat -c '%a' -- "$token_file")"
  [[ "$token_mode" == "600" ]] \
    || fail "GitHub token file must have mode 0600 (found $token_mode): $token_file"

  IFS= read -r GH_TOKEN <"$token_file"
  [[ -n "$GH_TOKEN" ]] || fail "GitHub token file is empty: $token_file"
  export GH_TOKEN
}

latest_gate_status() {
  local sha="$1"
  gh api "repos/$gate_repo/commits/$sha/statuses?per_page=100" \
    --jq "[.[] | select(.context == \"$gate_context\")] | first // {}"
}

status_needs_gate() {
  local status_json="$1"
  local state created_at created_epoch now_epoch
  state="$(jq -r '.state // "missing"' <<<"$status_json")"
  case "$state" in
    missing)
      return 0
      ;;
    pending)
      created_at="$(jq -r '.created_at // empty' <<<"$status_json")"
      [[ -n "$created_at" ]] || return 0
      created_epoch="$(date --date="$created_at" '+%s')"
      now_epoch="$(date '+%s')"
      ((now_epoch - created_epoch >= pending_max_age))
      return
      ;;
    success | failure | error)
      return 1
      ;;
    *)
      printf 'fleet gate poller: unknown status state %q; re-running gate\n' "$state" >&2
      return 0
      ;;
  esac
}

gate_if_needed() {
  local sha="$1"
  local source="$2"
  local status_json state
  status_json="$(latest_gate_status "$sha")"
  state="$(jq -r '.state // "missing"' <<<"$status_json")"
  if ! status_needs_gate "$status_json"; then
    printf 'fleet gate poller: skip %s %s (status=%s)\n' "$source" "$sha" "$state"
    return
  fi

  printf 'fleet gate poller: gate %s %s (previous status=%s)\n' "$source" "$sha" "$state"
  if ! "$gate_runner" "$sha"; then
    printf 'fleet gate poller: gate did not pass for %s %s\n' "$source" "$sha" >&2
  fi
}

for required in date flock gh jq stat; do
  command -v "$required" >/dev/null 2>&1 || fail "required command is unavailable: $required"
done

gate_repo="${TALLY_GATE_REPO:-$default_repo}"
[[ "$gate_repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid GitHub repository: $gate_repo"
gate_runner="${TALLY_GATE_RUNNER:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/fleet-gate.sh}"
[[ -x "$gate_runner" ]] || fail "gate runner is not executable: $gate_runner"
pending_max_age="${TALLY_GATE_PENDING_MAX_AGE_SEC:-7200}"
[[ "$pending_max_age" =~ ^[0-9]+$ ]] || fail "pending max age must be seconds: $pending_max_age"
lock_file="${TALLY_GATE_LOCK_FILE:-${XDG_RUNTIME_DIR:-/tmp}/tally-fleet-gate.lock}"

exec 9>"$lock_file"
if ! flock --nonblock 9; then
  printf 'fleet gate poller: another poll is active\n'
  exit 0
fi

load_github_token

while IFS=$'\t' read -r number sha; do
  [[ -n "$sha" ]] || continue
  gate_if_needed "$sha" "PR #$number"
done < <(
  gh pr list --repo "$gate_repo" --state open --limit 100 \
    --json number,headRefOid \
    --jq '.[] | [.number, .headRefOid] | @tsv'
)

main_sha="$(gh api "repos/$gate_repo/commits/main" --jq .sha)"
gate_if_needed "$main_sha" main

unset GH_TOKEN
