#!/usr/bin/env bash

set -euo pipefail

readonly gate_context="fleet/gate-ladder"
readonly default_repo="mecattaf/tally.nix"
readonly default_remote="https://github.com/mecattaf/tally.nix.git"

fail() {
  printf 'fleet gate: %s\n' "$*" >&2
  exit 2
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command is unavailable: $1"
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

github_api() {
  (
    load_github_token
    gh api "$@"
  )
}

post_status() {
  local sha="$1"
  local state="$2"
  local result_word="$3"
  local target_url="${4:-}"
  local timestamp host description
  timestamp="$(date --utc '+%Y-%m-%dT%H:%M:%SZ')"
  host="$(hostname -s)"
  description="ladder $result_word on $host at $timestamp"

  local -a arguments=(
    "repos/$gate_repo/statuses/$sha"
    -f "state=$state"
    -f "context=$gate_context"
    -f "description=$description"
  )
  if [[ -n "$target_url" ]]; then
    arguments+=(-f "target_url=$target_url")
  fi
  github_api "${arguments[@]}" >/dev/null
}

# Invoked indirectly by the EXIT/INT/TERM trap.
# shellcheck disable=SC2329
remove_gate_root() {
  local root="${gate_root:-}"
  [[ -n "$root" ]] || return 0
  case "$root" in
    /tmp/tally-fleet-gate.* | /var/tmp/tally-fleet-gate.* | "${TMPDIR:-/tmp}"/tally-fleet-gate.*)
      rm -rf -- "$root"
      ;;
    *)
      printf 'fleet gate: refusing to remove unexpected temporary root: %s\n' "$root" >&2
      return 1
      ;;
  esac
}

prepare_pristine_clone() {
  mkdir -p "$cache_dir"
  chmod 0700 "$cache_dir"

  if [[ ! -d "$pristine_clone" ]]; then
    git clone --bare --quiet "$gate_remote" "$pristine_clone"
  fi

  [[ -f "$pristine_clone/HEAD" ]] || fail "pristine clone is invalid: $pristine_clone"
  git --git-dir="$pristine_clone" remote set-url origin "$gate_remote"
  git --git-dir="$pristine_clone" fetch --quiet --force --prune origin \
    '+refs/heads/*:refs/remotes/origin/*' \
    '+refs/pull/*/head:refs/remotes/pull/*'
  git --git-dir="$pristine_clone" cat-file -e "$gate_sha^{commit}" \
    || fail "commit is not fetchable from $gate_remote: $gate_sha"
}

create_disposable_worktree() {
  git --git-dir="$pristine_clone" worktree add --quiet --detach "$worktree" "$gate_sha"

  local checked_out
  checked_out="$(git -C "$worktree" rev-parse HEAD)"
  [[ "$checked_out" == "$gate_sha" ]] \
    || fail "worktree resolved to $checked_out instead of $gate_sha"
  [[ -z "$(git -C "$worktree" status --porcelain)" ]] \
    || fail "disposable worktree is not clean before the ladder"
}

run_step() {
  local label="$1"
  shift
  printf '\n==> %s\n' "$label"
  printf '$'
  printf ' %q' "$@"
  printf '\n'
  "$@"
  printf 'PASS %s\n' "$label"
}

run_no_stubs_check() {
  printf '\n==> no Rust placeholders\n'
  printf '%s\n' "$ grep -rn 'todo!\\|unimplemented!\\|TODO' crates/"

  local grep_status
  set +e
  grep -rn 'todo!\|unimplemented!\|TODO' crates/
  grep_status=$?
  set -e
  case "$grep_status" in
    1)
      printf 'PASS no Rust placeholders (grep exited 1 with no matches)\n'
      ;;
    0)
      printf 'FAIL no Rust placeholders (matches are shown above)\n' >&2
      return 1
      ;;
    *)
      printf 'FAIL no Rust placeholders (grep exited %d)\n' "$grep_status" >&2
      return "$grep_status"
      ;;
  esac
}

run_no_workflows_check() {
  printf '\n==> no GitHub workflows\n'
  if [[ -e .github/workflows ]]; then
    printf 'FAIL .github/workflows must not exist\n' >&2
    find .github/workflows -maxdepth 2 -print >&2
    return 1
  fi

  local workflow_yaml=""
  if [[ -d .github ]]; then
    workflow_yaml="$(find .github -type f \( -name '*.yml' -o -name '*.yaml' \) -print)"
  fi
  if [[ -n "$workflow_yaml" ]]; then
    printf 'FAIL YAML files under .github are forbidden:\n%s\n' "$workflow_yaml" >&2
    return 1
  fi
  printf 'PASS no GitHub workflow directory or YAML files\n'
}

