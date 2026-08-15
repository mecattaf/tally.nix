## FIELD: sequencing_ruling

# Sequencing ruling: (b) FOLLOW — refined to "follow ext0, precede ext1"

**Ruling: zeta runs as a second campaign on its own fresh identity (`zeta`), armed at the ext0→ext1 boundary — after epsilon-extension's ext0 interim close and the boundary deploy, before the ext1 sitting authors anything.** Not absorbed, not beside, and not deferred to the full epsilon-extension close.

## Why not (a) absorb

- **The identity/spec join forbids it.** Directory name = campaign identity (specs/README.md §artifact-set). Zeta's lanes must *write* `specs/epsilon-extension/`-adjacent artifacts and the spec-layer machinery; a campaign can never be governed by the surface it authors (lens-seams Seam 4: "a blanket entry would make the first real spec-layer campaign refuse itself"). Absorbing zeta under the epsilon-extension identity makes the first spec instance instantly false — it names none of zeta's deliverables — or forces amending a just-ratified program mid-run.
- **The chapter gate closes over the ratified nine.** `chapter-gate` (epsilon-extension.json:346–366) depends on exactly the ext0 task set; appending tasks means amending the checkpoint of a ratified stage and a hand re-arm mid-chain — churn on the authority file during the one run the supervisor called "the last ceremonial campaign."
- **Deny-list scoping (R3, EPSILON-EXTENSION.md:41) is per governing spec of the armed identity.** That scoping only works if zeta *has* its own identity. Absorption erases the distinction the whole ext1 design depends on.

## Why not (c) beside (tonight, second identity)

- **Cross-campaign collision blindness.** Ownership certification and conflictDomains are per-campaign. Zeta must touch `flake.nix` (the spec-lint check attribute) and `Cargo.toml`/`Cargo.lock` (new workspace member, Cargo.toml:3–16) while ext0's `final-bar-executes` owns `flake.nix` (epsilon-extension.json:282) and every ext0 gate runs `cargo test --workspace`. Two identities interleaving merges on one base with no shared ownership map is a race the machinery has never run.
- **Width is already spent.** Effective width 2 on ext0 (the supervisor's chain-not-fan statement); a beside-campaign contends for the same host agent-slot pool and stretches both runs.
- **The 3-hour chapter gate** (runtimeMaxSec 10800) audits an integration head; a sibling campaign fast-forwarding the base mid-gate invalidates it.

## Why not full-follow (after ext2)

- **ext1's sitting is the payoff moment.** The trajectory: "each next stage is authored against the observed tree at the boundary." If zeta lands *before* the ext1 sitting, ext1 becomes the first worklist derived per `skills/author-spec` with real `specs/**` anchors, trace rows, and the lint rehearsal — the two-way loop flying, not just parked. Waiting until after ext2 means ext1 and ext2 are authored anchors-less, and the priced ext-era deepenings (scoped deny-list entry, admission pointer resolution, escalation citation rendering, release coverage from durable facts — lens-seams §deepenings) would have no committed spec layer to point at when their stages are authored.
- **Shakedown ordering.** Zeta's cargo is low-risk (one new crate, one check attribute, doc/skill edits — the zeta floor needs zero machinery changes). Running it on the freshly deployed post-ext0 pin makes zeta the shakedown campaign for epoch budgets, outcome envelopes, judge verdicts, and subject adoption *before* ext1's self-modifying last mile depends on them.

## The dependency picture

Tonight belongs to ext0 as ratified (that worklist already exists and is not mine to alter). Zeta's authored bytes (spec instances, v2 docs, author-spec skill — the other designer's half) land **tonight** via operator commits, inert to the running machinery by construction (spec out of the epoch key, lens-seams Seam 2; machinery decisions never read spec bytes, Seam 4). Zeta's *built* bytes (the lint, the check attribute, the wiring) land as the second overnight. Zeta waits on ext0 **quiescence**, not ext0 success — the floor has no dependency on any ext0 deliverable; it needs only dead lanes on `flake.nix`/`skills/` and a deployed pin.

## FIELD: stage_plan

# Zeta stage plan

Zeta is one stage plus a chapter gate, mirroring ext0's shape. The structural split that governs everything: **authored artifacts land at sittings via operator commits (ratification is an ordinary operator commit; Fable/Opus is the only trusted spec author; no machinery hand touches authority files); machine-buildable artifacts land via Codex lanes.** A Codex lane transcribing a document whose full text already exists in its goal would be a transcription act by proxy — the exact class the destination deletes.

