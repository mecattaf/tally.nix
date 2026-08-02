#!/usr/bin/env bash
# Squash-fidelity spike for git-ai authorship notes.
#
# AUGUST-01-DESIGN.md §7 states that git-ai notes "do not survive squash-merge",
# and §9.3.2 rules that the first step of arming git-ai is the empirical check of
# what actually survives: per-line attribution on the minted squash commit, or a
# summary only. This script is that check. It is evidence, not a gate: it needs
# the externally provisioned `git-ai` binary that the flake deliberately does not
# package, so it skips with exit 0 when the binary is absent.
#
# Four scenarios, each a distinct way a campaign can reach a squash commit:
#
#   local-squash    the squash happens in the same clone that holds the working
#                   branch's notes (the `forge = "local"` merge node)
#   remote-squash   the squash happens elsewhere and is fetched (the GitHub
#                   `gh pr merge --squash` merge node), with the noted working
#                   commits still present locally
#   pruned-source   the same, after the working branch and its notes are gone
#   fresh-clone     a clone that fetches only the base branch and refs/notes/ai
#
# Each scenario records whether a note exists on the squash commit once the
# background service has been flushed with `git-ai await` but before any git-ai
# read command runs, whether one exists after such a read, and whether the
# recovered attribution is per-line or summary-only.
#
# Usage: test/git-ai-squash-fidelity.sh [--out DIR]
#
# Writes findings.json plus a human-readable transcript to DIR (default: a fresh
# temporary directory whose path is printed).
set -euo pipefail

out_dir=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out)
      [ "$#" -ge 2 ] || { echo "--out requires a directory" >&2; exit 2; }
      out_dir="$2"
      shift 2
      ;;
    -h | --help)
      sed -n '2,27p' "$0"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if ! command -v git-ai >/dev/null 2>&1; then
  echo "SKIP: git-ai is not on PATH; this spike needs the externally provisioned binary"
  exit 0
fi

scratch="$(mktemp -d "${TMPDIR:-/tmp}/git-ai-squash-fidelity.XXXXXX")"
if [ -z "$out_dir" ]; then
  out_dir="$scratch/findings"
fi
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
transcript="$out_dir/transcript.txt"
: > "$transcript"

cleanup() {
  # The background service is per-HOME; shutting it down before the scratch HOME
  # disappears is what keeps the spike from leaking a daemon.
  git-ai bg shutdown >/dev/null 2>&1 || true
  rm -rf "$scratch"
}
trap cleanup EXIT

# The spike must not touch the operator's git-ai state: hooks, credentials, and
# the background service's bookkeeping all live under HOME.
export HOME="$scratch/home"
export GIT_CONFIG_GLOBAL="$scratch/home/.gitconfig"
export GIT_CONFIG_SYSTEM=/dev/null
mkdir -p "$HOME"

# git-ai binds authorship through a global `trace2.eventTarget` pointing at its
# background service, not through repository hooks. Without this the daemon never
# observes `git commit` and no note is ever minted — which is itself worth
# recording: an unarmed host produces the same evidence as a squash that lost its
# attribution, so §7's binding must not infer one from the other.
git-ai install-hooks >/dev/null 2>&1 || true

git_ai_version="$(git-ai --version 2>&1 | tr -d '\r')"

say() {
  printf '%s\n' "$*" | tee -a "$transcript"
}

record() {
  printf '%s\n' "$*" >> "$transcript"
}

run_git() {
  git -C "$1" "${@:2}"
}

seed_repo() {
  # A working branch with three commits: an AI line, a human line, and a second
  # AI line from a second session. The mixture is the point — a summary-only
  # survival collapses it, per-line survival keeps the human line unattributed.
  local repo="$1"
  git init -q --initial-branch=main "$repo"
  run_git "$repo" config user.name "Tally Spike"
  run_git "$repo" config user.email "tally-spike@invalid"
  printf 'line1\nline2\nline3\n' > "$repo/a.txt"
  run_git "$repo" add a.txt
  run_git "$repo" commit -qm "chore: seed"
  run_git "$repo" switch -qc work
  printf 'line1\nAI-A\nline2\nline3\n' > "$repo/a.txt"
  (cd "$repo" && git-ai checkpoint mock_ai a.txt >/dev/null 2>&1)
  run_git "$repo" add a.txt
  run_git "$repo" commit -qm "feat: first assisted edit"
  printf 'line1\nAI-A\nline2\nHUMAN-B\nline3\n' > "$repo/a.txt"
  run_git "$repo" add a.txt
  run_git "$repo" commit -qm "chore: unassisted edit"
  printf 'line1\nAI-A\nline2\nHUMAN-B\nAI-C\nline3\n' > "$repo/a.txt"
  (cd "$repo" && git-ai checkpoint mock_ai a.txt >/dev/null 2>&1)
  run_git "$repo" add a.txt
  run_git "$repo" commit -qm "feat: second assisted edit"
  (cd "$repo" && git-ai await --timeout 30 >/dev/null 2>&1 || true)
  run_git "$repo" switch -q main
}