run_flow_multi_host_assertion() {
  local system checks
  system="$(nix eval --raw --impure --expr builtins.currentSystem)"
  checks="$(nix eval --json ".#checks.$system" --apply 'set: builtins.attrNames set')"
  jq -e 'index("flow-multi-host") != null' <<<"$checks" >/dev/null \
    || {
      printf 'FAIL checks.%s does not contain flow-multi-host\n' "$system" >&2
      return 1
    }
  printf 'PASS checks.%s contains flow-multi-host\n' "$system"
}

run_cargo_deny_stage() {
  printf '\n==> dependency policy\n'
  if [[ -f deny.toml ]] \
    && nix develop --command bash -c 'command -v cargo-deny >/dev/null 2>&1'; then
    run_step "cargo deny check" nix develop --command cargo deny check
    return
  fi
  printf 'NOT RUN cargo deny check: the pinned dependency policy has not landed yet\n'
}

resolve_pull_request_metadata() {
  local pull_json main_sha
  pull_json="$(
    github_api "repos/$gate_repo/commits/$gate_sha/pulls?per_page=100" \
      --jq "[.[] | select(.state == \"open\" and .head.sha == \"$gate_sha\")][0] // {}"
  )"
  gate_pr_number="$(jq -r '.number // empty' <<<"$pull_json")"
  gate_base_sha="$(jq -r '.base.sha // empty' <<<"$pull_json")"
  gate_no_changelog_label="$(
    jq -r 'any(.labels[]?; .name == "no-changelog")' <<<"$pull_json"
  )"
  gate_is_main_audit=false

  if [[ -z "$gate_pr_number" ]]; then
    main_sha="$(github_api "repos/$gate_repo/commits/main" --jq .sha)"
    if [[ "$main_sha" == "$gate_sha" ]]; then
      gate_is_main_audit=true
    fi
  fi
}

run_changelog_stage() {
  printf '\n==> changelog policy\n'
  if [[ ! -f CHANGELOG.md ]]; then
    printf 'NOT RUN changelog-touch rule: the changelog policy has not landed yet\n'
    return
  fi

  if [[ "$gate_is_main_audit" == true ]]; then
    printf 'PASS changelog policy was enforced on the pull-request head before this main audit\n'
    return
  fi
  if [[ -z "$gate_pr_number" ]] || [[ -z "$gate_base_sha" ]]; then
    printf 'FAIL CHANGELOG.md exists but no open pull request contains this head SHA\n' >&2
    return 1
  fi
  if [[ "$gate_no_changelog_label" == true ]]; then
    printf 'PASS PR #%s carries the no-changelog label\n' "$gate_pr_number"
    return
  fi
  git cat-file -e "$gate_base_sha^{commit}" \
    || {
      printf 'FAIL PR #%s base SHA is not present: %s\n' "$gate_pr_number" "$gate_base_sha" >&2
      return 1
    }
  if git diff --name-only "$gate_base_sha...$gate_sha" -- CHANGELOG.md | grep -Fx CHANGELOG.md \
    >/dev/null; then
    printf 'PASS PR #%s touches CHANGELOG.md\n' "$gate_pr_number"
    return
  fi

  printf 'FAIL PR #%s neither touches CHANGELOG.md nor carries no-changelog\n' \
    "$gate_pr_number" >&2
  return 1
}

run_ladder() {
  unset GH_TOKEN GITHUB_TOKEN
  cd "$worktree"

  printf 'tally fleet gate transcript\n'
  printf 'repository: %s\n' "$gate_repo"
  printf 'commit: %s\n' "$gate_sha"
  printf 'host: %s\n' "$(hostname -f)"
  printf 'started-at: %s\n' "$(date --utc '+%Y-%m-%dT%H:%M:%SZ')"
  printf 'worktree-head: %s\n' "$(git rev-parse HEAD)"

  run_step "cargo fmt" nix develop --command cargo fmt --all --check
  run_step "cargo test" nix develop --command env -u TALLY_TEST_REMOTE_HOST cargo test --workspace
  run_step "cargo clippy" \
    nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings
  run_cargo_deny_stage
  run_step "nix flake check" nix flake check -L

  printf '\n==> evaluated VM check inventory\n'
  run_flow_multi_host_assertion
  run_no_stubs_check
  run_no_workflows_check
  run_changelog_stage

  [[ -z "$(git status --porcelain --untracked-files=no)" ]] \
    || {
      printf 'FAIL ladder changed tracked worktree files:\n' >&2
      git status --short >&2
      return 1
    }
  printf '\nPASS fleet gate ladder for %s\n' "$gate_sha"
  printf 'finished-at: %s\n' "$(date --utc '+%Y-%m-%dT%H:%M:%SZ')"
}

