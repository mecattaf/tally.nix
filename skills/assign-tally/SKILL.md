---
name: assign-tally
description: Prepare and arm a local tally campaign from a committed JSON worklist. Use when assigning a multi-task buildout to tally, turning a project document into a dependency graph, or starting an autonomous campaign from a repository worklist.
---

# Assign a buildout to tally

Turn the requested outcome into one committed worklist, arm it locally, and let
tally own execution. Do not create per-campaign orchestration or supervise task
work through a public forge. Use `campaign-operator` after arming.

## Equip the sitting

Authoring is one sitting against the observed tree, repeated at every stage
boundary. A well-equipped sitting needs no mid-run judgment: no ownership
correction, no gate cycle, no steer. Equip it in this order.

1. Run the edge census against the observed tree (below).
2. Assemble the gate set from the template (below).
3. Rehearse every argv verbatim in a pristine worktree — gates, preflights, and
   checkpoints — including against a local un-pushed HEAD.
4. Audit acceptance-argv output and write the taming into the argv itself.
5. Author seam acceptance criteria from real artifacts, and give every
   delivered behavior a criterion that fails without it.
6. Write the standing discipline into the briefs that need it: commit first and
   verify second; take the machine's enumerated list verbatim rather than
   re-deriving it; attribute against the deployed store path before crediting
   or blaming any merged commit.
7. Take known breakage in as fully scoped tasks, with the scoping ruling in the
   `goal`. Something already broken at authoring time is never a steer target.
8. Preflight external authority: `gh auth status` scopes, including
   `delete_repo` when the worklist carries release or probe tasks, plus one
   rollback rehearsal before any run that will deploy.
9. Commit the supervisor as a fixture rather than as prose: the proven
   `reg=1 && jobs=0` stall predicate as a script in the repository, armed with
   the campaign.
10. Leave the close to `campaign-operator`, where it is an ordered checklist.
11. Invent no worklist keys. Admission refuses unknown ones, and the two
    constructs the record priced — campaign-wide discipline rendered into every
    brief, and a structured refusal outcome — arrive through tally, not through
    the file you write. Until they land, item 6 is their only carrier.

Nothing above is a standing process document. Ten of the eleven items are
greps, rehearsals, one-line gate entries, and sentences in existing briefs; the
census itself dies with the sitting, and only the templates persist.

## Run the edge census

A census of magnitudes — lines, actions, test counts, schema sites — sizes the
work and cannot see its edges. Ask the edge questions in the same sitting: who
references the thing a task deletes, and what does the toolchain regenerate
when it builds. Fold every answer into `conflictDomains`, `deliveredBehaviors`,
or an acceptance criterion.

1. **Deletion and rename consumers.** For every file, fixture, symbol, CLI
   surface, or serde field a task deletes or renames, grep the whole tree for
   each name and path variant:
   `grep -rn '<name-variants>' crates/ nix/ test/ examples/ flake.nix`. Every
   hit enters that task's `conflictDomains` or a prerequisite task.
2. **Assertion inversion.** For every behavior a task changes, grep the
   flake-only suites, the VM tests, and `test/final-bar/cases/` for assertions
   of the old behavior. The decisive question is whether a suite asserts
   symbols the task deletes: `grep -c '<symbol>' test/...`.
3. **Estate bytes.** For any durable-format or serde change:
   `grep -rl '<field>' ~/.local/state/tally | wc -l`. Count the fields the
   writer emitted, never the values a reader cares about. A nonzero count means
   the task carries an accept-and-discard arm as a delivered behavior, proven
   by a real-sample fixture criterion.
4. **Toolchain side effects.** Take one scratch-worktree build for each lane
   that may touch dependencies. Any file the toolchain regenerates —
   `Cargo.lock` foremost — enters the `conflictDomains` of every such lane at
   once, as a cohort, before the first of them runs.
5. **Effective width.** Intersect `conflictDomains` pairwise and report the
   widest antichain of ready tasks beside `maxParallel`. Estimate a deletion
   stage as a chain and a build stage as a fan: overlapping domains serialize a
   deletion wave whatever the bound says, and a free slot beside pending tasks
   is that arithmetic, not a stall.
6. **Acceptance-argv output.** Run each acceptance argv once. Any argv emitting
   more than a few KB gets `2>&1 | tail -30` written into the worklist argv
   itself.
7. **Seam artifacts.** A task that builds a verifier or reader of an existing
   writer gets a criterion that runs both sides on one real artifact — a real
   merged trailer, a real estate row.

## Write the worklist