has_note() {
  run_git "$1" notes --ref=ai show "$2" >/dev/null 2>&1
}

# The note body is `<file>\n  <session>::<turn> <line>...` followed by `---` and
# a JSON session table. Line numbers in the first section are the per-line
# evidence; their absence with a session table present is summary-only.
attribution_shape() {
  local repo="$1" commit="$2" body
  if ! body="$(run_git "$repo" notes --ref=ai show "$commit" 2>/dev/null)"; then
    echo "none"
    return
  fi
  if printf '%s' "$body" | sed -n '/^---$/q;p' | grep -Eq '::[A-Za-z0-9_]+ [0-9]+'; then
    echo "per-line"
  elif printf '%s' "$body" | grep -q 'schema_version'; then
    echo "summary-only"
  else
    echo "unrecognized"
  fi
}

attributed_lines() {
  run_git "$1" notes --ref=ai show "$2" 2>/dev/null \
    | sed -n '/^---$/q;p' \
    | grep -Eo '::[A-Za-z0-9_]+ [0-9]+' \
    | awk '{print $2}' \
    | sort -n \
    | tr '\n' ',' \
    | sed 's/,$//'
}

scenario_rows=""
add_row() {
  # name, note-after-await, note-after-read, shape, attributed lines
  scenario_rows="$scenario_rows$1|$2|$3|$4|$5
"
  say "  $1: noteAfterAwait=$2 noteAfterRead=$3 shape=$4 lines=[$5]"
}

say "git-ai $git_ai_version"
say "scratch $scratch"
say ""

##############################################################################
say "scenario local-squash — squash performed in the clone that holds the notes"
##############################################################################
local_repo="$scratch/local/repo"
mkdir -p "$scratch/local"
seed_repo "$local_repo"
record "--- pre-squash git-ai blame (working branch) ---"
(cd "$local_repo" && git switch -q work && git-ai blame a.txt) >> "$transcript" 2>&1
run_git "$local_repo" switch -q main
run_git "$local_repo" merge --squash work >/dev/null
run_git "$local_repo" commit -qm "feat: squashed task

Assisted-by: mock_ai:unknown (tally:spike witness:0)"
local_squash="$(run_git "$local_repo" rev-parse HEAD)"
(cd "$local_repo" && git-ai await --timeout 30 >/dev/null 2>&1 || true)
before="no"; has_note "$local_repo" "$local_squash" && before="yes"
(cd "$local_repo" && git-ai stats "$local_squash" --json >/dev/null 2>&1 || true)
(cd "$local_repo" && git-ai await --timeout 30 >/dev/null 2>&1 || true)
after="no"; has_note "$local_repo" "$local_squash" && after="yes"
record "--- squash-commit note (local-squash) ---"
run_git "$local_repo" notes --ref=ai show "$local_squash" >> "$transcript" 2>&1 || true
record "--- post-squash git-ai blame (base branch) ---"
(cd "$local_repo" && git-ai blame a.txt) >> "$transcript" 2>&1
add_row "local-squash" "$before" "$after" \
  "$(attribution_shape "$local_repo" "$local_squash")" \
  "$(attributed_lines "$local_repo" "$local_squash")"

##############################################################################
say "scenario remote-squash — squash performed elsewhere, then fetched"
##############################################################################
remote="$scratch/remote/origin.git"
noted="$scratch/remote/noted"
forge="$scratch/remote/forge"
mkdir -p "$scratch/remote"
git init -q --bare --initial-branch=main "$remote"
seed_repo "$noted"
run_git "$noted" remote add origin "$remote"
run_git "$noted" push -q origin main work
# The forge clone stands in for GitHub's server-side squash: it has the commits
# and none of the notes, exactly like `gh pr merge --squash`.
git clone -q "$remote" "$forge"
run_git "$forge" config user.name "Tally Forge"
run_git "$forge" config user.email "tally-forge@invalid"
run_git "$forge" switch -q main
run_git "$forge" merge --squash origin/work >/dev/null
run_git "$forge" commit -qm "feat: squashed task

Assisted-by: mock_ai:unknown (tally:spike witness:0)"
run_git "$forge" push -q origin main
remote_squash="$(run_git "$forge" rev-parse HEAD)"
run_git "$noted" fetch -q origin
run_git "$noted" switch -q main
run_git "$noted" reset -q --hard origin/main
(cd "$noted" && git-ai await --timeout 30 >/dev/null 2>&1 || true)
before="no"; has_note "$noted" "$remote_squash" && before="yes"
(cd "$noted" && git-ai stats "$remote_squash" --json >/dev/null 2>&1 || true)
(cd "$noted" && git-ai await --timeout 30 >/dev/null 2>&1 || true)
after="no"; has_note "$noted" "$remote_squash" && after="yes"
record "--- squash-commit note (remote-squash) ---"
run_git "$noted" notes --ref=ai show "$remote_squash" >> "$transcript" 2>&1 || true
record "--- git-ai stats (remote-squash) ---"
(cd "$noted" && git-ai stats "$remote_squash" --json) >> "$transcript" 2>&1 || true
add_row "remote-squash" "$before" "$after" \
  "$(attribution_shape "$noted" "$remote_squash")" \
  "$(attributed_lines "$noted" "$remote_squash")"