## Stage Z-A — the authored plane (lands TONIGHT, operator commits A1/A2, no lanes)

Content is the **other designer's half** (INTERFACE-2/3/4/5); placement and timing are mine:

1. `specs/README.md` **v2** — slimmed to lintable law; the README *is* the lint rule list (INTERFACE-2).
2. `specs/constitution.md` **v2** — accepted critiques applied (trace out of spec.md; deciding/rendering line; scoped deny-list; the freeze/append article; procedures moved out).
3. `specs/epsilon-extension/` — instance one: `spec.md` recast from EPSILON-EXTENSION.md v2 (Status: ratified by the A2 commit itself), `trace.json` with downward rows for the ten ext0 tasks, `evidence/` holding the five Aug-14 excavation ledgers copied in from the session scratchpad. This is what makes the live worklist's citations (CA-3, F37, VD-5, PA-25/26…) resolvable at ext0's authority revision — E6 territory per 00-INDEX.
4. `specs/zeta/` — the **governing spec of the zeta identity**: `spec.md` (Status: **proposed** tonight, ratified at the Z1 boundary sitting — see DECISION-5), `trace.json` rows for the five zeta tasks, `contracts/trace.schema.json` (INTERFACE-3). Requirement areas I bind anchors to (final numbering/wording theirs, INTERFACE-1): r1 the lint engine + must-fail corpus; r2 cross-resolution + census + coverage; r3 the flake check attribute as standing consumer; r4 the skills carry the doctrine; r5 the docs teach the real anchor grammar; r6 unchanged behavior (worklist schema stays closed; no machine decision reads spec bytes; fleet-gate ladder untouched except through `nix flake check`).
5. `skills/author-spec/SKILL.md` — the sitting procedure + analyze pass + grind checklist, migrated out of the constitution (INTERFACE-4).
6. `ZETA.md` — the session lead's compilation; carries these task specs verbatim as the authoring source for `silent-factory-worklists/zeta.json`, which is **not** committed tonight (A12: author against the observed tree — the worklist finalizes at Z1 against the post-ext0 tree).

## Stage Z-0 — the built plane (Codex lanes, overnight #2), dependency order, cheap-first

Dispatch order (width 2 at start, then a chain — this is a build stage that is domain-chained; estimate as a chain):

```
spec-lint-core ──► spec-lint-resolution ──► spec-lint-flake-check ──► spec-layer-skills-amend ──┐
doc-anchor-regrammar (parallel, independent) ───────────────────────────────────────────────────┴─► zeta-chapter-gate
```

