# ZETA — the authority plane, compiled

Written 2026-08-14, compiled from two blind-tandem Fable designs (the campaign
architect and the authority-plane designer) plus the zeta-learnings record.
This file is the **authoring source for `silent-factory-worklists/zeta.json`**,
which is deliberately NOT committed tonight: it finalizes at the boundary
sitting (act A7) against the observed post-ext0 tree (A12). Standing consumers:
the boundary sitting; `specs/zeta/spec.md` (which this file's task specs derive
into); the operator running acts A1–A9.

Zeta in one line: tally teaching Claude Code's spec instincts to leave
receipts. Zeta = EPSILON-EXTENSION as ratified (untouched, runs tonight) plus
the authority plane grown by campaign — tally changing itself, and the first
witnessed two-way code⇄spec round trip.

## Compiler rulings (the collision, settled)

The two designs agreed on the deep structure: the `#rN` number-derived anchor
grammar, `specs/README.md` as the lint rule list, `checks.x86_64-linux.spec-lint`
as the standing consumer, the sitting shape, and the reflexive first spec.
They collided on six points; settled here, recorded as Rulings Z1–Z7 in
`specs/zeta/spec.md`:

1. **Identity = `zeta`** (not `zeta-authority-plane`). Directory `specs/zeta/`,
   worklist `silent-factory-worklists/zeta.json`. (Plane's DECISION-1, deferred
   to sequencing; settled short.)
2. **`specs/epsilon-extension/` is evidence-only — no spec.md recast.** The
   architect wanted EPSILON-EXTENSION.md recast into house grammar tonight; the
   plane designer ruled one-authority-per-fact: a second ratified surface for a
   program mid-flight is a fork of authority. The plane wins. EPSILON-EXTENSION.md
   stays the sole ratified authority for its campaign; the eight excavation
   ledgers live under `specs/epsilon-extension/evidence/` so the live worklist's
   citations (CA-3, F37, PA-25/26, VD-5, EQ-…) resolve at the authority
   revision; the lint skips spec-less identity dirs. This also dissolves the
   architect's DECISION-4 (day-one lint scope: the only spec.md in the corpus
   is `specs/zeta/spec.md`, authored tonight with the rule list in hand) and
   DECISION-6 (no freeze-pointer edit to EPSILON-EXTENSION.md is needed — the
   ratified program's file is not touched at all). The epsilon-extension recast
   happens later, as the tally-crystallization sitting's work, when that
   campaign has closed and the recast is history, not a fork.
3. **Anchor grammar `### R<n> — name` → `#r<n>`** (architect's DECISION-2,
   settled to the short form both halves already used).
4. **Fixture home: `crates/spec-lint/tests/fixtures/{golden,must-fail}/` with
   `expected-defects.json`** asserting the exact `{rule: count}` map — the
   plane's stronger bite proof supersedes the architect's `fixtures/` path and
   mere-nonzero-exit acceptance.
5. **No `spec-lint` campaign gate in zeta.json.** The architect is right: the
   gate would fail every merge until the flake task lands. The chapter gate's
   `nix flake check` (via test/fleet-gate.sh:254) covers it from the moment the
   attribute exists. The gate template below is recorded for ext1-era worklists.
6. **`specs/zeta/trace.json` ships empty tonight** (`rows: []`). The architect
   wanted downward rows tonight; the plane's L14 forbids rows naming task ids
   that exist in no committed worklist. Sitting rows are written at A7, in the
   same commit as the worklist — which is also the doctrine's own rule (rows
   are written at sittings, nowhere else).