##############################################################################
say "scenario pruned-source — working branch and its notes deleted first"
##############################################################################
pruned="$scratch/pruned/noted"
pruned_remote="$scratch/pruned/origin.git"
pruned_forge="$scratch/pruned/forge"
mkdir -p "$scratch/pruned"
git init -q --bare --initial-branch=main "$pruned_remote"
seed_repo "$pruned"
run_git "$pruned" remote add origin "$pruned_remote"
run_git "$pruned" push -q origin main work
git clone -q "$pruned_remote" "$pruned_forge"
run_git "$pruned_forge" config user.name "Tally Forge"
run_git "$pruned_forge" config user.email "tally-forge@invalid"
run_git "$pruned_forge" switch -q main
run_git "$pruned_forge" merge --squash origin/work >/dev/null
run_git "$pruned_forge" commit -qm "feat: squashed task"
run_git "$pruned_forge" push -q origin main
run_git "$pruned_forge" push -q origin --delete work
pruned_squash="$(run_git "$pruned_forge" rev-parse HEAD)"
# Delete every trace of the working branch's noted commits before fetching the
# squash: the branch, its notes, the reflog, and the unreachable objects.
run_git "$pruned" branch -qD work
run_git "$pruned" update-ref -d refs/notes/ai
run_git "$pruned" reflog expire --expire=now --all
run_git "$pruned" gc --prune=now --quiet 2>/dev/null || true
run_git "$pruned" fetch -q --prune origin
run_git "$pruned" switch -q main
run_git "$pruned" reset -q --hard origin/main
(cd "$pruned" && git-ai await --timeout 30 >/dev/null 2>&1 || true)
before="no"; has_note "$pruned" "$pruned_squash" && before="yes"
(cd "$pruned" && git-ai stats "$pruned_squash" --json >/dev/null 2>&1 || true)
(cd "$pruned" && git-ai await --timeout 30 >/dev/null 2>&1 || true)
after="no"; has_note "$pruned" "$pruned_squash" && after="yes"
record "--- git-ai stats (pruned-source) ---"
(cd "$pruned" && git-ai stats "$pruned_squash" --json) >> "$transcript" 2>&1 || true
record "--- git-ai blame (pruned-source) ---"
(cd "$pruned" && git-ai blame a.txt) >> "$transcript" 2>&1 || true
add_row "pruned-source" "$before" "$after" \
  "$(attribution_shape "$pruned" "$pruned_squash")" \
  "$(attributed_lines "$pruned" "$pruned_squash")"

##############################################################################
say "scenario fresh-clone — base branch plus refs/notes/ai, nothing else"
##############################################################################
# Publish the local-squash repository's notes and base branch, then clone only
# those. This is what a reviewer or a later `tally authorship verify` sees.
publish="$scratch/fresh/origin.git"
fresh="$scratch/fresh/clone"
mkdir -p "$scratch/fresh"
git init -q --bare --initial-branch=main "$publish"
run_git "$local_repo" remote add publish "$publish"
run_git "$local_repo" push -q publish main
if run_git "$local_repo" rev-parse --verify --quiet refs/notes/ai >/dev/null; then
  run_git "$local_repo" push -q publish "refs/notes/ai:refs/notes/ai"
fi
git clone -q "$publish" "$fresh"
run_git "$fresh" fetch -q origin "refs/notes/ai:refs/notes/ai" 2>/dev/null || true
before="no"; has_note "$fresh" "$local_squash" && before="yes"
(cd "$fresh" && git-ai stats "$local_squash" --json >/dev/null 2>&1 || true)
after="no"; has_note "$fresh" "$local_squash" && after="yes"
record "--- git-ai blame (fresh-clone) ---"
(cd "$fresh" && git-ai blame a.txt) >> "$transcript" 2>&1 || true
add_row "fresh-clone" "$before" "$after" \
  "$(attribution_shape "$fresh" "$local_squash")" \
  "$(attributed_lines "$fresh" "$local_squash")"

say ""
say "transcript $transcript"

{
  printf '{\n'
  printf '  "gitAiVersion": "%s",\n' "$git_ai_version"
  printf '  "scenarios": [\n'
  first=1
  while IFS='|' read -r name before after shape lines; do
    [ -n "$name" ] || continue
    [ "$first" = 1 ] || printf ',\n'
    first=0
    printf '    {"name": "%s", "noteAfterAwait": "%s", "noteAfterRead": "%s", "attribution": "%s", "attributedLines": "%s"}' \
      "$name" "$before" "$after" "$shape" "$lines"
  done <<EOF
$scenario_rows
EOF
  printf '\n  ]\n'
  printf '}\n'
} > "$out_dir/findings.json"

say "findings $out_dir/findings.json"