- **spec-lint-core** — `crates/spec-lint` workspace member; grammar parser; structural + model-facing rules per README v2; must-fail fixture corpus. D58: deletes the manual analyze pass and pointer-checking by eye.
- **spec-lint-resolution** — cross-artifact half: worklist↔spec↔trace resolution, `--census` (byte-oracle-or-nothing as enumeration), `--coverage` (renders the close-out table). D58: deletes the hand-maintained trace check and the hand-rendered close-out table.
- **spec-lint-flake-check** — `checks.x86_64-linux.spec-lint` in flake.nix (checks set at flake.nix:3455); lints the committed corpus AND proves the linter bites its must-fail fixtures inside the same derivation. Because test/fleet-gate.sh:254 already runs `nix flake check -L --keep-going`, this single attribute is fleet-tier standing coverage with **zero fleet-gate edits** — the standing consumer that keeps the layer alive (A15). D58: deletes the "verify the linter still bites" doubt and any fleet-gate edit.
- **spec-layer-skills-amend** — the spec-layer sentences into `assign-tally` and `campaign-operator` (safe now: ext0's `authoring-doctrine-skills` has closed), pointer truth-up in `author-spec`. D58: deletes "check the goal restates the requirement" and "hand-render the coverage table" from the operator's checklist.
- **doc-anchor-regrammar** — S13: doc/src/flows/campaigns.md:1341's `specs/001-crm/spec.md#customer-model` re-rendered to the ratified number-derived grammar.
- **zeta-chapter-gate** — checkpoint: fleet-gate + final bar on the integration head, argv identical to ext0's (epsilon-extension.json:349–353). By this point fleet-gate's `nix flake check` includes spec-lint: **the chapter gate that closes zeta is itself the first standing execution of the spec layer's own bar.**

## Folded-in seams, bugs, and open items

- **Zeta floor, all six items**: specs dir (Z-A), lint as flake check (Z-0), real readFirst anchors (`zeta.json` is the first consumer — every task's specSections are `specs/zeta/spec.md#rN` anchors), trace rows at sittings (A2 + Z1), coverage handed to release (A9 uses `--coverage`), governing spec out of conflict domains (`specs/zeta` appears in **no** task's conflictDomains; enforced tonight by authoring discipline, mechanized in ext1 by the scoped deny-list).
- **Left open by EPSILON-EXTENSION, closed here**: the scratchpad evidence ledgers committed (A2); specs v1→v2 supersession; EPSILON-EXTENSION.md gains a freeze-pointer header to the recast instance (DECISION-6); the upward trace half for ext0 written by hand at its interim close (the last ceremonial close), by tool at zeta's.
- **Deliberately left for ext1/ext2 sittings** (they are machinery, owned by the epsilon-extension program, now authorable *with* real anchors): `specs/<armed-identity>/**` joins the R3 deny-list; admission resolves `specs/**` pointers; escalation reports resolve citations; release renders coverage from durable completion facts; `tally spec` CLI verbs (diff/questions); authored caps leaving the schema.

## FIELD: task_specs

# Task specs — silent-factory-worklists/zeta.json (finalized and committed at Z1)

**Campaign header:** `schemaVersion: 1`; name `zeta`; `maxTasks: 8`; `maxParallel: 2`; `steward:` — DECISION-1 (post-ext0 catalog role name; draft `"narrator"` as schema placeholder, settled at Z1 against the observed tree). **Gates:** the four ratified template gates copied **verbatim** from epsilon-extension.json:8–37 — `driver-suite` (python3 test/spec_build_driver_test.py, 900s), `cargo-tests` (nix develop cargo test --workspace, 3600s), `clippy` (-D warnings, 1800s), `flake-build-subset` (built attributes spec-build-driver-tests, module-layer, campaign-runtime, 1800s). The spec-lint attribute is deliberately NOT a campaign gate — it would fail every merge until spec-lint-flake-check lands; the chapter gate's `nix flake check` covers it from the moment it exists. Caps stay authored (pre-ext1 schema requires them) — flagged against the zero-required-numbers destination.

**Width/wall-clock:** effective width 2 for ~40 minutes (core ∥ doc), then a chain of 4 plus the gate. Estimate: core 2.5–3.5h → resolution 1.5–2.5h → flake-check 0.75–1.5h → skills 0.5–1h → chapter gate 1–2.5h ≈ **7–10h total; one overnight** (arm ~21:00 Aug 15 → close Aug 16 morning).

**INTERFACE note:** every `#rN` anchor below assumes the other designer's number-derived anchor grammar (INTERFACE-1); pointer strings regenerate mechanically at Z1 if their final grammar differs. All readFirst paths exist at Z1's authority revision (landed by A1/A2).

---

## 1. `spec-lint-core` — implementation

**goal (verbatim):** "The authority plane's enforcement engine does not exist: specs/README.md v2 defines the lintable claim grammar and rule list, and both authoring models specified — verbatim, in zeta-learnings/raw/instinct-fable.md and raw/instinct-opus.md — the two rules that catch most of what actually breaks downstream (unsourced numerals; identifiers absent from provided context), yet nothing mechanical reads a spec today, so enforcement is exhortative, which is the categorical failure this layer exists to delete. Build crates/spec-lint: a new workspace member binary crate (add it to both members and default-members in Cargo.toml:3–16; use only existing [workspace.dependencies] — serde, serde_json, regex, anyhow, clap, sha2 — cargo-deny in the fleet ladder gates any new external dependency, so add none). Contract: `spec-lint DIR...` where each DIR is a specs/<identity>/ directory; exit 0 iff every rule passes; each defect prints exactly one stderr line `<file>:<line>: <rule-id>: <message>`; exit 1 on any defect; exit 2 on unreadable input. Implement the structural and model-facing rule classes exactly as specs/README.md v2 enumerates them — the README is the rule list; implement from its bytes, not from this goal — covering at minimum: section and status-block grammar; requirement-heading grammar with number-derived anchors; claim-id uniqueness and ordering; one claim per line (' and ' joining two verbs is a split defect); provenance marks (DECIDE default, BELIEVE:path must resolve against the working tree, GUESS blocks, [HUMAN-ATTENDED] legal on oracle gaps); blocking doubt (any unresolved GUESS, BLOCKING unknown, or DECISION-n fails the lint); the unsourced-numeral rule; the out-of-context identifier rule (backticked tokens set-differenced against BELIEVE'd paths, the vocabulary section, and NEW: marks); the hedge lexicon; the e.g./etc. ban; vocabulary drift; empty-section-without-reason. Ship fixtures: crates/spec-lint/fixtures/pass/ (one clean minimal spec) and crates/spec-lint/fixtures/must-fail/ with at least one deliberately broken spec per rule class; unit tests prove every rule bites its fixture by rule-id and the pass fixture is clean — a linter never shown to bite is the --list-only flake attribute reborn (VD-5, F33). Build no cross-artifact resolution here (worklist, trace, census, coverage belong to spec-lint-resolution, which depends on this task) and no flake wiring (spec-lint-flake-check owns flake.nix)."

**deliveredBehaviors:** (1) "spec-lint fails every must-fail fixture with its named rule id and passes the pass fixture"; (2) "the crate is a workspace member adding zero new external dependencies".

**readFirst.specSections:** `specs/zeta/spec.md#r1`, `specs/README.md`, `zeta-learnings/raw/instinct-fable.md`, `zeta-learnings/raw/instinct-opus.md` · **styleReferences:** `crates/spec-build-driver`

**acceptanceCriteria:**
- `rules-bite`: every rule class has a must-fail fixture the tests prove it rejects — `["bash","-lc","nix develop --command cargo test -p spec-lint 2>&1 | tail -20"]`
- `fixture-corpus-exists`: `["bash","-lc","ls crates/spec-lint/fixtures/must-fail | head -20 && test \"$(ls crates/spec-lint/fixtures/must-fail | wc -l)\" -ge 8"]`
- `no-new-deps`: `["bash","-lc","! git diff HEAD~1 -- Cargo.toml | grep -E '^\\+.*(git|version) =' | grep -v spec-lint"]`
- `workspace-green`: `["nix","develop","--command","cargo","test","--workspace"]`

**dependencies:** [] · **conflictDomains:** `crates/spec-lint`, `Cargo.toml`, `Cargo.lock`

---

## 2. `spec-lint-resolution` — implementation

**goal (verbatim):** "A spec that cannot be joined to the worklist and the receipts is prose with anchors: the layer's claim is that spec → worklist → receipts → release is one citable lineage (specs/README.md v2, Position). Extend crates/spec-lint with the cross-artifact half, three modes over one parser. (1) Default lint gains cross-resolution: every readFirst.specSections string in the identity's worklist (silent-factory-worklists/<identity>.json) that matches specs/** must resolve to a real file and, where an anchor is present, a real number-derived anchor in the working tree — the 48-phantom-pointer class, A9/D68, caught mechanically; every task id in specs/<identity>/trace.json exists in that worklist; every claim id in trace.json exists in spec.md; every claim is either traced to a task or listed under an unauthored stage's area — anything else is a defect; trace.json validates against specs/zeta/contracts/trace.schema.json. (2) `spec-lint --census DIR`: every claim binds to exactly one of {a named flake check attribute, a witnessed gate argv, an explicit [HUMAN-ATTENDED] mark}; zero bindings or two is a defect line — byte-oracle-or-nothing; coverage becomes an enumeration, not a judgment. (3) `spec-lint --coverage DIR`: renders the claim ↔ task ↔ acceptance-id ↔ evidence join from trace.json as a markdown table on stdout, for the operator to hand `tally campaign release` verbatim as the close-out proof — this deletes the hand-rendered close-out table, which is this task's D58 price. Add must-fail fixtures: phantom pointer, orphan trace row, unbound claim, doubly-bound claim; and one clean worklist+trace pair. Do not touch flake.nix (spec-lint-flake-check owns it); do not read receipts or durable completion state (release-from-durable-facts is ext2 scope, out of this campaign)."

**deliveredBehaviors:** (1) "a phantom specs/** pointer, an orphan trace row, and a zero-or-two-bound claim each fail with a named rule id"; (2) "--coverage renders the trace join as a markdown table byte-stable across runs".

**readFirst.specSections:** `specs/zeta/spec.md#r2`, `specs/README.md`, `specs/zeta/contracts/trace.schema.json`, `specs/epsilon-extension/trace.json` · **styleReferences:** `silent-factory-worklists/epsilon-extension.json`

**acceptanceCriteria:**
- `resolution-bites`: `["bash","-lc","nix develop --command cargo test -p spec-lint resolution 2>&1 | tail -20"]`
- `census-exclusive-binding`: `["bash","-lc","nix develop --command cargo test -p spec-lint census 2>&1 | tail -10"]`
- `coverage-golden`: a golden-fixture test pins the rendered table — `["bash","-lc","nix develop --command cargo test -p spec-lint coverage 2>&1 | tail -10"]`
- `workspace-green`: `["nix","develop","--command","cargo","test","--workspace"]`

**dependencies:** `["spec-lint-core"]` · **conflictDomains:** `crates/spec-lint`

---

## 3. `spec-lint-flake-check` — implementation

**goal (verbatim):** "A bar without a gate is not a bar: the grind's conformance bar rotted five days as a --list-only flake attribute (VD-5, F33), so the spec layer's standing consumer must execute from day one and must be proven able to fail. Add `checks.x86_64-linux.spec-lint` to the checks set at flake.nix:3455: one runCommand derivation that (1) runs the built spec-lint binary over every committed specs/<identity>/ directory and its declared worklist joins, failing the build on any defect, and (2) inside the same derivation runs the binary against each fixture under crates/spec-lint/fixtures/must-fail/ asserting nonzero exit — a green check therefore witnesses both 'the corpus is clean' and 'the tool can bite' in one attribute. Because test/fleet-gate.sh:254 already runs `nix flake check -L --keep-going`, this single attribute gives the layer fleet-tier standing coverage on every gated head with zero fleet-gate edits — do not edit test/fleet-gate.sh. Style: follow the existing runCommand checks (spec-build-driver-tests, flake.nix:3472–3486); specs/ and the fixture corpus enter as source inputs so the check re-runs when either changes. If any committed spec under specs/ fails the lint at first contact, do not edit the spec — spec bytes are authority; report the defect lines verbatim in your final message and let the acceptance record carry them (record, don't fix; the operator regenerates and re-ratifies)."

**deliveredBehaviors:** (1) "nix build .#checks.x86_64-linux.spec-lint fails on a corpus defect and on a must-fail fixture that unexpectedly passes"; (2) "fleet-gate inherits the check through nix flake check with zero fleet-gate edits".

**readFirst.specSections:** `specs/zeta/spec.md#r3`, `specs/README.md` · **styleReferences:** `flake.nix` (checks set near :3455)

**acceptanceCriteria:**
- `attribute-builds`: `["bash","-lc","nix build --no-link .#checks.x86_64-linux.spec-lint 2>&1 | tail -5"]`
- `bite-proof-in-derivation`: `["bash","-lc","grep -n 'must-fail' flake.nix | head -3"]`
- `no-fleet-gate-edit`: `["bash","-lc","git diff --name-only HEAD~1 | { ! grep -q 'test/fleet-gate.sh'; }"]`
- `workspace-green`: `["nix","develop","--command","cargo","test","--workspace"]`

**dependencies:** `["spec-lint-resolution"]` · **conflictDomains:** `flake.nix`

---

## 4. `spec-layer-skills-amend` — implementation

**goal (verbatim):** "The equipment must be committed bytes (F39/42/43): the two campaign skills now carry ext0's doctrine but say nothing about the authority plane, so the sitting's spec steps live only in zeta-learnings prose, which decays. Amend three skills in their existing voice; no new files. skills/assign-tally/SKILL.md gains the spec-layer authoring rules: when a governing spec exists, task goal text cites claim ids and evidence ids instead of restating them; readFirst.specSections point at number-derived anchors of the form specs/<identity>/spec.md#rN that exist at the authority revision; the sitting appends specs/<identity>/trace.json rows in the same commit as the worklist stage; the governing spec appears in no task's conflictDomains and no lane writes it. skills/campaign-operator/SKILL.md gains one interim-close step: render the coverage table with `spec-lint --coverage specs/<identity>` and hand it to release as part of the operator-authored intent (release renders intent verbatim; the table is the close-out proof — the hand-rendered table is deleted, this task's D58 price, together with the deleted authoring rule 'check that goals restate requirements'). skills/author-spec/SKILL.md: true up every reference to the tool against the merged tree — binary name, the checks.x86_64-linux.spec-lint attribute, fixture paths — the skill was committed before the tool existed and each pointer must now resolve. These are additive sentences and pointer truth-ups; change nothing else in the three files."

**deliveredBehaviors:** (1) "assign-tally states the cite-don't-restate, anchor, trace-row, and governing-spec-ownership rules"; (2) "campaign-operator's close checklist names the --coverage command; author-spec's pointers all resolve".

**readFirst.specSections:** `specs/zeta/spec.md#r4`, `skills/author-spec/SKILL.md` · **styleReferences:** `skills/assign-tally/SKILL.md`, `skills/campaign-operator/SKILL.md`

**acceptanceCriteria:**
- `authoring-rules-present`: `["bash","-lc","grep -n 'spec.md#r' skills/assign-tally/SKILL.md | head -2 && grep -in 'trace.json' skills/assign-tally/SKILL.md | head -2"]`
- `close-step-present`: `["bash","-lc","grep -n 'spec-lint --coverage' skills/campaign-operator/SKILL.md | head -2"]`
- `author-spec-pointers-resolve`: `["bash","-lc","grep -n 'checks.x86_64-linux.spec-lint' skills/author-spec/SKILL.md | head -2"]`

**dependencies:** `["spec-lint-flake-check"]` · **conflictDomains:** `skills/assign-tally`, `skills/campaign-operator`, `skills/author-spec`

---

## 5. `doc-anchor-regrammar` — implementation

**goal (verbatim):** "The shipped documentation taught the spec-pointer genre before the layer existed and used a name-derived slug: doc/src/flows/campaigns.md:1341 shows specs/001-crm/spec.md#customer-model, but the ratified grammar derives anchors from the claim number only, precisely so retitling breaks nothing, and numeric directory prefixes are dead — identity is the join key, specs/<identity>/ (specs/README.md v2). Re-render that example, and every sibling occurrence a grep of doc/ for 'specs/' pointers finds, to the committed grammar so the shipped docs and the linter agree. Touch only documentation; the doc flake check must stay green."

**deliveredBehaviors:** (1) "no doc page teaches a name-derived spec anchor or a numeric specs/ prefix"; (2) "the doc check builds".

**readFirst.specSections:** `specs/zeta/spec.md#r5`, `specs/README.md` · **styleReferences:** `doc/src/flows/campaigns.md`

**acceptanceCriteria:**
- `old-grammar-gone`: `["bash","-lc","! grep -rn 'specs/001-' doc/src && ! grep -rn '#customer-model' doc/src"]`
- `doc-builds`: `["bash","-lc","nix build --no-link .#checks.x86_64-linux.doc 2>&1 | tail -5"]`

**dependencies:** [] · **conflictDomains:** `doc`

---

## 6. `zeta-chapter-gate` — checkpoint

`"argv": ["bash","-lc","test/fleet-gate.sh \"$(git rev-parse HEAD)\" && exec test/final-bar/run \"$PWD\""]` (identical to ext0's chapter gate, epsilon-extension.json:349–353), `runtimeMaxSec: 10800`, **dependencies:** all five tasks. By construction this head's `nix flake check` executes `spec-lint` — the campaign closes through the bar it built.

## FIELD: operator_acts

# Operator acts — now to zeta close (9 planned acts)

All commits at a keyboard on the base branch (the checkout is currently a detached HEAD at e921cccc — A1 begins by switching to/creating the branch that `main` publication expects). Steers/approvals driven by escalations are unplanned and uncounted; the plan assumes the clean path.

**Anytime, gates nothing until ext1:** R4, the one-word `forge:"local"` remote-semantics ruling (EPSILON-EXTENSION.md R4).

## Tonight (Aug 14)

- **A1 — E6, commit the record (+ verify pre-step 2).** First confirm `git status skills/` is clean (the stale Aug-12 skills edits appear already resolved — verify, don't assume). Then one commit, push: `aug10-midday-session.md`, `AUG12-DAYRUN-HANDOFF.md`, `AUG12-HANDOFF.md`, `AUG12-overnight.md`, `AUG13-RUN.md`, `AUG14-LEARNINGS.md`, `AUGUST-11-OVERNIGHT.md`, `AUGUST-12-LEARNINGS.md`, `aug12-campaign-prep/`, `EPSILON-EXTENSION.md`, `silent-factory-worklists/epsilon-extension.json`, and **all of `zeta-learnings/`** (00–11 plus `raw/` — the citations in every zeta goal resolve through this commit). Deliberately excluded: `specs/` (v1 drafts; superseded by A2 — never commit v1 minutes before its supersession).
- **A2 — the zeta ratification commit.** One commit, push: `specs/README.md` v2, `specs/constitution.md` v2, `specs/epsilon-extension/` (spec.md recast — Status: ratified by this commit — trace.json downward rows for the ten ext0 tasks, `evidence/` with the five Aug-14 ledgers **copied in from the session scratchpad** — they exist nowhere in the repo today), `specs/zeta/` (spec.md Status: proposed, trace.json, contracts/trace.schema.json), `skills/author-spec/SKILL.md`, `ZETA.md`, plus the freeze-pointer header line on `EPSILON-EXTENSION.md` (DECISION-6). NOT in this commit: `zeta.json` (authored against the observed post-ext0 tree at Z1, per A12). *Fallback:* if compilation runs past arming time, A2 may slide to any point before A7 — ext0's own readFirst needs only A1; the cost is that ext0 runs with unresolvable evidence citations until A2 lands.
- **A3 — Deploy-3.** Prompted deploy of the fleet to the e921cccc generation; from this moment the Rust driver and the full epsilon-release surface grade.
- **A4 — Arm ext0.** `tally campaign arm mecattaf/tally.nix silent-factory-worklists/epsilon-extension.json` from the checkout. Walk away; ext0 is tonight's overnight and tomorrow's working day.

## Aug 15 (ext0 boundary)

- **A5 — ext0 interim close (composite, by checklist — the last hand-performed close).** Per the doctrine order: quiescent → `release --plan` → probe → `release` → nothing after. Plus the ceremonial upward half, by hand for the last time: append ext0's receipt/merged-sha facts upward into `specs/epsilon-extension/trace.json` (one commit). Whether this close disarms the identity or leaves it registered for ext1 follows the interim-close doctrine ext0's own `authoring-doctrine-skills` task just committed — read it as merged, not as remembered.
- **A6 — boundary deploy.** Deploy the fleet to the ext0-integrated pin (the frozen-flow rule: from here the post-ceremony machinery grades).
- **A7 — Z1, the zeta boundary sitting (~45 min, keyboard, one commit).** In order: run the falsity pass on `specs/zeta/spec.md` against the observed post-ext0 tree ("which of these statements about my codebase are false?"); regenerate any falsified section (never line-edit); flip Status: proposed → ratified; finalize `silent-factory-worklists/zeta.json` from ZETA.md's task specs against the observed tree — settle DECISION-1 (steward field value) and regenerate anchor strings if INTERFACE-1 changed the grammar; verify every readFirst path exists at the base tip; rehearse admission (every gate preflight argv verbatim, in a pristine worktree, including a local un-pushed HEAD); commit spec ratification + worklist together, push.
- **A8 — Arm zeta.** `tally campaign arm mecattaf/tally.nix silent-factory-worklists/zeta.json`. Overnight #2 (~7–10h).

## Aug 16

- **A9 — zeta close (composite).** Quiescent → render the coverage table with the campaign's own deliverable: `spec-lint --coverage specs/zeta` (first tool-rendered close-out — the hand-rendered table is dead) → hand the table plus `specs/zeta/spec.md` destination section to release as the operator intent → `release --plan` → probe → `release` → `disarm` last, nothing after. The ext1 sitting that follows (outside zeta's scope) is the first stage ever derived per `skills/author-spec` with real anchors — zeta's actual finish line.

**Count: 9 planned acts** (A5 and A9 composite checklists), two campaigns, against the destination's ~16-act bar for one campaign of epsilon's size.

## FIELD: risks

# Risks, deferrals, unsettled decisions

## Unsettled DECISION-n

- **DECISION-1 — the `steward` field value post-ext0.** `judge-verdict` rebinds the diagnosis slot to a steward catalog role; whether the worklist's `steward` key keeps `"narrator"` as a legacy value, takes a new role name, or becomes ignorable is a fact of the merged ext0 tree I cannot observe tonight. Settled at Z1 against the observed tree; zeta.json drafts `"narrator"` as placeholder. GUESS until then.
- **DECISION-2 — anchor grammar final form** (`#r2` per 00-INDEX paradigm 3 vs `#requirement-2` per lens-seams). Other designer's ruling (INTERFACE-1); my pointer strings and the `spec-layer-skills-amend` goal text regenerate mechanically at Z1 if it lands the long form.
- **DECISION-3 — where the lint rule list lives.** I bind readFirst to `specs/README.md` on the v2 doctrine "README keeps only what the linter enforces" (INTERFACE-2). If the other designer splits rules into a separate artifact, `spec-lint-core`'s readFirst gains that path at Z1.
- **DECISION-4 — day-one lint scope.** I ruled the flake check lints **all** of `specs/` including `specs/epsilon-extension/` (honest, but the corpus was authored pre-lint; first-contact defects escalate as spec defects for operator regeneration + re-ratification, per the record-don't-fix clause in `spec-lint-flake-check`'s goal). The timid alternative — scope to `specs/zeta/` first — is available if the lead prefers a guaranteed-green first night. Needs the lead's word.
- **DECISION-5 — specs/zeta ratification timing.** Ruled: proposed tonight, ratified at Z1 (its BELIEVE claims about the substrate must survive the falsity pass against the *post-ext0* observed tree, which does not exist tonight). Flagged because it makes zeta the only campaign whose spec ratifies at the same sitting that arms it — which is exactly the post-ext1 collapsed shape, rehearsed early.
- **DECISION-6 — freeze-pointer on EPSILON-EXTENSION.md at A2.** Ruled: add the header pointer to the recast instance now, content untouched (the Rulings self-containment law: predecessor surface freezes with a pointer). Flagged because it mutates the ratified program's file mid-run — additive and inert, but the lead should bless it.

## What breaks the overnight(s)

- **ext0 slips → zeta slips a night.** Zeta depends on ext0 *quiescence* plus a deployed pin, not ext0 success. If ext0 latches escalated, zeta may still arm once every lane owning `flake.nix`/`skills/`/workspace domains is dead — but not before; cross-campaign collisions are invisible to ownership certification. If ext0 is mid-recovery at Z1 time, wait.
- **First-contact lint defects in the committed corpus (DECISION-4's risk).** The authored instances were written before the tool existed. Mitigation: the other designer applies the rule list by hand during tonight's authoring; residual defects surface as verbatim stderr lines in `spec-lint-flake-check`'s task report and cost one operator regeneration commit — they must NOT cost lane edits to spec bytes.
- **Shakedown risk: zeta is the first campaign graded by the post-ext0 machinery.** Epoch budgets, outcome envelopes, judge verdicts, subject adoption all run their first live campaign on zeta's lanes. A machinery fault (not zeta-code fault) can burn attempts; the stale-pin attribution rule applies before blaming any zeta commit. This ordering is deliberate — better to shake down under one crate of cargo than under ext1's self-modifying last mile — but it is a real overnight risk.
- **Cargo.lock/cargo-deny bite.** A lane adding any external dependency to `spec-lint` trips the fleet ladder's cargo-deny stage at the chapter gate, hours after the mistake. The goal forbids new deps and the `no-new-deps` acceptance argv catches it at merge time instead.
- **Chapter-gate duration.** fleet-gate now carries the executing final bar plus a heavier `nix flake check` (spec-lint added). The 10800s template cap should hold; if the bar's executed subset grew in ext0 beyond expectation, the gate times out and the Z1 sitting must re-check the cap against the merged `final-bar-executes` reality.
- **Detached HEAD.** The checkout sits detached at e921cccc; every act A1–A2 must land on the branch the remote base expects or admission will read a base tip without them. A1 begins with the branch switch for exactly this reason.

## Deliberately deferred (with owner)

- `specs/<armed-identity>/**` on the R3 deny-list; admission resolving `specs/**` pointers; escalation reports resolving citations — **ext1 sitting** (each priced against D58 in lens-seams §deepenings, now authorable with real anchors).
- Release rendering coverage from durable completion facts; `completionProofs` join — **ext2** (rides E1).
- `tally spec diff` / `questions` as CLI verbs, the inbox-borne typed-doubt queue — **post-zeta**, each admitted only when it deletes an operator rule; tonight census+coverage ship as lint modes, not verbs.
- Golden-fixture double-pin for `trace.schema.json` (Nix render vs Rust read) — one overnight of over-engineering for a schema with one consumer; revisit when a second consumer exists.
- Dual blind derivation (the grind) for spec-lint itself — the must-fail corpus is the bar for a linter; a full grind waits for the first contract of consequence (agency's dialect bridge).
- Recasting the six historical worklists (ch0–ch5, epsilon.json) or amending epsilon-extension.json's prose locators to anchors — history is history; ext1's stage is the first anchor-native worklist.

## Interface points with the other designer (consumed by path/name)

INTERFACE-1 anchor grammar + requirement numbering (`specs/zeta/spec.md#r1…#r6` area map is my contract to them); INTERFACE-2 the rule list as `specs/README.md` v2; INTERFACE-3 `specs/zeta/contracts/trace.schema.json`; INTERFACE-4 `skills/author-spec/SKILL.md` content (landed A2, truth-up by task 4); INTERFACE-5 `specs/epsilon-extension/` recast + trace rows + the five evidence ledgers ready to copy at A2; INTERFACE-6 the attribute name `checks.x86_64-linux.spec-lint` (wired by task 3, cited by their README/skill — must match); INTERFACE-7 must-fail corpus at `crates/spec-lint/fixtures/must-fail/` (cited by their anti-rot section); INTERFACE-8 ZETA.md carries these task specs verbatim as Z1's authoring source.

