#!/usr/bin/env bash

set -euo pipefail

readonly default_repo="mecattaf/tally.nix"
readonly default_remote="https://github.com/mecattaf/tally.nix.git"

fail() {
  printf 'fleet gate: %s\n' "$*" >&2
  exit 2
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command is unavailable: $1"
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

  # A campaign integration head may exist only in the checkout invoking this
  # gate. Import that exact committed object without making the disposable
  # worktree depend on any uncommitted state in the checkout.
  if ! git --git-dir="$pristine_clone" cat-file -e "$gate_sha^{commit}" 2>/dev/null; then
    if [[ -n "$gate_local_repo" ]] \
      && git -C "$gate_local_repo" cat-file -e "$gate_sha^{commit}" 2>/dev/null; then
      git --git-dir="$pristine_clone" fetch --quiet "$gate_local_repo" "$gate_sha"
    fi
  fi
  git --git-dir="$pristine_clone" cat-file -e "$gate_sha^{commit}" \
    || fail "commit is neither fetchable from $gate_remote nor present in the invoking repository: $gate_sha"
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
  run_step "cargo deny check (pinned and offline)" \
    nix develop --command test/cargo-deny.sh
}

# Resolved exactly once, before the runner lock is taken and before any stage
# runs. The main-audit decision is time-of-check dependent: a merge landing
# while this run waits for the lock would move the tip of main away from the
# audited SHA. Pinning the decision to the SHA's status at script start keeps
# the verdict a property of the audited commit rather than of when the runner
# became available.
resolve_pull_request_metadata() {
  local pull_json main_sha
  if ! pull_json="$(
    gh api "repos/$gate_repo/commits/$gate_sha/pulls?per_page=100" \
      --jq "[.[] | select(.state == \"open\" and .head.sha == \"$gate_sha\")][0] // {}" \
      2>/dev/null
  )"; then
    jq -e '.status == 422 or .status == "422"' <<<"$pull_json" >/dev/null 2>&1 \
      || fail "cannot list pull requests for $gate_sha in $gate_repo (is the commit pushed?)"
    pull_json="{}"
  fi
  gate_pr_number="$(jq -r '.number // empty' <<<"$pull_json")"
  gate_base_sha="$(jq -r '.base.sha // empty' <<<"$pull_json")"
  gate_no_changelog_label="$(
    jq -r 'any(.labels[]?; .name == "no-changelog")' <<<"$pull_json"
  )"
  gate_is_main_audit=false
  gate_is_local_audit=false
  gate_changelog_subject="local audit: local head; no pull request"

  if [[ -n "$gate_pr_number" ]]; then
    gate_changelog_subject="head of open pull request #$gate_pr_number"
    return
  fi

  main_sha="$(gh api "repos/$gate_repo/commits/main" --jq .sha)" \
    || fail "cannot resolve the tip of main in $gate_repo"
  if [[ "$main_sha" == "$gate_sha" ]]; then
    gate_is_main_audit=true
    gate_changelog_subject="tip of main"
    return
  fi

  gate_is_local_audit=true
}

run_changelog_stage() {
  printf '\n==> changelog policy\n'
  if [[ "$gate_is_local_audit" == true ]]; then
    printf 'PASS local audit: the changelog policy is a pull-request policy; no pull request is required for this local head\n'
    return
  fi
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
  cd "$worktree"

  local -a timeout_scale_env
  local timeout_scale_note
  if [[ -n "$gate_timeout_scale" ]]; then
    timeout_scale_env=("TALLY_TEST_TIMEOUT_SCALE=$gate_timeout_scale")
    timeout_scale_note="$gate_timeout_scale (TALLY_GATE_TIMEOUT_SCALE; wait budgets deliberately widened)"
  else
    timeout_scale_env=(-u TALLY_TEST_TIMEOUT_SCALE)
    timeout_scale_note='1 (unscaled; any ambient TALLY_TEST_TIMEOUT_SCALE is scrubbed)'
  fi

  printf 'tally fleet gate transcript\n'
  printf 'repository: %s\n' "$gate_repo"
  printf 'commit: %s\n' "$gate_sha"
  printf 'host: %s\n' "$(hostname -f)"
  printf 'started-at: %s\n' "$(date --utc '+%Y-%m-%dT%H:%M:%SZ')"
  printf 'worktree-head: %s\n' "$(git rev-parse HEAD)"
  printf 'changelog-subject: %s (pinned at script start)\n' "$gate_changelog_subject"
  printf 'timeout-scale: %s\n' "$timeout_scale_note"

  run_changelog_stage

  run_step "cargo fmt" nix develop --command cargo fmt --all --check
  run_step "cargo test" nix develop --command env -u TALLY_TEST_REMOTE_HOST \
    "${timeout_scale_env[@]}" cargo test --workspace
  run_step "cargo clippy" \
    nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings
  run_cargo_deny_stage
  run_step "nix flake check" nix flake check -L --keep-going
  run_step "final-bar" nix run .#final-conformance-bar -- "$worktree"

  printf '\n==> evaluated VM check inventory\n'
  run_flow_multi_host_assertion
  run_no_stubs_check
  run_no_workflows_check

  [[ -z "$(git status --porcelain --untracked-files=no)" ]] \
    || {
      printf 'FAIL ladder changed tracked worktree files:\n' >&2
      git status --short >&2
      return 1
    }
  printf '\nPASS fleet gate ladder for %s\n' "$gate_sha"
  printf 'finished-at: %s\n' "$(date --utc '+%Y-%m-%dT%H:%M:%SZ')"
}

