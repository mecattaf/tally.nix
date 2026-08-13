---
name: assign-tally
description: Prepare and arm a local tally campaign from a committed JSON worklist. Use when assigning a multi-task buildout to tally, turning a project document into a dependency graph, or starting an autonomous campaign from a repository worklist.
---

# Assign a buildout to tally

Turn the requested outcome into one committed worklist, arm it locally, and let
tally own execution. Do not create per-campaign orchestration or supervise task
work through a public forge. Use `campaign-operator` after arming.

## Write the worklist

Create one repository JSON file containing exactly `schemaVersion: 1` and a
non-empty `tasks` array.

Populate `tasks` in topological order. Give every implementation task:

- a stable lowercase `id`, `kind: "implementation"`, and concise `title`;
- one bounded `goal` and explicit `deliveredBehaviors`;
- `readFirst.specSections` that exist at the authority revision, plus only
  genuinely useful `styleReferences`;
- non-empty `acceptanceCriteria`, each with the exact executable `argv` that
  proves its claim;
- dependencies naming only earlier tasks; and
- normalized repository-relative `conflictDomains` that cover every path the
  task may change. Require them whenever `maxParallel` is greater than one.

Give a checkpoint only `id`, `kind: "checkpoint"`, `title`, executable `argv`,
`runtimeMaxSec`, and earlier `dependencies`. Let tally render its brief.

Encode ordering as dependencies, invariants as executable gates, and ownership
as path domains. Do not put an operational requirement only in prose. For a
campaign of consequence, end with an adversarial test-and-fix task whose checks
enumerate the affected surface structurally.

## Rehearse admission

Make gate preflights test only the environment and tracked inputs. Never make a
preflight depend on state produced by another gate. Print a useful diagnostic to
stderr before failing.

Run every declared preflight argv verbatim in a pristine worktree on the target
host. Fix the worklist or host until admission is clean.

## Establish authority

Declare the campaign in the host module with `forge = "local"`, its repository
checkout, worklist pattern, task and parallelism bounds, agent, gates, and merge
policy.

Commit the worklist, merge it to the configured base branch, and push that base
branch. Arming fetches the configured remote and admits the single matching file
from that revision; working-tree bytes are not authority.

## Arm

Run:

```text
tally campaign arm OWNER/REPO PATH/TO/WORKLIST.json
```

Add `--wait` only when waiting for the newly admitted reconcile pass is useful;
that pass is not the whole campaign. Keep the JSON receipt, then hand observation
to `campaign-operator` using the same repository/worklist identity.
