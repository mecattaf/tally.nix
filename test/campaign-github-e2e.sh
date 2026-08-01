#!/usr/bin/env bash
set -euo pipefail

if [[ ${TALLY_CAMPAIGN_E2E_CONFIRM:-} != 1 ]]; then
  echo "set TALLY_CAMPAIGN_E2E_CONFIRM=1 to create a temporary private GitHub repository" >&2
  exit 2
fi

for program in gh git jq nix python3; do
  command -v "$program" >/dev/null || {
    echo "missing required program: $program" >&2
    exit 2
  }
done

root=$(git rev-parse --show-toplevel)
agent="$root/test/fixtures/campaign-github-e2e-agent.py"
[[ -f $agent ]] || {
  echo "missing campaign GitHub e2e agent: $agent" >&2
  exit 2
}
package=${TALLY_CAMPAIGN_E2E_PACKAGE:-}
if [[ -z $package ]]; then
  package=$(nix build --no-link --print-out-paths "$root#tally")
fi
tally="$package/bin/tally"
[[ -x $tally ]] || {
  echo "final package has no executable tally at $tally" >&2
  exit 2
}

config=${TALLY_CAMPAIGN_E2E_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/tally/config.json}
pool=${TALLY_CAMPAIGN_E2E_POOL:-campaign}
actor=$(gh api user --jq .login)
stamp=$(date -u +%Y%m%d%H%M%S)
repository="$actor/tally-campaign-e2e-$stamp-$$"
scratch=$(mktemp -d -t tally-campaign-github-e2e.XXXXXXXX)
daemon_mode=${TALLY_CAMPAIGN_E2E_DAEMON_MODE:-package}
case $daemon_mode in
package)
  socket=${TALLY_CAMPAIGN_E2E_SOCKET:-$scratch/tally.sock}
  ;;
host)
  socket=${TALLY_CAMPAIGN_E2E_SOCKET:-${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/tally/tally.sock}
  ;;
*)
  echo "TALLY_CAMPAIGN_E2E_DAEMON_MODE must be package or host" >&2
  exit 2
  ;;
esac
checkout="$scratch/checkout"
state_dir="$scratch/state"
workspace_root="$scratch/workspaces"
worklist="$scratch/worklist.json"
created=0
daemon_pid=

cleanup() {
  status=$?
  set +e
  if [[ -n $daemon_pid ]]; then
    kill "$daemon_pid" >/dev/null 2>&1 || true
    wait "$daemon_pid" >/dev/null 2>&1 || true
  fi
  if [[ $status != 0 && -f $scratch/daemon.log ]]; then
    echo "final-package daemon tail after e2e failure:" >&2
    tail -240 "$scratch/daemon.log" >&2
  fi
  if [[ $created == 1 && ${TALLY_CAMPAIGN_E2E_KEEP_REPO:-0} != 1 ]]; then
    if ! gh repo delete "$repository" --yes >/dev/null 2>&1; then
      echo "warning: could not delete temporary repository $repository; archiving it instead" >&2
      gh repo archive "$repository" --yes >/dev/null 2>&1 || {
        echo "warning: could not archive temporary repository $repository" >&2
      }
    fi
  fi
  rm -rf -- "$scratch"
  trap - EXIT
  exit "$status"
}
trap cleanup EXIT

if [[ $daemon_mode == package ]]; then
  mkdir -p "$scratch/daemon-state" "$scratch/daemon-data"
  "$tally" --config "$config" --socket "$socket" daemon run \
    --cpu-weight 100 --memory-max-bytes 8589934592 \
    --state-dir "$scratch/daemon-state" --data-dir "$scratch/daemon-data" \
    >"$scratch/daemon.log" 2>&1 &
  daemon_pid=$!
  for _ in $(seq 1 100); do
    [[ -S $socket ]] && break
    if ! kill -0 "$daemon_pid" >/dev/null 2>&1; then
      sed -n '1,240p' "$scratch/daemon.log" >&2
      echo "final-package daemon exited before creating $socket" >&2
      exit 1
    fi
    sleep 0.1
  done
  if [[ ! -S $socket ]]; then
    sed -n '1,240p' "$scratch/daemon.log" >&2
    echo "final-package daemon did not create $socket" >&2
    exit 1
  fi
fi