Also settled: the release-row `witness` field targets the summary/complete ref
(plane's proposal, `release_closing_summary` already resolves it); claim 4.1
binds `[check: spec-lint]` (no gate by ruling 5); the direction sentence ("the
spec points at tasks; the worklist schema does not change") lands in
constitution A2, where the seam map argued it belongs. Constitution v2 keeps
A1–A21 ids stable, amends A2, shrinks A16/A17 to one sentence + skill pointer,
and adds A22 (freeze/append). README v2 keeps only what the linter enforces.

**Still open (drained at A7, typed in `specs/zeta/spec.md` §Unknowns):**
DECISION-1 — the `steward` field value post-ext0 (drafted `"narrator"` as
schema placeholder; settled against the merged ext0 tree). UNKNOWN-1 — whether
an existing cargo test covers the `## Read first` brief rendering (3.2's
would-be oracle).

## Sequencing ruling: FOLLOW — after ext0's boundary, before the ext1 sitting

Zeta runs as a second campaign on its own fresh identity, armed at the
ext0→ext1 boundary — after ext0's interim close and the boundary deploy,
before the ext1 sitting authors anything.

- **Not absorbed** into epsilon-extension: a campaign can never be governed by
  the surface it authors (the first spec-layer campaign would refuse itself);
  the chapter gate closes over exactly the ratified ext0 task set; R3
  deny-list scoping is per governing spec of the armed identity and only works
  if zeta has its own identity.
- **Not beside** it tonight: cross-campaign collision blindness (ownership
  certification and conflictDomains are per-campaign; zeta owns `flake.nix`,
  `Cargo.toml`, `Cargo.lock` while ext0's `final-bar-executes` owns
  `flake.nix` and every ext0 gate runs `cargo test --workspace`); width is
  already spent (effective width 2, chain-not-fan); a sibling fast-forwarding
  the base mid-chapter-gate invalidates the 3-hour audit.
- **Not deferred** past ext1: the ext1 sitting is the payoff — if zeta lands
  first, ext1 becomes the first stage ever derived per `skills/author-spec`
  with real `specs/**` anchors, trace rows, and the lint rehearsal. And zeta's
  low-risk cargo (one crate, one check attribute, doc/skill edits) makes it
  the shakedown campaign for the post-ext0 machinery (epoch budgets, outcome
  envelopes, judge verdicts, subject adoption) before ext1's self-modifying
  last mile depends on them.

Zeta waits on ext0 **quiescence**, not ext0 success: it needs only dead lanes
on `flake.nix`/`skills/`/workspace domains and a deployed pin.

## Stage plan

**Stage Z-A — the authored plane (TONIGHT, operator commits A1/A2, no lanes).**
Authored artifacts land at sittings via operator commits; Fable is the only
trusted spec author; a Codex lane transcribing text that already exists in its
goal is a transcription act by proxy. The set (all written, in the tree now):
`specs/README.md` v2 · `specs/constitution.md` v2 · `specs/epsilon-extension/evidence/`
(eight ledgers) · `specs/zeta/{spec.md (Status: proposed), trace.json (empty),
contracts/trace.schema.json, contracts/claim-line.fixtures.json}` ·
`skills/author-spec/SKILL.md` · this file.

**Stage Z-0 — the built plane (Codex lanes, overnight #2).** Dispatch order
(width 2 for ~40 min, then a chain):

```
spec-lint-core ──► spec-lint-resolution ──► spec-lint-flake-check ──► spec-layer-skills-amend ──┐
doc-anchor-regrammar (parallel, independent) ───────────────────────────────────────────────────┴─► zeta-chapter-gate
```

Estimate: core 2.5–3.5h → resolution 1.5–2.5h → flake-check 0.75–1.5h →
skills 0.5–1h → chapter gate 1–2.5h ≈ **7–10h; one overnight** (arm ~21:00
Aug 15 → close Aug 16 morning).

Zeta floor, all six items, placed: specs dir (Z-A) · lint as flake check (Z-0)
· real readFirst anchors (zeta.json is the first consumer — every task's
specSections are `specs/zeta/spec.md#rN` anchors) · trace rows at sittings
(A7) · coverage handed to release (A9 uses `--coverage`) · governing spec out
of conflict domains (`specs/zeta` appears in NO task's conflictDomains;
authoring discipline now, scoped deny-list at ext1).

Deferred with owner: `specs/<armed-identity>/**` on the R3 deny-list, admission
resolving `specs/**` pointers, escalation reports resolving citations —
**ext1 sitting** (each priced against D58, now authorable with real anchors).
Release rendering coverage from durable completion facts — **ext2**.
`tally spec diff`/`questions` verbs, the inbox-borne doubt queue — post-zeta,
each admitted only when it deletes an operator rule. The grind for spec-lint
itself — waits for the first contract of consequence (agency's dialect
bridge); the must-fail corpus is the bar for a linter. Recasting historical
worklists — history is history.

## Worklist header (zeta.json, finalized at A7)

`schemaVersion: 1` · name `zeta` · `maxTasks: 8` · `maxParallel: 2` ·
`steward:` DECISION-1 (draft `"narrator"`, settled at A7) · gates: the four
template gates **verbatim** from epsilon-extension.json:8–37 — `driver-suite`,
`cargo-tests`, `clippy`, `flake-build-subset`. No `spec-lint` gate (ruling 5).
Caps stay authored (pre-ext1 schema requires them) — flagged against the
zero-required-numbers destination.

Recorded for ext1-era worklists (NOT in zeta.json): the spec-lint gate
template, cheap-first placement before `cargo-tests`:
`{"kind":"command","id":"spec-lint","preflightArgv":["sh","-euc","command -v nix >/dev/null"],"argv":["nix","build","--no-link",".#checks.x86_64-linux.spec-lint"],"runtimeMaxSec":900}`
(900 is GUESS — unmeasured cold build; cached after).

## Task specs (verbatim goals — the A7 authoring source)

### 1. `spec-lint-core` — implementation

**goal:** "The authority plane's enforcement engine does not exist:
specs/README.md v2 defines the lintable claim grammar and rule list, and both
authoring models specified — verbatim, in zeta-learnings/raw/instinct-fable.md
and raw/instinct-opus.md — the two rules that catch most of what actually
breaks downstream (unsourced numerals; identifiers absent from provided
context), yet nothing mechanical reads a spec today, so enforcement is
exhortative, which is the categorical failure this layer exists to delete.
Build crates/spec-lint: a new workspace member binary crate (add it to both
members and default-members in Cargo.toml:3–16; use only existing
[workspace.dependencies] — serde, serde_json, regex, anyhow, clap, jsonschema —
cargo-deny in the fleet ladder gates any new external dependency, so add
none). Contract: `spec-lint DIR...` where each DIR is a specs/<identity>/
directory; exit 0 iff every rule passes; each defect prints exactly one stderr
line `<file>:<line>: <rule-id>: <message>`; exit 1 on warnings only; exit 2 on
blocking defects; a directory without spec.md is skipped silently. Implement
the structural and model-facing rule classes exactly as specs/README.md v2
enumerates them — the README is the rule list; implement from its bytes, not
from this goal — covering at minimum: section and status-block grammar;
requirement-heading grammar with number-derived anchors; claim-id uniqueness
and ordering; one claim per line (' and ' joining two verbs is a split
defect); provenance marks (unmarked-DECIDE default, BELIEVE:path must resolve
against the working tree, GUESS blocks, [HUMAN-ATTENDED] legal on oracle
gaps); blocking doubt (any unresolved GUESS, BLOCKING unknown, or DECISION-n
fails the lint at Status: ratified); the unsourced-numeral rule; the
out-of-context identifier rule (backticked tokens set-differenced against
BELIEVE'd paths, the vocabulary section, and (NEW) marks); the hedge lexicon;
the e.g./etc. ban; vocabulary drift; empty-section-without-reason. Ship
fixtures: crates/spec-lint/tests/fixtures/golden/ (one clean minimal spec) and
crates/spec-lint/tests/fixtures/must-fail/ with expected-defects.json — the
exact {rule-id: count} map — and at least one deliberately broken artifact per
rule class; unit tests prove the must-fail corpus produces exactly that map
and the golden fixture is clean — a linter never shown to bite is the
--list-only flake attribute reborn (VD-5, F33). Consult
specs/zeta/contracts/claim-line.fixtures.json as the accept/reject corpus for
the claim-line parser. Build no cross-artifact resolution here (worklist,
trace, census, coverage belong to spec-lint-resolution, which depends on this
task) and no flake wiring (spec-lint-flake-check owns flake.nix)."

**deliveredBehaviors:** (1) "spec-lint produces exactly the expected-defects
map on the must-fail corpus and exit 0 on the golden fixture"; (2) "the crate
is a workspace member adding zero new external dependencies".

**readFirst.specSections:** `specs/zeta/spec.md#r1`, `specs/README.md`,
`specs/zeta/contracts/claim-line.fixtures.json`,
`zeta-learnings/raw/instinct-fable.md`, `zeta-learnings/raw/instinct-opus.md`
· **styleReferences:** `crates/spec-build-driver`

**acceptanceCriteria:**
- `rules-bite`: `["bash","-lc","nix develop --command cargo test -p spec-lint 2>&1 | tail -20"]`
- `fixture-corpus-exists`: `["bash","-lc","test -f crates/spec-lint/tests/fixtures/must-fail/expected-defects.json && ls crates/spec-lint/tests/fixtures/must-fail | head -20"]`
- `no-new-deps`: `["bash","-lc","! git diff HEAD~1 -- Cargo.toml | grep -E '^\\+.*(git|version) =' | grep -v spec-lint"]`
- `workspace-green`: `["nix","develop","--command","cargo","test","--workspace"]`

**dependencies:** [] · **conflictDomains:** `crates/spec-lint`, `Cargo.toml`, `Cargo.lock`

### 2. `spec-lint-resolution` — implementation

**goal:** "A spec that cannot be joined to the worklist and the receipts is
prose with anchors: the layer's claim is that spec → worklist → receipts →
release is one citable lineage (specs/README.md v2, Position). Extend
crates/spec-lint with the cross-artifact half, three modes over one parser.
(1) Default lint gains cross-resolution: every readFirst.specSections string
in the identity's worklist (silent-factory-worklists/<identity>.json, when the
file exists) that matches specs/** must resolve to a real file and, where an
anchor is present, a real number-derived anchor in the working tree — the
48-phantom-pointer class, A9/D68, caught mechanically; every task id in
specs/<identity>/trace.json exists in that worklist; every claim id in
trace.json exists in spec.md; every claim is either traced to a task or listed
under an unauthored stage's area — anything else is a defect; trace.json
validates against specs/zeta/contracts/trace.schema.json (the jsonschema
workspace dep). (2) `spec-lint --census DIR`: every claim binds to exactly one
of {a named flake check attribute, a witnessed gate argv, an explicit
[HUMAN-ATTENDED] mark}; zero bindings or two is a defect line —
byte-oracle-or-nothing; coverage becomes an enumeration, not a judgment.
(3) `spec-lint --coverage DIR`: renders the claim ↔ task ↔ acceptance-id ↔
evidence join from trace.json as a markdown table on stdout, byte-stable
across runs, for the operator to hand `tally campaign release` verbatim as the
close-out proof — this deletes the hand-rendered close-out table, which is
this task's D58 price. Add must-fail fixtures: phantom pointer, orphan trace
row, unbound claim, doubly-bound claim; and one clean worklist+trace pair;
extend expected-defects.json accordingly. Do not touch flake.nix
(spec-lint-flake-check owns it); do not read receipts or durable completion
state (release-from-durable-facts is ext2 scope, out of this campaign)."

**deliveredBehaviors:** (1) "a phantom specs/** pointer, an orphan trace row,
and a zero-or-two-bound claim each fail with a named rule id";
(2) "--coverage renders the trace join as a markdown table byte-stable across
runs".

**readFirst.specSections:** `specs/zeta/spec.md#r2`, `specs/README.md`,
`specs/zeta/contracts/trace.schema.json` · **styleReferences:**
`silent-factory-worklists/epsilon-extension.json`

**acceptanceCriteria:**
- `resolution-bites`: `["bash","-lc","nix develop --command cargo test -p spec-lint resolution 2>&1 | tail -20"]`
- `census-exclusive-binding`: `["bash","-lc","nix develop --command cargo test -p spec-lint census 2>&1 | tail -10"]`
- `coverage-golden`: `["bash","-lc","nix develop --command cargo test -p spec-lint coverage 2>&1 | tail -10"]`
- `workspace-green`: `["nix","develop","--command","cargo","test","--workspace"]`

**dependencies:** `["spec-lint-core"]` · **conflictDomains:** `crates/spec-lint`

### 3. `spec-lint-flake-check` — implementation

**goal:** "A bar without a gate is not a bar: the grind's conformance bar
rotted five days as a --list-only flake attribute (VD-5, F33), so the spec
layer's standing consumer must execute from day one and must be proven able to
fail. Add `checks.x86_64-linux.spec-lint` to the checks set at flake.nix:3455:
one runCommand derivation that (1) runs the built spec-lint binary over every
committed specs/<identity>/ directory containing a spec.md (with --worklist
silent-factory-worklists/<identity>.json when that file exists; spec-less
evidence-only dirs like specs/epsilon-extension/ are skipped), failing the
build on any blocking defect, and (2) inside the same derivation runs the
binary against crates/spec-lint/tests/fixtures/must-fail/ asserting exit 2
with defect codes exactly matching expected-defects.json — a green check
therefore witnesses both 'the corpus is clean' and 'the tool can bite' in one
attribute. Because test/fleet-gate.sh:254 already runs `nix flake check -L
--keep-going`, this single attribute gives the layer fleet-tier standing
coverage on every gated head with zero fleet-gate edits — do not edit
test/fleet-gate.sh. Style: follow the existing runCommand checks
(spec-build-driver-tests, module-layer); specs/ and the fixture corpus enter
as source inputs so the check re-runs when either changes. If any committed
spec under specs/ fails the lint at first contact, do not edit the spec —
spec bytes are authority; report the defect lines verbatim in your final
message and let the acceptance record carry them (record, don't fix; the
operator regenerates and re-ratifies)."

**deliveredBehaviors:** (1) "nix build .#checks.x86_64-linux.spec-lint fails
on a corpus defect and on a must-fail fixture that unexpectedly passes";
(2) "fleet-gate inherits the check through nix flake check with zero
fleet-gate edits".

**readFirst.specSections:** `specs/zeta/spec.md#r1`, `specs/zeta/spec.md#r3`,
`specs/README.md` · **styleReferences:** `flake.nix` (checks set near :3455)

**acceptanceCriteria:**
- `attribute-builds`: `["bash","-lc","nix build --no-link .#checks.x86_64-linux.spec-lint 2>&1 | tail -5"]`
- `bite-proof-in-derivation`: `["bash","-lc","grep -n 'must-fail' flake.nix | head -3"]`
- `no-fleet-gate-edit`: `["bash","-lc","git diff --name-only HEAD~1 | { ! grep -q 'test/fleet-gate.sh'; }"]`
- `workspace-green`: `["nix","develop","--command","cargo","test","--workspace"]`

**dependencies:** `["spec-lint-resolution"]` · **conflictDomains:** `flake.nix`

### 4. `spec-layer-skills-amend` — implementation

**goal:** "The equipment must be committed bytes (F39/42/43): the two campaign
skills now carry ext0's doctrine but say nothing about the authority plane, so
the sitting's spec steps live only in zeta-learnings prose, which decays.
Amend three skills in their existing voice; no new files.
skills/assign-tally/SKILL.md gains the spec-layer authoring rules: when a
governing spec exists, task goal text cites claim ids and evidence ids instead
of restating them; readFirst.specSections point at number-derived anchors of
the form specs/<identity>/spec.md#rN that exist at the authority revision; the
sitting appends specs/<identity>/trace.json rows in the same commit as the
worklist stage; the governing spec appears in no task's conflictDomains and no
lane writes it. skills/campaign-operator/SKILL.md gains one interim-close
step: render the coverage table with `spec-lint --coverage specs/<identity>`
and hand it to release as part of the operator-authored intent (release
renders intent verbatim; the table is the close-out proof — the hand-rendered
table is deleted, this task's D58 price, together with the deleted authoring
rule 'check that goals restate requirements').
skills/author-spec/SKILL.md: true up every reference to the tool against the
merged tree — binary name, the checks.x86_64-linux.spec-lint attribute,
fixture paths — the skill was committed before the tool existed and each
pointer must now resolve. These are additive sentences and pointer truth-ups;
change nothing else in the three files."

**deliveredBehaviors:** (1) "assign-tally states the cite-don't-restate,
anchor, trace-row, and governing-spec-ownership rules"; (2)
"campaign-operator's close checklist names the --coverage command;
author-spec's pointers all resolve".

**readFirst.specSections:** `specs/zeta/spec.md#r4`,
`skills/author-spec/SKILL.md` · **styleReferences:**
`skills/assign-tally/SKILL.md`, `skills/campaign-operator/SKILL.md`

**acceptanceCriteria:**
- `authoring-rules-present`: `["bash","-lc","grep -n 'spec.md#r' skills/assign-tally/SKILL.md | head -2 && grep -in 'trace.json' skills/assign-tally/SKILL.md | head -2"]`
- `close-step-present`: `["bash","-lc","grep -n 'spec-lint --coverage' skills/campaign-operator/SKILL.md | head -2"]`
- `author-spec-pointers-resolve`: `["bash","-lc","grep -n 'checks.x86_64-linux.spec-lint' skills/author-spec/SKILL.md | head -2"]`

**dependencies:** `["spec-lint-flake-check"]` · **conflictDomains:**
`skills/assign-tally`, `skills/campaign-operator`, `skills/author-spec`

### 5. `doc-anchor-regrammar` — implementation

**goal:** "The shipped documentation taught the spec-pointer genre before the
layer existed and used a name-derived slug: doc/src/flows/campaigns.md:1341
shows specs/001-crm/spec.md#customer-model, but the ratified grammar derives
anchors from the claim number only, precisely so retitling breaks nothing, and
numeric directory prefixes are dead — identity is the join key,
specs/<identity>/ (specs/README.md v2). Re-render that example, and every
sibling occurrence a grep of doc/ for 'specs/' pointers finds, to the
committed grammar so the shipped docs and the linter agree. Touch only
documentation; the doc flake check must stay green."

**deliveredBehaviors:** (1) "no doc page teaches a name-derived spec anchor or
a numeric specs/ prefix"; (2) "the doc check builds".

**readFirst.specSections:** `specs/zeta/spec.md#r4`, `specs/README.md` ·
**styleReferences:** `doc/src/flows/campaigns.md`

**acceptanceCriteria:**
- `old-grammar-gone`: `["bash","-lc","! grep -rn 'specs/001-' doc/src && ! grep -rn '#customer-model' doc/src"]`
- `doc-builds`: `["bash","-lc","nix build --no-link .#checks.x86_64-linux.doc 2>&1 | tail -5"]`

**dependencies:** [] · **conflictDomains:** `doc`

### 6. `zeta-chapter-gate` — checkpoint

`"argv": ["bash","-lc","test/fleet-gate.sh \"$(git rev-parse HEAD)\" && exec test/final-bar/run \"$PWD\""]`
(identical to ext0's chapter gate, epsilon-extension.json:349–353),
`runtimeMaxSec: 10800`, **dependencies:** all five tasks. By construction this
head's `nix flake check` executes `spec-lint` — the campaign closes through
the bar it built.

## Operator acts — now to zeta close (9 planned acts)

All commits at a keyboard on the base branch. The checkout is currently a
**detached HEAD at e921cccc** — A1 begins by switching to/creating the branch
that `main` publication expects, or admission reads a base tip without any of
this. Steers/approvals driven by escalations are unplanned and uncounted.

**Anytime, gates nothing until ext1:** R4, the one-word `forge:"local"`
remote-semantics ruling.

**Tonight (Aug 14):**

- **A1 — E6, commit the record.** First verify `git status skills/` is clean.
  One commit, push: `aug10-midday-session.md`, `AUG12-DAYRUN-HANDOFF.md`,
  `AUG12-HANDOFF.md`, `AUG12-overnight.md`, `AUG13-RUN.md`,
  `AUG14-LEARNINGS.md`, `AUGUST-11-OVERNIGHT.md`, `AUGUST-12-LEARNINGS.md`,
  `aug12-campaign-prep/`, `EPSILON-EXTENSION.md`,
  `silent-factory-worklists/epsilon-extension.json`, all of `zeta-learnings/`.
- **A2 — the zeta authoring commit.** One commit, push: `specs/README.md` v2,
  `specs/constitution.md` v2, `specs/epsilon-extension/evidence/` (eight
  ledgers, recovered from the epsilon session scratchpad — already copied into
  the tree), `specs/zeta/` (spec.md Status: proposed, trace.json empty,
  contracts/), `skills/author-spec/SKILL.md`, `ZETA.md`. NOT in this commit:
  `zeta.json` (authored at A7 against the observed post-ext0 tree). *Fallback:*
  if time is short, A2 may slide to any point before A7 — ext0's own readFirst
  needs only A1; the cost is ext0 running with unresolvable evidence citations
  until A2 lands.
- **A3 — Deploy-3.** Deploy the fleet to the e921cccc generation; from this
  moment the Rust driver and the full epsilon-release surface grade.
- **A4 — Arm ext0.** `tally campaign arm mecattaf/tally.nix
  silent-factory-worklists/epsilon-extension.json`. Walk away; ext0 is
  tonight's overnight and tomorrow's working day.

**Aug 15 (ext0 boundary):**

- **A5 — ext0 interim close** (composite, by checklist — the last
  hand-performed close). Quiescent → `release --plan` → probe → `release` →
  nothing after. Whether this close disarms the identity or leaves it
  registered for ext1 follows the interim-close doctrine ext0's own
  `authoring-doctrine-skills` just committed — read it as merged, not as
  remembered.
- **A6 — boundary deploy.** Deploy the fleet to the ext0-integrated pin (from
  here the post-ceremony machinery grades).
- **A7 — the zeta boundary sitting** (~45 min, keyboard, one commit). In
  order: run the falsity pass on `specs/zeta/spec.md` against the observed
  post-ext0 tree ("which of these statements about my codebase are false?");
  regenerate any falsified section (never line-edit); drain DECISION-1
  (steward) and UNKNOWN-1; flip Status: proposed → ratified; finalize
  `silent-factory-worklists/zeta.json` from this file's task specs against the
  observed tree; append the sitting trace rows to `specs/zeta/trace.json`
  (claim ↔ task ↔ acceptance ids, per the schema); verify every readFirst path
  exists at the base tip; rehearse admission (every gate preflight argv
  verbatim, in a pristine worktree); commit spec ratification + worklist +
  trace rows together, push.
- **A8 — Arm zeta.** `tally campaign arm mecattaf/tally.nix
  silent-factory-worklists/zeta.json`. Overnight #2 (~7–10h).

**Aug 16:**

- **A9 — zeta close** (composite). Quiescent → render the coverage table with
  the campaign's own deliverable: `spec-lint --coverage specs/zeta` (first
  tool-rendered close-out — the hand-rendered table is dead) → hand the table
  plus the spec's Outcome section to release as the operator intent →
  `release --plan` → probe → `release` → `disarm` last, nothing after. The
  ext1 sitting that follows (outside zeta's scope) is the first stage ever
  derived per `skills/author-spec` with real anchors — zeta's actual finish
  line.

**Count: 9 planned acts** (A5 and A9 composite), two campaigns, against the
destination's ~16-act bar for one campaign of epsilon's size.

## Risks

- **ext0 slips → zeta slips a night.** Zeta needs ext0 quiescence + a deployed
  pin, not ext0 success. If ext0 latches escalated, zeta may still arm once
  every lane owning `flake.nix`/`skills/`/workspace domains is dead — not
  before. If ext0 is mid-recovery at A7 time, wait.
- **Shakedown risk.** Zeta is the first campaign graded by the post-ext0
  machinery (epoch budgets, outcome envelopes, judge verdicts, subject
  adoption). A machinery fault can burn attempts; the stale-pin attribution
  rule applies before blaming any zeta commit. Deliberate: better to shake
  down under one crate of cargo than under ext1's self-modifying last mile.
- **First-contact lint defects.** The corpus the flake check lints day-one is
  exactly `specs/zeta/` (ruling 2 shrank the exposure); it was authored with
  the rule list in hand, but the tool did not exist to check it. Residual
  defects surface as verbatim stderr lines in `spec-lint-flake-check`'s task
  report and cost one operator regeneration commit — never lane edits to spec
  bytes.
- **cargo-deny bite.** A lane adding any external dependency trips the fleet
  ladder hours later at the chapter gate; the goal forbids it and the
  `no-new-deps` acceptance catches it at merge time.
- **Chapter-gate duration.** fleet-gate now carries the executing final bar
  plus a heavier `nix flake check`. The 10800s cap should hold; A7 re-checks
  it against the merged `final-bar-executes` reality.
