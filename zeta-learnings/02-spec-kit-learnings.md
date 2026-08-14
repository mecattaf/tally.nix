# Learnings II — spec-kit, the newest anatomy

*What GitHub's spec-kit (v0.16.x, cloned 2026-08) actually installs, what its
methodology claims, and what of it the house should take. Verified
empirically: the installer was run into a scratch brownfield repo and a
hand-built minimal set was proven to still work.*

## Three changes that reshape the picture

The newest spec-kit is materially friendlier to tally's situation than its
reputation suggests. First, **Claude commands install as skills** —
`.claude/skills/speckit-*/SKILL.md`, invoked `/speckit-plan` — not as a
custom command directory. A house version is therefore just skills we author
ourselves, on a surface tally already has a culture for. Second, **feature
creation no longer touches git**: `specs/NNN-slug/` plus a `feature.json`
pointer; branching is an opt-in extension. Brownfield installs are safe by
default. Third, **everything ships in the wheel** — no release zips, fully
offline, all assets under `core_pack/`.

## The load-bearing minimum is small

The complete working install is about thirty files, and the genuinely
required set is smaller: ten skill files, five templates (spec, plan, tasks,
constitution, checklist — looked up by exact stem), six bash scripts that
must remain siblings in `.specify/scripts/bash/`, and
`.specify/memory/constitution.md`. The `.specify/` directory name is
hardcoded throughout as the project-root marker (`find_specify_root` walks
upward for it — which is also what makes monorepo and subdirectory installs
work). Two install-time rewrites must be reproduced if hand-copying:
`__SPECKIT_COMMAND_X__` placeholders in three templates become `/speckit-x`,
and two scripts get the same literal baked in. Everything else — manifests,
init-options, workflow registry — is upgrade machinery, safely omittable.

Notable in the command set: `specify` itself runs no script (the agent does
the work); `converge` is new — assess the codebase against spec/plan/tasks,
append remaining work to tasks.md, loop until clean, strictly append-only.
The `assess` extension adds a five-stage discovery funnel; the `checklist`
command frames requirement checklists as "unit tests for English."

## Brownfield is a philosophy, not a mode

There is no brownfield flag. Support is converge plus a written guide naming
three artifact-persistence models: **Flow-Forward** (each feature directory
is a historical record), **Living Spec** (spec.md is the standing contract;
plan and tasks are derived and disposable), **Flow-Back** (implementation
discoveries reshape the artifacts, then re-align). The Living Spec model is
almost exactly the EPSILON-EXTENSION relationship — a ratified contract from
which staged worklists are derived at boundaries — which is reassuring: the
house pattern is one of the officially recognized shapes, not a deviation.

## The methodology, and where it is strong

The core claim: the spec is the primary artifact and code is its expression;
maintaining software means evolving specs. Its enforcement mechanism is
templates — they block premature implementation detail, force explicit
`[NEEDS CLARIFICATION]` markers, and impose gates (simplicity,
anti-abstraction, integration-first) whose failures must be justified in a
Complexity Tracking section. This is the same insight PA-34 measured on
tally's own record: the template is where doctrine survives. The constitution
is wired into everything — plan runs a Constitution Check, analyze and
converge treat it as non-negotiable authority.

Two structural ideas worth keeping even if their implementation isn't: the
**four-tier template override stack** (overrides → presets → extensions →
core) as a model for scoped doctrine, and **converge as a loop with an
append-only contract** — the honest brownfield verb.

## Where it is weak, on the agency record

This is not theoretical: agency's corpus *is* spec-kit (v0.12.18, twenty
domains, an 823-line constitution), and the D13 pilot ran the downstream
cadence by hand. The verdicts from that run: `analyze` reported zero
findings while three contract-vs-contract defects waited, because analyze
never looks inside `contracts/` for cross-schema resolvability or fixture
producibility — the missing gate was a contract linter, not a better prompt.
The converge report could not be trusted at face value; the orchestrator had
to re-run the suite, recompute byte hashes against frozen fixtures, and run
a perturbation probe to prove the gate was non-vacuous. And the templates'
prose-first requirements style produces exactly the loose acceptance
criteria that Kiro's EARS discipline exists to prevent.

## What the house should take

- The **directory shape**: one directory per spec unit, artifacts inside it,
  tooling elsewhere. (House variant: identity-named, not numbered — the
  campaign identity is the join key to the worklist.)
- The **constitution as wired authority**: not a document that exists, but
  one every derivation step is contractually required to check.
- **Converge's stance**: append-only assessment against the spec, looping
  until dry — recognizable as the reconcile pass applied to authoring.
- The **minimal-install discipline**: the fewest files that work, every one
  with a named consumer.

## What the house should not take

- The **numbering scheme and feature.json machinery** — tally has campaign
  identity.
- **Mid-flow gates** (workflow.yml `type: gate`) — the lineage's one
  standing anti-lesson from spec-kit remains: no human wait states inside a
  run.
- **Generic analyze** as the verification story — superseded by the contract
  linter and, for specs of consequence, the grind.
- The **tasks template** — tally's worklist is a categorically stronger
  tasks artifact: machine-admitted, sha-keyed, epoch-bearing, graded by
  gates. Nothing in spec-kit's tasks.md survives contact with it.

One interoperability note: agency's corpus already lives in spec-kit's
layout (`.specify/memory/constitution.md`, `specs/NNN-dXX-*/`, contracts
directories). Whatever dialect the house defines for tally should either
read that layout or state explicitly how the two relate — two spec dialects
with no declared relationship would be a twice-implemented contract, the G2
defect class, at the meta level.