git init --quiet --initial-branch=main "$checkout"
git -C "$checkout" config user.name "Tally Campaign E2E"
git -C "$checkout" config user.email "tally-campaign-e2e@invalid"
touch "$checkout/.gitkeep"
git -C "$checkout" add .gitkeep
git -C "$checkout" commit --quiet -m "fixture: initialize campaign e2e"
gh repo create "$repository" --private --source "$checkout" --remote origin --push >/dev/null
created=1
repository_ready=0
for _ in $(seq 1 40); do
  if labels=$(gh label list --repo "$repository" --limit 1000 --json name 2>/dev/null) &&
    jq -e 'type == "array"' >/dev/null <<<"$labels"; then
    repository_ready=1
    break
  fi
  sleep 0.5
done
if [[ $repository_ready != 1 ]]; then
  echo "GitHub label API did not become ready for $repository" >&2
  exit 1
fi

jq -n \
  --arg checkout "$checkout" \
  --arg pool "$pool" \
  --arg python "$(command -v python3)" \
  --arg agent "$agent" \
  '{
    schemaVersion: 1,
    campaign: {
      name: "campaign-e2e",
      repository: {
        checkout: $checkout,
        baseBranch: "main",
        remote: "origin",
        forge: "github"
      },
      maxTasks: 2,
      maxParallel: 1,
      driverRuntimeMaxSec: 300,
      runtimeMaxSec: 3600,
      pool: $pool,
      agent: {
        adapter: "shell",
        argv: [$python, $agent],
        priority: "low",
        runtimeMaxSec: 120,
        approvalPolicy: null,
        sandboxPolicy: null
      },
      gates: [{
        kind: "command",
        id: "clean-diff",
        preflightArgv: ["git", "rev-parse", "HEAD"],
        argv: ["git", "diff", "--check", "origin/main...HEAD"],
        runtimeMaxSec: 60
      }]
    },
    tasks: [
      {
        id: "first",
        kind: "implementation",
        title: "Create the first proof file",
        body: "Create exactly one.txt containing the single line one. Do not modify any other tracked file. Commit the result.",
        dependencies: [],
        conflictDomains: ["one.txt"]
      },
      {
        id: "second",
        kind: "implementation",
        title: "Create the dependent proof file",
        body: "Create exactly two.txt containing the single line two. Preserve one.txt and do not modify any other tracked file. Commit the result.",
        dependencies: ["first"],
        conflictDomains: ["two.txt"]
      }
    ]
  }' >"$worklist"

projected=$(
  "$tally" --config "$config" --socket "$socket" \
    campaign project "$worklist" --repo "$repository"
)
issue_url=$(jq -er .issue <<<"$projected")

"$tally" --config "$config" --socket "$socket" campaign arm "$issue_url" \
  --state-dir "$state_dir" --workspace-root "$workspace_root" --wait >/dev/null

for _ in $(seq 1 8); do
  state=$(gh issue view "$issue_url" --json state --jq .state)
  [[ $state == CLOSED ]] && break
  "$tally" --config "$config" --socket "$socket" campaign poll --once --wait \
    --state-dir "$state_dir" >/dev/null
done

master=$(gh issue view "$issue_url" --json state,body,comments)
[[ $(jq -r .state <<<"$master") == CLOSED ]]
"$tally" --config "$config" --socket "$socket" campaign poll --once \
  --state-dir "$state_dir" >/dev/null
[[ $(jq -r .body <<<"$master" | grep -c -- '^- \[x\].*tally:campaign-task:v1') == 2 ]]
[[ $(jq '[.comments[].body | contains("tally:campaign-complete:v1")] | any' <<<"$master") == true ]]

pulls=$(gh pr list --repo "$repository" --state merged --limit 10 \
  --json body,headRefName,mergeCommit,url)
[[ $(jq 'length' <<<"$pulls") == 2 ]]
[[ $(jq '[.[].body | contains("tally:spec-build:v2")] | all' <<<"$pulls") == true ]]
[[ $(jq '[.[].body | capture("revision=(?<revision>sha256:[0-9a-f]{64})").revision] | unique | length' <<<"$pulls") == 2 ]]

git -C "$checkout" fetch --quiet origin main
git -C "$checkout" show origin/main:one.txt | grep -qx one
git -C "$checkout" show origin/main:two.txt | grep -qx two
[[ $("$tally" campaign list --state-dir "$state_dir" | jq 'length') == 0 ]]

jq -n \
  --arg package "$package" \
  --arg daemonMode "$daemon_mode" \
  --arg repository "$repository" \
  --arg issue "$issue_url" \
  --argjson pulls "$pulls" \
  '{status:"pass", package:$package, daemonMode:$daemonMode, repository:$repository, issue:$issue, mergedPullRequests:($pulls | map(.url))}'