[[ "$#" -eq 1 ]] || fail "usage: $0 <full-commit-sha>"
gate_sha="$1"
[[ "$gate_sha" =~ ^[0-9a-f]{40}$ ]] || fail "commit must be a full lowercase SHA-1: $gate_sha"

gate_repo="${TALLY_GATE_REPO:-$default_repo}"
[[ "$gate_repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid GitHub repository: $gate_repo"
gate_remote="${TALLY_GATE_REMOTE_URL:-$default_remote}"
state_dir="${TALLY_GATE_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/tally-fleet-gate}"
cache_dir="${TALLY_GATE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/tally-fleet-gate}"
pristine_clone="$cache_dir/pristine.git"

# Widening the suite's wait budgets is a deliberate act of this gate, never an
# inherited one. `nix develop` is not `--ignore-environment`, so an ambient
# TALLY_TEST_TIMEOUT_SCALE left over from a reproduce run would otherwise reach
# the ladder's own `cargo test` and make a 10x-budget run byte-indistinguishable
# from an honest one in the transcript. The ladder scrubs that variable and
# honours this one instead, and records the decision in the transcript header.
#
# The accepted range is the one `crates/tally/tests/support/timeout_scale.rs`
# enforces: [1, 1000]. The two validators are written in different languages and
# must refuse the same values — an earlier version applied the optional fraction
# to the `1000` alternative as well, so it blessed the whole interval
# (1000, 1001) and the ladder then died an hour later inside `cargo test`. This
# regex is deliberately the stricter of the two on exotic spellings that the
# Rust parser would accept (`1e3`, surrounding whitespace): refusing them here
# costs a retype at second zero, accepting one the Rust side then rejects costs
# a gate run.
gate_timeout_scale="${TALLY_GATE_TIMEOUT_SCALE:-}"
if [[ -n "$gate_timeout_scale" ]]; then
  [[ "$gate_timeout_scale" =~ ^([1-9][0-9]{0,2}(\.[0-9]+)?|1000(\.0+)?)$ ]] \
    || fail "TALLY_GATE_TIMEOUT_SCALE=\"$gate_timeout_scale\" must be a multiplier between 1 and 1000; the knob only widens budgets, so a value below 1 would tighten them, and a value above 1000 would overflow the scaled Duration"
fi

require_command date
require_command find
require_command flock
require_command git
require_command gh
require_command grep
require_command hostname
require_command jq
require_command nix
require_command tee

gate_local_repo="$(git rev-parse --show-toplevel 2>/dev/null || true)"

mkdir -p "$state_dir/transcripts"
chmod 0700 "$state_dir" "$state_dir/transcripts"
# Red evidence outlives the green re-run that follows it. A transcript named
# by the audited SHA alone is overwritten by the next run on that same head:
# witnessed at the eta C1 re-witness (2026-08-18, specs/eta/evidence/run-log.md),
# where the transcript holding the original failing check was replaced by the
# re-run that went green, and the only copy of the failure was gone. The run
# stamp is UTC seconds plus this shell's PID, so two runs of one head -- even
# two started in the same second -- write two files, and the PASS/FAIL line
# below names the exact one this run wrote.
gate_run_stamp="$(date --utc '+%Y%m%dT%H%M%SZ')-$$"
transcript="$state_dir/transcripts/$gate_sha-$gate_run_stamp.log"
gate_root="$(mktemp -d "${TMPDIR:-/tmp}/tally-fleet-gate.XXXXXX")"
chmod 0700 "$gate_root"
worktree="$gate_root/worktree"
trap remove_gate_root EXIT INT TERM

# Pin the pull-request classification before waiting on the runner lock, so
# that neither the wait nor the ladder itself can move it.
resolve_pull_request_metadata

mkdir -p "$cache_dir"
exec 8>"$cache_dir/runner.lock"
flock 8

set +e
(
  set -e
  prepare_pristine_clone
  create_disposable_worktree
  run_ladder
) 2>&1 | tee "$transcript"
ladder_status="${PIPESTATUS[0]}"
set -e

if [[ -d "$worktree" ]]; then
  git --git-dir="$pristine_clone" worktree remove --force "$worktree"
fi
git --git-dir="$pristine_clone" worktree prune

if [[ "$ladder_status" -eq 0 ]]; then
  printf 'fleet gate: PASS %s (transcript: %s)\n' "$gate_sha" "$transcript"
  exit 0
fi

printf 'fleet gate: FAIL %s (transcript: %s)\n' "$gate_sha" "$transcript" >&2
exit "$ladder_status"