publish_transcript() (
  unset GH_TOKEN GITHUB_TOKEN
  local publish_root="$gate_root/evidence"
  local evidence_remote="${TALLY_GATE_EVIDENCE_REMOTE:-$gate_remote}"
  local evidence_branch="gate-evidence"
  local evidence_file="$publish_root/$gate_sha.log"

  if git ls-remote --exit-code --heads "$evidence_remote" "refs/heads/$evidence_branch" \
    >/dev/null 2>&1; then
    git clone --quiet --single-branch --branch "$evidence_branch" "$evidence_remote" "$publish_root"
  else
    git init --quiet "$publish_root"
    git -C "$publish_root" remote add origin "$evidence_remote"
    git -C "$publish_root" switch --quiet --orphan "$evidence_branch"
  fi

  install -m 0644 "$transcript" "$evidence_file"
  git -C "$publish_root" add -- "$gate_sha.log"
  git -C "$publish_root" config user.name "tally fleet gate"
  git -C "$publish_root" config user.email "fleet-gate@users.noreply.github.com"
  git -C "$publish_root" commit --quiet -m "gate: record $gate_sha"

  git -C "$publish_root" push --quiet origin "HEAD:refs/heads/$evidence_branch"
)

[[ "$#" -eq 1 ]] || fail "usage: $0 <full-commit-sha>"
gate_sha="$1"
[[ "$gate_sha" =~ ^[0-9a-f]{40}$ ]] || fail "commit must be a full lowercase SHA-1: $gate_sha"

gate_repo="${TALLY_GATE_REPO:-$default_repo}"
[[ "$gate_repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid GitHub repository: $gate_repo"
gate_remote="${TALLY_GATE_REMOTE_URL:-$default_remote}"
state_dir="${TALLY_GATE_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/tally-fleet-gate}"
cache_dir="${TALLY_GATE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/tally-fleet-gate}"
pristine_clone="$cache_dir/pristine.git"
target_url="https://github.com/$gate_repo/blob/gate-evidence/$gate_sha.log"

require_command date
require_command find
require_command flock
require_command git
require_command gh
require_command grep
require_command hostname
require_command install
require_command jq
require_command nix
require_command stat
require_command tee

mkdir -p "$state_dir/transcripts"
chmod 0700 "$state_dir" "$state_dir/transcripts"
transcript="$state_dir/transcripts/$gate_sha.log"
gate_root="$(mktemp -d "${TMPDIR:-/tmp}/tally-fleet-gate.XXXXXX")"
chmod 0700 "$gate_root"
worktree="$gate_root/worktree"
trap remove_gate_root EXIT INT TERM

mkdir -p "$cache_dir"
exec 8>"$cache_dir/runner.lock"
flock 8

post_status "$gate_sha" pending pending "$target_url"

set +e
(
  set -e
  prepare_pristine_clone
  create_disposable_worktree
  resolve_pull_request_metadata
  run_ladder
) 2>&1 | tee "$transcript"
ladder_status="${PIPESTATUS[0]}"
set -e

if [[ -d "$worktree" ]]; then
  git --git-dir="$pristine_clone" worktree remove --force "$worktree"
fi
git --git-dir="$pristine_clone" worktree prune

set +e
publish_transcript >>"$transcript" 2>&1
publish_status=$?
set -e

if [[ "$publish_status" -ne 0 ]]; then
  post_status "$gate_sha" error error
  printf 'fleet gate: transcript publication failed; status posted as error\n' >&2
  exit 1
fi

if [[ "$ladder_status" -eq 0 ]]; then
  post_status "$gate_sha" success pass "$target_url"
  printf 'fleet gate: PASS %s (%s)\n' "$gate_sha" "$target_url"
  exit 0
fi

post_status "$gate_sha" failure fail "$target_url"
printf 'fleet gate: FAIL %s (%s)\n' "$gate_sha" "$target_url" >&2
exit 1