Create one repository JSON file containing `schemaVersion: 1`, a top-level
`campaign` policy object, and a non-empty `tasks` array. Keep `campaign` closed:
set task/parallelism bounds, the agent adapter and policies, merge/runtime
policy, and 1–16 command or `forbidPaths` gates there. Do not put labels,
mentions, actors, issue coordinates, posting policy, checkout paths, or pool
names in it.

Populate `tasks` in topological order. Give every implementation task:

- a stable lowercase `id`, `kind: "implementation"`, and concise `title`;
- one bounded `goal` and explicit `deliveredBehaviors`;
- `readFirst.specSections` that exist at the authority revision, plus only
  genuinely useful `styleReferences`;
- non-empty `acceptanceCriteria`, each with the exact executable `argv` that
  proves its claim, and at least one that fails without each delivered
  behavior;
- dependencies naming only earlier tasks; and
- normalized repository-relative `conflictDomains` that cover every path the
  task may change. Require them whenever `maxParallel` is greater than one.

When a governing spec exists — `specs/<identity>/spec.md`, sharing the
worklist's filename stem — four rules bind the bytes you write:

- Task `goal` text cites claim ids and evidence ids; it does not restate them.
  The spec is the one authority for what a claim says, and a restatement is a
  second copy that drifts.
- `readFirst.specSections` point at number-derived anchors of the form
  `specs/<identity>/spec.md#rN` — `### R2 — the trace` anchors at `#r2` and
  nowhere else, so a retitle cannot dangle the pointer. Every anchor must
  exist at the authority revision.
- The sitting appends `specs/<identity>/trace.json` rows joining claim → task
  → acceptance ids, in the same commit as the worklist stage. A trace row
  written later is a row written from memory.
- The governing spec appears in no task's `conflictDomains`, and no lane
  writes it. Spec churn reaches the graph only through the sitting that
  amends the worklist.

Give a checkpoint only `id`, `kind: "checkpoint"`, `title`, executable `argv`,
`runtimeMaxSec`, and earlier `dependencies`. Let tally render its brief.

Encode ordering as dependencies, invariants as executable gates, and ownership
as path domains. Do not put an operational requirement only in prose. For a
campaign of consequence, end with an adversarial test-and-fix task whose checks
enumerate the affected surface structurally.

## Assemble the gate set

The gate set is the highest-leverage artifact in the worklist, and the cap is
16. Start from this template, and drop an entry only for a stated reason:

- the driver suite: `python3 test/spec_build_driver_test.py`;
- workspace tests: `nix develop --command cargo test --workspace`;
- lints as a gate rather than as weather:
  `nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- a built, non-VM `flake check` subset naming its attributes literally;
- cheap metadata predicates — changelog and pull-request shape — first in the
  order, never last; and
- the final bar at the chapter gate, executed rather than listed.

Build the check subset. Do not evaluate it:

```text
nix build --no-link \
  .#checks.x86_64-linux.spec-build-driver-tests \
  .#checks.x86_64-linux.module-layer \
  .#checks.x86_64-linux.campaign-runtime
```

A bare `nix flake check --no-build` eval structurally cannot see the class that
fails chapter gates, because those checks are derivations that fail at build
time. Name attributes that exist at this revision, and confirm each one runs
its cases rather than listing them: a harness that builds without asserting
anything is a gate that passes without grading.

Changing a gate is a worklist commit, never a deploy. A lint class a lane will
write costs one JSON line today, or an amendment task, a re-arm, and an hour of
chapter gate tomorrow.

## Rehearse admission

Make gate preflights test only the environment and tracked inputs. Never make a
preflight depend on state produced by another gate. Print a useful diagnostic to
stderr before failing.

Run every declared preflight argv verbatim in a pristine worktree on the target
host. Fix the worklist or host until admission is clean.

Rehearse the gate and checkpoint argv the same way, and rehearse them against a
local un-pushed HEAD — the state a gate actually meets in local mode. An argv
whose failure you can predict is fixed at this sitting, never armed and
watched.

## Establish authority

Commit the worklist, merge it to the intended base branch, and push that branch.
The host supplies only its adapter catalog and ordinary tally state/socket
configuration. Arming receives the checkout, base branch, and remote directly,
then admits the single matching file from that remote revision; working-tree
bytes are not authority.

## Arm

Run:

```text
tally campaign arm OWNER/REPO PATH/TO/WORKLIST.json
```

Run from the repository checkout, or pass `--checkout PATH`; use
`--base-branch` or `--remote` when their `main`/`origin` defaults do not match.

Add `--wait` only when waiting for the newly admitted reconcile pass is useful;
that pass is not the whole campaign. Keep the JSON receipt, then hand observation
to `campaign-operator` using the same repository/worklist identity.
