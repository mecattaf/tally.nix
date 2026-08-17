# zeta — the authority plane

Status: proposed
Governs: silent-factory-worklists/zeta.json
Consumers: checks.x86_64-linux.spec-lint; the zeta worklist readFirst anchors; the close sitting coverage render
Supersedes: none

## Outcome

Today no mechanical reader exists for a spec: the format is exhortative, its
pointers are checked by eye, and the close-out table is rendered by hand.
After this campaign, `crates/spec-lint` parses every committed
`specs/<identity>/` directory, `checks.x86_64-linux.spec-lint` executes it on
every fleet-gated head with a bite proof inside the same derivation, the
worklist ↔ spec ↔ trace join is resolved mechanically, and the coverage table
is tool-rendered at the close. The three campaign skills carry the spec-layer
doctrine as committed bytes, and the shipped docs teach the ratified anchor
grammar. The first stage ever derived per `skills/author-spec` with real
anchors becomes possible at the sitting that follows this campaign's close.

## Vocabulary

- spec-lint (NEW) — the enforcement engine: one Rust binary crate at
  `crates/spec-lint`, one parser, surfaced as a flake check, a sitting-mode
  command, and coverage/census render modes.
- claim — one numbered arrow line `condition → observable` with exactly one
  oracle binding.
- binding — a claim's single oracle: a flake check attribute, a worklist gate
  id, or HUMAN-ATTENDED.
- sitting — the human-attended boundary act that authors a worklist stage
  from a ratified spec and appends trace rows.
- trace row — one append-only record in `trace.json` joining a claim to a
  task and, at release, to a merged commit and witness ref.
- must-fail corpus (NEW) — `crates/spec-lint/tests/fixtures/must-fail/`, one
  deliberately broken artifact per rule class.
- expected-defects.json (NEW) — the exact defect map, rule id to count, that
  the must-fail corpus must reproduce.
- identity — the directory name under `specs/` and the worklist filename stem;
  the join key.

## Rulings

| id | decision | ruling |
|---|---|---|
| Z1 | campaign identity | `zeta`; `specs/zeta/` and `silent-factory-worklists/zeta.json` share the stem |
| Z2 | epsilon-extension governance | `specs/epsilon-extension/` is evidence-only, no spec.md; `EPSILON-EXTENSION.md` stays the one ratified authority for its campaign mid-flight; the lint skips spec-less identity dirs |
| Z3 | campaign gates | the four template gates verbatim from the epsilon-extension worklist; no spec-lint gate in this worklist — the chapter gate's flake check covers it once the attribute exists |
| Z4 | trace timing | `trace.json` ships with zero rows; sitting rows are appended at the boundary sitting in the same commit as the worklist |
| Z5 | fixture home | `crates/spec-lint/tests/fixtures/golden/` and the must-fail corpus with an exact defect map — never a bare nonzero-exit assertion |
| Z6 | ratification timing | proposed tonight; ratified at the boundary sitting after the falsity pass against the observed post-ext0 tree |
| Z7 | release-row witness | the summary/complete ref, resolved by the release closing summary |

## Claims

### R1 — the linter and its bite
Why: a bar without a gate is not a bar (VD-5, F33); the lint is the layer's standing consumer and the must-fail corpus is the lint's own.
1.1 `spec-lint --mode check` over a defect-free `specs/zeta` → exit 0 (given). [gate: cargo-tests]
1.2 `spec-lint --mode check` over the must-fail corpus → exit 2 (given) with defect codes equal to `expected-defects.json`. [gate: cargo-tests]
1.3 a gated head where either side of 1.1/1.2 flips → the flake check attribute fails. [check: spec-lint]
1.4 a claim line with zero or two binding tokens → defect L9 (given). [gate: cargo-tests]
1.5 a numeral with no provenance on a non-BELIEVE claim line → defect L7 (given). [gate: cargo-tests]
1.6 a backticked identifier absent from tree, Vocabulary, and (NEW) set → defect L8 (given). [gate: cargo-tests]
1.7 `Status: ratified` with an outstanding GUESS or DECISION line → defect L10 (given). [gate: cargo-tests]

### R2 — the trace
Why: the freeze contradiction resolves only if the join lives beside the frozen file, append-only.
2.1 a trace row naming a claim id absent from `spec.md` → defect L14 (given). [gate: cargo-tests]
2.2 a release row with no prior sitting row for the same claim and task → defect L14 (given). [gate: cargo-tests]
2.3 in sitting mode, parent rows not a structural prefix of head rows → defect L17 (given). [gate: cargo-tests]
2.4 `trace.json` invalid against `contracts/trace.schema.json` → defect L14 (given). [gate: cargo-tests]

### R3 — the seams
Why: the layer attaches with zero machinery change; these lines are falsified by the tree, not defended by it.
3.1 BELIEVE:examples/flows/spec-build.js — `specSections` items are free strings of maxLength 1000 → anchors of the form specs/zeta/spec.md#r2 are admissible worklist bytes unchanged. [HUMAN-ATTENDED]
3.2 BELIEVE:crates/tally/src/cli/campaign.rs — the worker brief renders `specSections` verbatim under its read-first heading → the worker receives the anchor untouched. [HUMAN-ATTENDED]
3.3 BELIEVE:test/fleet-gate.sh — the ladder runs `nix flake check -L --keep-going` → the new attribute grades every fleet-gated head with zero fleet-gate edits. [check: spec-lint]

### R4 — the skills and docs
Why: doctrine in prose decays; the procedures land where agents execute them.
4.1 `skills/author-spec/SKILL.md` exists with the four sections and the sitting checklist → the sitting runs from committed bytes. [check: spec-lint]
4.2 the `specs/README.md` rule-index table and the implemented rule set diverge → a crate test fails. [gate: cargo-tests]

## Unchanged

U.1 worklist admission refuses unknown keys → the zeta worklist admits under schemaVersion 1 (given) with zero added keys. [gate: cargo-tests]
U.2 the pre-existing checks module-layer and campaign-runtime build beside the new attribute → the flake stays green. [gate: flake-build-subset]

## Unknowns

UNKNOWN-1 whether an existing cargo test covers the read-first brief rendering (the would-be oracle for 3.2) — the spec-lint-core lane greps the campaign CLI tests; if present, 3.2 rebinds from HUMAN-ATTENDED at the next sitting.
DECISION-1 the worklist `steward` field value after the ext0 merge? proposed: narrator (given)

## Stages

### S1 — the built plane
Order: spec-lint-core, then spec-lint-resolution, then spec-lint-flake-check,
then spec-layer-skills-amend; doc-anchor-regrammar independent and parallel;
the chapter gate closes over all five. Claims R1–R4; rulings Z1–Z7; task
specs verbatim in `ZETA.md`, finalized into the worklist at the boundary
sitting.

## Forbidden

F.1 Do not add keys to the worklist schema.
F.2 Do not put model names in spec or worklist bytes.
F.3 Do not write under `specs/zeta/` from any lane.
F.4 Do not build an archive verb, a spec index file, or a spec-sha receipt stamp.
F.5 Do not bind any claim to a list-only flake attribute.
F.6 Do not edit `test/fleet-gate.sh`.
