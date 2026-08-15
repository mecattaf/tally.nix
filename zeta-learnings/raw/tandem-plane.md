## FIELD: file_manifest

# ZETA FILE MANIFEST — authority-plane half

Every path zeta creates or rewrites, with purpose and standing consumer (A15: every artifact names its standing consumer or dies). Paths are repo-relative under `/home/tom/mecattaf/tally.nix`.

## New directories and files

| # | path | purpose | standing consumer |
|---|------|---------|-------------------|
| 1 | `specs/README.md` (rewrite, v2) | The format, slimmed to what the linter enforces; every sentence annotated with the lint rule id that enforces it | A `crates/spec-lint` unit test parses README's rule-index table and asserts it matches the implemented rule set (drift fails `cargo-tests`); plus `skills/author-spec` §Author reads it at every authoring loop |
| 2 | `specs/constitution.md` (rewrite, v2) | The laws, with the five accepted critiques applied (see docs_v2) | `skills/author-spec` sitting checklist cites article ids; the judge's amendment proposals cite article ids; ratification review reads it |
| 3 | `specs/zeta-authority-plane/spec.md` | The first house-grammar spec instance — governs zeta's own deliverables (see first_spec) | `checks.x86_64-linux.spec-lint` (standing, every fleet-gated head via test/fleet-gate.sh:254); the zeta worklist's `readFirst` anchors; the close sitting's coverage render |
| 4 | `specs/zeta-authority-plane/trace.json` | Append-only three-way join claim↔task↔receipt/commit | `spec-lint` rules L14/L17; `spec-lint --coverage` at the close sitting |
| 5 | `specs/zeta-authority-plane/contracts/trace.schema.json` | Byte contract for trace rows, in the flake's golden-fixture double-pin shape | `spec-lint` validates trace.json against it at runtime (jsonschema crate, already a workspace dep — Cargo.toml `[workspace.dependencies]`); a crate test double-pins schema vs serde model |
| 6 | `specs/zeta-authority-plane/contracts/claim-line.fixtures.json` | Accept/reject corpus of claim lines — the double pin between README's grammar prose and the parser | `crates/spec-lint` parser tests (run under the `cargo-tests` gate) |
| 7 | `specs/zeta-authority-plane/evidence/` | Census reports per sitting (`census-s1.md`, ...) and authoring evidence the spec cites | Goal/trace citations resolved by L13; the sitting writes into it |
| 8 | `specs/epsilon-extension/evidence/{process-archaeology,verified-defects,ceremony-audit,equipment,design-pass}.md` | The five Aug-14 excavation ledgers (PA/VD/CA/EQ + design pass), committed out of the session scratchpad — E6 territory, operator pre-step, NOT a lane task | The LIVE `silent-factory-worklists/epsilon-extension.json` goal citations (CA-3, F37, PA-25, VD-5...) become resolvable at the authority revision (D68 applied to specs). Deliberately NO `spec.md` here: `EPSILON-EXTENSION.md` v2 stays the one ratified authority mid-flight (one authority per fact); the lint skips spec-less identity dirs |
| 9 | `crates/spec-lint/` (`Cargo.toml`, `src/main.rs`, `src/parser.rs`, `src/rules.rs`, `src/trace.rs`, `src/worklist.rs`, `src/report.rs`) | The enforcement engine — one Rust parser, three surfacings (flake check, campaign gate, sitting rehearsal) | `checks.x86_64-linux.spec-lint`; the `spec-lint` campaign gate; `skills/author-spec` §Sit |
| 10 | `crates/spec-lint/tests/lint_test.rs`, `tests/fixtures/golden/**`, `tests/fixtures/must-fail/**` (incl. `expected-defects.json`) | Golden minimal passing spec + the must-fail perturbation fixture proving the lint bites | `cargo-tests` gate; the flake check attribute re-runs must-fail inside its own derivation (bite proven in the standing consumer itself — the anti-`--list`-only law, VD-5/F33) |
| 11 | `Cargo.toml` (workspace, edit) | Add `"crates/spec-lint"` to `members` and `default-members` (Cargo.toml:3–16) | `cargo-tests` and `clippy` gates build it on every merge |
| 12 | `flake.nix` (edit) | Add `spec-lint = ...` to the `checks` set (the block at flake.nix:3455–3488, beside `module-layer`:3464 and `campaign-runtime`:3488) | `test/fleet-gate.sh:254` (`nix flake check -L --keep-going`) — fleet-tier standing coverage with zero fleet-gate edits |
| 13 | `skills/author-spec/SKILL.md` (new file — collision-free with ext0) | The authoring loop, falsity pass, typed questions, sitting steps (see author_skill) | Every sitting; the operator invoking the skill; deletes the procedure prose from constitution v1 (A16/A17 bodies) and README v1 §Verification |
| 14 | `skills/assign-tally/SKILL.md` (edit — spec-layer sentences: goals cite claim ids, readFirst points at `#rN` anchors, sitting appends trace rows) | Worklist authoring at every sitting. **SEQUENCING CONSTRAINT (theirs to wire): ext0's `authoring-doctrine-skills` task owns this file (epsilon-extension.json:316,343); zeta's amendment must depend on / follow it, never collide** | |
| 15 | `skills/campaign-operator/SKILL.md` (edit — interim close renders coverage via `spec-lint --coverage`; same ext0 constraint) | The close sitting | |
| 16 | `doc/src/flows/campaigns.md` (edit, ~line 1341) | Re-render the doc example anchor `specs/001-crm/spec.md#customer-model` to the number-derived grammar (`#r1`) so shipped docs and linter agree (seam S13) | `checks.doc` (flake.nix:3461) |

NOT in this half (the other designer's): `silent-factory-worklists/zeta-authority-plane.json`, task goals/budgets, sequencing against ext0/ext1, the operator-act list, gate ordering within the worklist.

## INTERFACES (what the sequencing designer must wire)

1. **Flake check attribute name**: `checks.x86_64-linux.spec-lint`. Lands via manifest items 9–12; no gate may reference it before the flake-edit task merges (ordering dependency: crate task → flake-attr task → first gate use).
2. **Campaign gate** (verbatim, for the zeta worklist gate set; place it cheap-first, before `cargo-tests`):
   `{"kind":"command","id":"spec-lint","preflightArgv":["sh","-euc","command -v nix >/dev/null"],"argv":["nix","build","--no-link",".#checks.x86_64-linux.spec-lint"],"runtimeMaxSec":900}` — runtimeMaxSec 900 is GUESS (unmeasured first build; cached after).
3. **Gate ids the spec's bindings require verbatim in the worklist**: `cargo-tests` (argv as epsilon-extension.json:20) and `flake-build-subset` whose built list includes `.#checks.x86_64-linux.spec-lint`, `.#checks.x86_64-linux.module-layer`, `.#checks.x86_64-linux.campaign-runtime`. If these ids change, the spec's `[gate: ...]` bindings fail L9 — tell me and I re-bind.
4. **Campaign identity**: `zeta-authority-plane`; worklist filename must match (`silent-factory-worklists/zeta-authority-plane.json`). DECISION-1: final say on the name is yours; the spec dir renames with it.
5. **readFirst-citable paths** (must exist at the authority revision — E6 + the spec commit are prerequisites): `specs/zeta-authority-plane/spec.md#r1`…`#r4`, `#unchanged`, `#forbidden`; `specs/zeta-authority-plane/contracts/trace.schema.json`; `zeta-learnings/raw/instinct-fable.md`, `zeta-learnings/raw/instinct-opus.md`. Never put `specs/README.md` or `specs/constitution.md` in readFirst (Seam-1 exclusion) — the tasks that rewrite them carry the outline verbatim in `goal` and name them in conflictDomains only.
6. **conflictDomains law**: `specs/zeta-authority-plane/**` appears in NO task's conflictDomains and no lane writes it (pre-R3-extension discipline, works-today §6). Lanes DO own: `crates/spec-lint`, `Cargo.toml`, `flake.nix`, `specs/README.md`, `specs/constitution.md`, `skills/author-spec`, `doc/src/flows`.
7. **Operator pre-step dependency**: E6 (commit zeta-learnings/, EPSILON-EXTENSION.md, the ledgers under specs/epsilon-extension/evidence/, this design) must precede arming or every spec pointer is phantom.
8. **Sitting rehearsal command** (for your operator-act list): `nix develop --command cargo run -p spec-lint -- specs/zeta-authority-plane --worklist silent-factory-worklists/zeta-authority-plane.json --repo-root . --mode sitting`.
9. **Must-fail fixture path** (cite it in the lint task's acceptance): `crates/spec-lint/tests/fixtures/must-fail/` with `expected-defects.json` — acceptance must assert exact defect-code match, not mere nonzero exit.
10. **Trace rows**: your sitting step list must include appending kind:`sitting` rows (fields in trace_schema) in the same commit as the worklist stage.

## FIELD: format_grammar

# THE HOUSE spec.md GRAMMAR (v2, lintable)

File: `specs/<identity>/spec.md`. Line-oriented by design — the linter parses a line grammar, never a markdown AST. The format was elicited from the authoring models, not imposed (zeta-learnings/09; raws cited per rule below).

## Status block (preamble, before the first `##`)

```
# <identity> — <title>
Status: proposed | ratified YYYY-MM-DD | closed <release-ref>
Governs: silent-factory-worklists/<identity>.json
Consumers: <at least one — gate, check attribute, skill, or sitting>
Supersedes: <path> | none
```

`Consumers` non-empty is law (A15: a bar without a gate is not a bar). `Governs` must name an existing file once Status is ratified.

## Section set, exact order (lint L1)

1. `## Outcome` — anchor `#outcome`. 3–8 sentences, observable before/after difference. Never omittable. *(instinct-opus §1: "if I can't write it in eight sentences I don't understand the task yet — that failure is worth surfacing immediately"; instinct-fable §1: "After this change, X can Y. Today it cannot because Z.")*
2. `## Vocabulary` — `#vocabulary`. Lines `- <term> — <definition>`, optional ` (NEW)` flag on identifiers this campaign creates. One noun per concept, declared once, used identically forever. *(both §1; fable: "the highest-leverage cheap thing I do"; opus: "most implementation drift I've seen is two words that were nearly synonyms". NEW comes from opus §6: invented identifiers "presumed invented until marked NEW: — and NEW count should be small".)*
3. `## Rulings` — `#rulings`. Table `| id | decision | ruling |`; ids match `[A-Z][0-9]+` and must NOT match `R[0-9]+` (that namespace belongs to claim groups; zeta uses `Z1…`). Every ambiguity resolved while authoring gets a row — silent respecification's mandatory home. *(instinct-fable §2: "The spec reads as if the decision came from the operator. The format must give resolved ambiguities a mandatory home"; §6: an empty decisions section "almost never truly is" — hence the L15 warning.)*
4. `## Claims` — `#claims`. Groups `### R<n> — <name>` with the anchor derived from the number only: `### R2 — the trace` → `#r2`. Retitle-safe by construction (lens-seams Seam 1: "anchor stability is a format obligation, not a hope"). Group body: one plain-prose *why* line, then claim lines.
5. `## Unchanged` — `#unchanged`. The SHALL-CONTINUE-TO discipline without the EARS costume: flat arrow lines `U.<m> <condition> → <observable that continues to hold> [binding]`, bound to already-passing oracles. *(zeta-learnings/09: "the instinct keeps EARS's discipline — stable IDs, one testable claim, guard-then-consequence — and drops its costume"; Kiro's contribution kept, its ceremony not.)*
6. `## Unknowns` — `#unknowns`. Two typed line forms only:
   - `UNKNOWN-<n> [BLOCKING]? <what could not be determined> — <action>` *(instinct-opus §1: "things I could not determine and refused to invent, each with what to do about it… This is the section frameworks omit and I need most.")*
   - `DECISION-<n> <question>? proposed: <answer> (GUESS|given)` *(instinct-fable §5: "human decisions arrive as answers to typed questions I emit — `DECISION-1: retention period? [proposed: 30d, invented]` — never as edits to the artifact.")*
7. `## Stages` — `#stages`. `### S<n> — <name>` → `#s<n>`. Build order only, no calendar (A13). Unauthored stages list ruling ids and claim-group refs, nothing more (F42).
8. `## Forbidden` — `#forbidden`. **Always the last section.** Lines `F.<m> Do not <...>` or `F.<m> Never <...>` — verb-first negation, one prohibition per line, own section. *(instinct-opus §1: "Negative constraints buried mid-paragraph get dropped by every implementer"; §4: "Recency wins. The last instruction in the document dominates. So the FORBIDDEN and DONE sections go last"; "Negations get dropped when embedded in a positive sentence… I write it as its own line, starting with the verb.")* The `F.` dot distinguishes spec-forbidden ids from evidence-ledger finding ids (`F38`).

## Omission rule

A section other than Outcome and Claims may be omitted only by keeping its heading and making its entire body the single line `Omitted: <one-line reason>.` — never by deleting the heading. *(instinct-opus §3: "If the format has 12 sections and my task needs 5, you get 7 sections of fabrication. Let sections be omitted, explicitly, with a one-line reason"; instinct-fable §3: "mandatory templates with sections that don't apply — I will fill them.")*

## Claim line grammar

```
<g>.<m> [BELIEVE:<path> — ] <condition> → <observable> [check: <attr> | gate: <id> | HUMAN-ATTENDED]
```

- `<g>` equals the enclosing `### R<g>` group number; `<m>` ascending within the group; ids globally unique (L3).
- Arrow `→` mandatory; exactly one claim per line; no ` and ` joining two verbs in the observable *(instinct-opus §4: "They stop at the first satisfied clause. A sentence with two requirements joined by 'and' gets half-implemented. One claim per line, always"; sentence shapes that survive: "`<condition> → <observable outcome>.` Return X. Do not Y.")*
- Exactly one oracle binding per claim/unchanged line; zero or two is lint defect L9 (paradigm 10, byte-oracle-or-nothing).

## Provenance marks — exact syntax and valve semantics

| mark | syntax | which side is authoritative | on conflict |
|---|---|---|---|
| DECIDE | *unmarked* (the default state of a claim line) | the spec | oracle fails → the code is wrong; the gate ladder already mechanizes this direction |
| BELIEVE | `BELIEVE:<path> — ` immediately after the claim id | the tree | tree disagrees → the spec is wrong; mechanized by L12 (path must exist; backticked identifiers on the line must appear in the named file's bytes — rename the symbol and the gated head fails: drift is a build failure the day it starts) |
| GUESS | `(GUESS)` suffix on a numeral; or the `(GUESS)` basis on a DECISION line | nobody — it blocks | outstanding GUESS at Status: ratified is a blocking lint defect (L10); resolved only by a typed operator answer, then rewritten to `(given)` |
| HUMAN-ATTENDED | `[HUMAN-ATTENDED]` as the claim's binding | the named human moment | legal, honest, enumerated by the census — an oracle gap declared, never discovered |

*(instinct-opus §7: "DECIDE: — this is my decision, obey it, do not verify — versus BELIEVE: — this is my model of your system, verify before relying, and here is what to read"; instinct-fable §7: "let the spec contain first-class uncertainty markers that the pipeline treats as blocking… my worst failure mode becomes a queue of small explicit questions instead of a fluent lie in paragraph four.")* Unmarked-as-DECIDE follows synthesis doc 10 ("DECIDE (default, unmarked)"): certainty is the ground state; only doubt needs syntax.

## Numeral provenance

Every numeral in Claims/Unchanged/Forbidden that is not a cross-reference (claim id, ruling id, stage id) must be one of: on a `BELIEVE:<path>` line (sourced by the path), suffixed `(given)` (operator supplied), or suffixed `(GUESS)`. *(instinct-opus §6: "Every number must be sourced: given by operator, present in a quoted file, or marked GUESS"; instinct-fable §6: unsourced-literal check "alone catches my most frequent fabrication".)*

## Lexicon bans (Claims/Unchanged/Forbidden; Outcome/Rulings warn)

Hedges: should, ideally, typically, appropriately, robust, gracefully, as needed, if necessary, reasonable, properly. `e.g.` and `etc.` banned document-wide. *(both §6; opus: "each is a hole where a decision should be… both are how I smuggle in unclosed sets.")*

## Length

No lint maximum (no speculative rules — lens-seams S10). Authoring guidance in skills/author-spec targets opus §1's 400–1200 words / fable §1's degradation past ~1500 lines; the goal-shrink metric (S10) is how we know the layer works, unlinted.

## FIELD: lint_design

# crates/spec-lint — LINT DESIGN

One Rust crate, one binary, three surfacings of one implementation (synthesis doc 10 §verbs). Invocation: `spec-lint <specs/IDENTITY-dir> [--worklist PATH] [--repo-root PATH] [--mode check|sitting] [--coverage [--append]]`. Exit 0 clean, 1 warnings-only, 2 blocking defects. Every rule prints `L<nn> <path>:<line> <message>`.

## Rules (each: catches / operator rule it DELETES (D58) / severity)

- **L1 — section set, order, omission grammar.** Wrong/missing/misordered sections; malformed `Omitted:` line. Deletes: the structural half of README v1's manual analyze pass ("wrong level of detail", format review by eye at every sitting). Blocking.
- **L2 — status-block grammar + standing consumer named.** Unparseable Status line; empty `Consumers:`; ratified spec whose `Governs:` file does not exist. Deletes: the anti-rot audit glance ("does anything still read this") — the rule the grind's bar rotted 5 days for want of (VD-5, F33). Blocking.
- **L3 — anchor and id grammar.** `### R<n> — name` headings unique/ascending; claim ids `<g>.<m>` matching their group, unique, ascending; `U.<m>`/`F.<m>`/`S<n>` namespaces; ruling ids not colliding with `R\d+`. Deletes: hand-verifying citation targets after any retitle. Blocking.
- **L4 — claim-line shape.** Missing arrow; two claims on one line (` and ` joining verbs in the observable); Forbidden lines not verb-first negations. Deletes: the compound-criteria review (opus §6 "Any B- line containing ' and ' joining two verbs. Split it."). Blocking.
- **L5 — hedge lexicon** in Claims/Unchanged/Forbidden (fixed word list from both §6 testimonies). Deletes: the "vague qualifiers" sweep of the manual analyze pass. Blocking (warning in Outcome/Rulings).
- **L6 — `e.g.` / `etc.` ban**, document-wide. Deletes: enumeration-scope review ("did the author leave a set open"). Blocking.
- **L7 — unsourced numerals.** A numeral in Claims/Unchanged/Forbidden that is not a cross-reference and carries no `(given)`/`(GUESS)` and sits on no BELIEVE line. Deletes: the operator's read-every-number confirmation sweep (fable §5 moment 2). Blocking.
- **L8 — out-of-context identifiers.** Every backticked token and path-shaped token set-differenced against (a) the tree at the lint revision, (b) Vocabulary terms, (c) Vocabulary `(NEW)` declarations; leftovers are defects. Deletes: the mechanical half of the falsity pass (path/symbol existence), leaving the human pass pure semantics. Blocking. *(The two rules that catch most real damage are L7+L8 — opus §6 verbatim.)*
- **L9 — oracle-binding census.** Each claim/unchanged line has exactly one of `[check: <attr>]` / `[gate: <id>]` / `[HUMAN-ATTENDED]`; zero or two = defect; `[check:]` must resolve (token `<attr> =` present in the flake.nix checks region — the grep-presence genre campaign-runtime already uses, flake.nix:3515–3529); `[gate:]` must resolve to a gate id in the governing worklist's `campaign.gates`. Deletes: the "is this requirement tested anywhere" coverage judgment — coverage becomes an enumeration (paradigm 10). Blocking.
- **L10 — doubt gate.** Outstanding `(GUESS)`, `DECISION-n`, or `UNKNOWN-n [BLOCKING]`: blocking when `Status: ratified` (any mode) and always in `--mode sitting`; warning while `Status: proposed`. Rationale, stated not smoothed: the typed-doubt queue must be committable bytes (a proposed spec with open questions IS the queue), but derivation while doubt is outstanding is impossible and ratification with doubt outstanding fails the next gated head — enforcement in the harness, not the prompt. Deletes: the pre-derivation hedge re-read; doubt arrives as a typed inbox queue (paradigm 4).
- **L11 — vocabulary declared once.** Term defined twice = blocking; term defined and never used again = warning (fable §6: an identifier appearing exactly once "is usually a rename I forgot to propagate"). Deletes: the vocabulary-drift review.
- **L12 — BELIEVE resolution (the upward valve).** `BELIEVE:<path>` path must exist at the revision; every backticked identifier on that line must appear in the named file's bytes. The tree moving under a BELIEVE claim fails the next gated head — code falsifies spec through the same ladder. Deletes: the manual "is the spec still true of the tree" re-read. Blocking.
- **L13 — dangling pointers.** Every `specs/**` string in the governing worklist's `readFirst`/`goal` resolves to file + anchor (anchors derived by grammar, so resolution is exact); every evidence id cited in spec or trace resolves into a committed `evidence/` file containing that id. Deletes: the doctrine sentence at skills/assign-tally/SKILL.md:25 checked by eye — the 48-phantom-pointer class closed mechanically. Blocking.
- **L14 — trace resolution.** Every trace row's claim id exists in spec.md; task id exists in the governing worklist; acceptance ids ⊆ that task's acceptanceCriteria ids; a release row must follow a sitting row for the same (claim, task); seq strictly monotonic; the file validates against `contracts/trace.schema.json`. Deletes: the hand-maintained trace table and the hand-verified close-out table. Blocking.
- **L15 — mandatory-section-missing-without-reason.** Heading absent entirely, or omission line malformed = blocking; a Rulings section that is empty or omitted = warning (fable §6: "an empty section means I resolved ambiguities silently"). Deletes: template-completeness review; prevents fabricated filler by construction.
- **L16 — model names in authority bytes.** Fixed lexicon (sonnet, opus, haiku, fable, codex, gpt, claude-) in spec.md or the governing worklist = blocking. Deletes: the operator's review for host-catalog leakage (judge-verdict goal: "which model answers is a host-catalog fact, never worklist bytes"). Blocking.
- **L17 — trace append-only** (`--mode sitting` only, where git exists): `git show HEAD^:specs/<id>/trace.json` parsed; old rows must be a structural prefix of new rows (byte-stable, only appends). Honest limitation, stated: the flake-check sandbox receives a store path without `.git`, so history-dependent enforcement lives in sitting mode (and, ext-era, a fleet-gate step — deny-listed file, so that edit rides a boundary deploy, the other designer's sequencing). Blocking in sitting mode.

Self-test is not a numbered rule but the harness law: the crate ships the must-fail fixture from day one, and the flake check re-runs it (below).

## Parser strategy

One crate, no external markdown parser: the grammar is line-oriented by design, so `parser.rs` is a small state machine over lines (section tracking, regex per line class); `serde_json` for trace.json and the worklist; the `jsonschema` workspace dep (Cargo.toml `[workspace.dependencies]`, v0.33) validates trace against `contracts/trace.schema.json` — the double pin. flake.nix `[check:]` resolution is token-presence in the checks region (grep genre, not nix eval — the sandbox cannot evaluate). Tree existence checks walk `--repo-root` (the store-path source in the check; the worktree at sittings).

## Wiring

- **Flake check attribute**: `checks.x86_64-linux.spec-lint`, added in the checks set at flake.nix:3455–3488. The derivation: build the crate, run `spec-lint --mode check` over every `specs/<identity>/` containing a `spec.md` (with `--worklist silent-factory-worklists/<identity>.json` when that file exists; spec-less dirs like `specs/epsilon-extension/` skipped), THEN run it over `crates/spec-lint/tests/fixtures/must-fail` and assert exit 2 with defect codes exactly matching `expected-defects.json` — the attribute both executes and proves non-vacuity in one run, or dies. Standing coverage is free: test/fleet-gate.sh:254 already runs `nix flake check -L --keep-going`.
- **Gate argv** (lane/campaign tier, cheap-first placement): `["nix","build","--no-link",".#checks.x86_64-linux.spec-lint"]`, preflight `["sh","-euc","command -v nix >/dev/null"]`, id `spec-lint`, runtimeMaxSec 900 (GUESS — unmeasured cold build). Same genre as the live `flake-build-subset` gate (epsilon-extension.json:30–36).
- **Sitting rehearsal**: `nix develop --command cargo run -p spec-lint -- specs/<id> --worklist silent-factory-worklists/<id>.json --repo-root . --mode sitting` — adds L17 and hard L10.

## Must-fail fixture contents

`crates/spec-lint/tests/fixtures/must-fail/` = `spec.md` + `trace.json` + `worklist.json` + `expected-defects.json`. The spec.md is compact and seeds ONE instance of each blocking class: misordered section (L1); missing `Consumers:` (L2); claim `2.1` under `### R1` (L3); a claim with two bindings and a claim with none (L9); "gracefully" in a claim (L5); "e.g." (L6); bare `30` with no provenance (L7); backticked `retry_with_jitter` absent from the tree (L8); `BELIEVE:src/does-not-exist.rs` (L12); `Status: ratified` with `DECISION-1 … proposed: 30d (GUESS)` open (L10); a Vocabulary term defined twice (L11); the word `sonnet` in a claim (L16). `trace.json` seeds: a row citing claim `9.9` (L14) and a release row with no sitting row (L14). `worklist.json` seeds a readFirst pointer to `spec.md#r9` (L13). `expected-defects.json` is the exact `{rule: count}` map; crate test and flake check both assert exact equality — a lint failing for the wrong reason is not proven to bite.

## Authoring loop vs witnessing path

- **Authoring loop** (author → lint → fix): the model runs the sitting-mode command locally after each draft; "don't hallucinate" instructions cannot work, mechanical catching can (opus §3; paradigm 5). Warnings are the author's queue; blocking defects stop the loop.
- **Witnessing path**: every gated head runs the flake check (fleet tier); every campaign merge runs the `spec-lint` gate; the moment the tree moves under a BELIEVE claim or a pointer dangles, the head fails. The lint is the spec layer's standing consumer; the must-fail fixture is the lint's own.

## FIELD: trace_schema

# specs/<identity>/trace.json — APPEND-ONLY SCHEMA

The three-way join claim ↔ task ↔ receipt/commit: written downward at sittings, completed upward at close/release (synthesis doc 10 §nouns). It resolves the v1 freeze contradiction (lens-seams Seam 6): spec.md freezes at ratification; trace rows are authored per stage, after ratification — so they live beside it, append-only.

## Shape

```json
{
  "schemaVersion": 1,
  "spec": "specs/zeta-authority-plane/spec.md",
  "rows": [
    {
      "seq": 1,
      "at": "2026-08-14T21:00:00Z",
      "kind": "sitting",
      "sitting": "zeta-authority-plane/s1",
      "claim": "1.2",
      "task": "spec-lint-crate",
      "acceptance": ["must-fail-bites"],
      "evidence": ["EQ-2.4"]
    },
    {
      "seq": 14,
      "at": "2026-08-15T07:40:00Z",
      "kind": "release",
      "claim": "1.2",
      "task": "spec-lint-crate",
      "merged": "<40-hex sha of the squash-merge commit>",
      "witness": "refs/tally/spec-build/v1/<digest>/summary/complete",
      "release": "<release identity/issue ref>"
    }
  ]
}
```

## Exact field set

Common (every row): `seq` (int, strictly increasing by 1 across the whole file), `at` (RFC3339), `kind` (`"sitting"` | `"release"`), `claim` (a claim or unchanged id existing in spec.md: `"1.2"`, `"U.1"`), `task` (task id existing in the governing worklist).

`kind: "sitting"` adds: `sitting` (`<identity>/s<n>`, the stage sat), `acceptance` (array of acceptance-criterion ids WITHIN that task — task-local ids, deliberately not renamed to claim ids; the namespaces stay separate and this row IS the join, lens-seams S11), `evidence` (optional array of ledger ids resolvable under an `evidence/` dir, L13).

`kind: "release"` adds: `merged` (40-hex sha), `witness` (the durable completion fact consulted — summary ref or receipt pointer; DECISION-2: exact witness target, summary ref vs release-record id, to be fixed when the other designer's close wiring is known; `proposed: the summary/complete ref, since release_closing_summary already resolves it — crates/tally/src/cli/campaign.rs:2306 per lens-seams Seam 5`), `release` (the release identity).

The schema is committed as `specs/<identity>/contracts/trace.schema.json` and double-pinned: spec-lint validates at runtime, a crate test validates the golden fixture against both the schema and the serde model.

## Append-only enforcement rule

Old rows are a **structural prefix** of new rows: parse parent revision, parse head, `old.rows == new.rows[..old.len]`, byte-stable per row, `seq` gapless. Enforced by L17 in `--mode sitting` (git available); the flake-check sandbox cannot see history (store path, no .git), so the standing check enforces schema + resolution (L14) and the history rule bites at every sitting and, ext-era, as a fleet-gate step (deny-listed file — that edit rides a boundary deploy). Considered and rejected: renaming to JSONL for byte-prefix cuteness — keeps the ratified artifact name `trace.json` and ordinary JSON tooling instead.

## Who writes each row (the decision/rendering bright line)

- **sitting rows** — the authoring model's hand at the sitting, committed by the operator in the one sitting commit. This is a human-attended DECISION (which claim a task discharges is the sitting's authorship judgment, frozen at derivation time — release later *renders* this mapping, never re-computes it).
- **release rows** — machine RENDERING, human-committed: at zeta, `spec-lint --coverage --append` at the close sitting joins sitting rows with durable completion facts (merged shas, summary refs) and appends the rows; the operator reviews and commits. No transcription act (A19: the operator never retypes what the machine printed) and no machine decision takes spec bytes — completion is decided from receipts alone; trace only shapes the rendering. Ext2 moves the same append inside the release verb (legal under the freeze/append article's explicit enumeration; the machinery still never touches spec.md or the worklist).
- **Nothing else ever writes it.** Lanes never touch `specs/<governing-identity>/**` (conflictDomains exclusion now; deny-list entry ext-era).

## FIELD: docs_v2

# DOCS V2 — specs/README.md and specs/constitution.md

Both rewrites apply the accepted critiques from zeta-learnings/00-INDEX §repo-state notes, verbatim: (1) traceability moves out of frozen spec.md into append-only trace.json; (2) A2's read-half becomes the decision/rendering line (no machine *decision* takes spec bytes; machine *rendering* may resolve citations); (3) the deny-list entry is scoped to the governing spec only; (4) the missing freeze/append article is added (post-ratification legal changes: status transitions, trace appends, evidence additions — nothing else); (5) procedures (grind, analyze checklists) move from constitution to skills/author-spec; README keeps only what the linter enforces.

## specs/README.md v2 — outline

Discipline: every sentence is either a lint rule or a pointer to one (lens-seams Seam 8); each grammar statement carries its `[L#]` annotation. Standing consumer: the spec-lint crate test that parses §7's rule-index table and asserts parity with the implemented rule set.

1. **Position** (short). Spec above worklist; the worklist stays the only machine-admitted authority, schema closed; the spec points at tasks, never the reverse; lineage spec → worklist → receipts → release. Pointers to constitution A2/A22.
2. **The artifact set.** `spec.md` (required), `trace.json` [L14/L17], `contracts/` (double-pin law; schemas + fixtures), `evidence/` (what makes citations resolvable [L13]). Each entry names its standing consumer [L2]. Identity = directory name = worklist filename; no numeric prefixes. A spec-less identity dir (evidence-only, e.g. `specs/epsilon-extension/`) is legal and skipped by the lint.
3. **spec.md grammar.** The full grammar from format_grammar: status block [L2], section set and order [L1], omission rule [L15], heading/anchor derivation `### R2 → #r2` [L3], claim-line form [L4], numeral provenance [L7], identifier context [L8], lexicon bans [L5/L6], vocabulary-once [L11], Forbidden-last, Unknowns line types [L10].
4. **Provenance marks.** The four-mark valve table (DECIDE unmarked / BELIEVE:path / GUESS / [HUMAN-ATTENDED]) with which side is authoritative per mark [L10/L12].
5. **Oracle bindings and the census.** Exactly one binding per claim; the three binding classes; zero-or-two-is-a-defect [L9].
6. **Lifecycle.** proposed → ratified (an ordinary operator commit flipping the Status line, keyboard only) → derived per sitting → closed by release receipt. Doubt blocks at ratified [L10]. Post-ratification legality: pointer to constitution A22 only — no restatement (one authority per fact).
7. **The lint rule index.** Table L1–L17 with severities and the deleted operator rule per row — the README's own consumer contract.

Removed from v1, with destinations: §Verification (analyze pass, the grind) → skills/author-spec (critique 5); §Traceability inside spec.md → trace.json (critique 1); §Position's design-load essay and §Provenance history → one line each, the rest stays in zeta-learnings (unlinted prose has no consumer here); the EARS dialect table → replaced by the arrow-line grammar (the costume drop, zeta-learnings/09).

## specs/constitution.md v2 — outline

Article ids are STABLE (A8, A15 are cited across committed bytes; renumbering breaks citations). Preamble keeps citation-is-the-argument.

- **A1, A3–A15, A18–A21 — unchanged in v2.** (The seam map's further critique — A5–A7 to tally's own crystallized spec, A16–A18/A20–A21 shrink-to-consumer — is recorded at the bottom of the file as *candidates for the tally-crystallization sitting*, citing lens-seams §Constitution critique. Not applied: 00-INDEX accepted only the five critiques above, and this file does not smooth that line.)
- **A2 — amended (critiques 2+3).** New text, three parts: (i) write half, absolute: the machinery never writes spec.md or the worklist; ratification and every spec-byte change is a human hand at a keyboard. (ii) read half, the bright line: "No machine *decision* — admission, dispatch, budget derivation, merge, failure classification — takes spec bytes as input. Machine *rendering* — the escalation report, the release record, campaign status — may resolve citations for human and judge eyes." (iii) deny-list scope: `specs/<armed-identity>/**` of the *governing* spec only — a campaign whose deliverable is a spec must be able to write specs it is not governed by (the crystallization campaign would otherwise refuse itself). Plus the direction sentence relocated from README v1:61–64: "the spec points at tasks; the worklist schema does not change." — flagged **DECISION-3** at ratification: this sentence was seam-map "missing #2", not on 00-INDEX's accepted-five list; it must live somewhere once README v2 drops unlintable law, and beside A2 is where the seam map argued it belongs.
- **A16, A17 — shrunk (critique 5).** One sentence each: "Contracts of consequence get dual blind derivation; disagreements escalate as spec defects" (A16); "Frozen inputs are recorded, never edited" (A17); both end with "procedure: skills/author-spec". The grind checklist and analyze steps move out entirely.
- **A22 — NEW: the freeze/append article (critique 4).** "A ratified spec.md admits exactly one class of in-file change: Status transitions (ratified → closed). Beside it, exactly two artifact classes may grow: appends to trace.json (structural-prefix rule; sitting rows by the author's hand at a sitting; release rows machine-rendered and human-committed, machine-appended only when the release verb owns it), and additions under evidence/. Nothing else. Any other diff under a ratified specs/<identity>/ is a defect." Citations: the v1 self-contradiction (README v1 froze spec.md at ratification while housing per-stage trace rows in its §7 — lens-seams Seam 6); E7's freeze genre; A21's terminal-state logic applied at spec altitude. Consumer: L17 + the sitting checklist.

## FIELD: author_skill

# skills/author-spec/SKILL.md — outline (house voice)

```
---
name: author-spec
description: Author, lint, and ratify a campaign spec under the house claim
  grammar, then sit the derivation from ratified spec to worklist stage. Use
  when creating or amending specs/<identity>/, resolving a spec's typed
  questions, ratifying at a boundary, or running a stage-boundary sitting.
---
```

# Author a spec and sit its derivation

Author against the observed tree, never a predicted one. Write claims, not
chapters. Doubt gets syntax, never tone. Ratify at a keyboard. Use
`assign-tally` for the worklist the sitting derives and `campaign-operator`
after arming.

## Author the claims

Read the real files before writing one line; a claim about the tree you did
not read this session is fiction. Write Outcome first — eight sentences or
stop and surface that you do not understand the task. Declare every term once
in Vocabulary; flag created identifiers `(NEW)`. Record every ambiguity you
resolve as a Rulings row — an empty Rulings section is itself a finding. Write
one claim per line, `condition → observable`, exactly one oracle binding
(`[check:]`, `[gate:]`, or `[HUMAN-ATTENDED]`). Mark what you read `BELIEVE:path`;
leave what you rule unmarked; suffix every numeral `(given)` or `(GUESS)`.
Omit a section by writing `Omitted: <reason>.` under its heading — never fill
it. Forbidden goes last, verb-first, one prohibition per line.

Then loop: run
`nix develop --command cargo run -p spec-lint -- specs/<identity> --worklist silent-factory-worklists/<identity>.json --repo-root . --mode sitting`,
fix, rerun until blocking defects are zero. Do not argue with the linter in
prose; change the bytes.

## Run the falsity pass

Hand the numbered claim lines — nothing else — to a fresh reader (operator, or
a fresh model instance with no loyalty to the prose) with one question:
"Which of these statements about my codebase are false?" Demote refuted
DECIDE lines, fix or delete refuted BELIEVE lines, and record every
correction as a Rulings row. Run one contradiction pass the same way: "do any
two numbered statements conflict?" Nothing about style; only facts.

## Ask, never guess

Emit doubt as typed lines: `DECISION-n <question>? proposed: <answer> (GUESS)`
for forks and unsourced constants; `UNKNOWN-n [BLOCKING] <statement> — <action>`
for what you could not determine. Answers arrive as typed replies, steers, or
commits — never as line edits to the artifact; regenerate the affected lines
from the answer and flip `(GUESS)` to `(given)`. Ratification happens only at
zero outstanding doubt: the operator, at a keyboard, commits the Status line
flip in an ordinary commit. No machinery hand ever touches the file.

## Sit the derivation

At each stage boundary, in order: run the edge census against the observed
tree; drain the questions queue; author or amend the worklist stage per
`assign-tally` with `readFirst` pointing at real `#rN` anchors and goals
citing claim ids instead of restating them; append `kind: "sitting"` trace
rows joining claim → task → acceptance ids; write the census report to
`specs/<identity>/evidence/census-<stage>.md`; rerun the sitting-mode lint;
rehearse admission per `assign-tally`. Commit once — worklist stage, trace
rows, census report — and push. Pre-ext1, ring the doorbell with
`tally campaign arm`; post-ext1 the push is the arming act and only the first
`run` stays deliberate. At close, render completion with
`spec-lint --coverage --append`, review, commit. Walk away; the next gated
head is the witness.

## FIELD: first_spec

# FIRST SPEC INSTANCE — recommendation and skeleton

## The weighing

**Reflexive candidate** — `specs/zeta-authority-plane/spec.md`, governing zeta's own deliverables (the lint, the trace, the release wiring). For: (a) the campaign that builds the authority plane runs tonight, so the first spec gets a standing consumer within hours, not weeks — the lint lints its own governing spec, the strongest possible bite proof (A15); (b) it is the first two-way round-trip by construction: DECIDE claims graded downward by gates the same night, BELIEVE claims about the seams (readFirst free strings, fleet-gate's flake-check step, the checks attribute set) falsifiable upward against the tree the moment either moves; (c) it exercises the scoped deny-list discipline for real — lanes write `specs/README.md` and `crates/spec-lint` while the governing dir `specs/zeta-authority-plane/**` stays out of every conflictDomain. Against: bootstrap ordering — the spec exists before its linter does; mitigated because most claims bind `[gate: cargo-tests]` (the crate's own tests), which bites the moment the crate task lands, and `[check: spec-lint]` bites from the flake task onward.

**Calmer candidate** — the receipts surface (stable, already tested): pure crystallization, nearly all BELIEVE, no derivation pressure. But it would give zeta a spec with *no campaign consuming it* — a spec whose trace stays empty and whose only consumer is the lint's grammar check. That is the grind's bar again, one layer up.

**Recommendation: the reflexive spec, `zeta-authority-plane`.** The calmer subject is the right *second* instance (the tally-crystallization campaign, "an overnight or two away" per doc 10), where nearly everything starts BELIEVE and the falsity pass promotes.

What it deletes (D58, since a spec is itself a mechanism): the operator rule "re-read the zeta design conversation / ZETA.md prose to know what the campaign must prove" — close conditions become claims with bindings, and completion becomes a trace join, not a recollection.

## Claims skeleton (real claims, verified bindings)

Bindings verified: `cargo-tests` gate genre exists (epsilon-extension.json:17–22) and the zeta worklist must carry that id verbatim (INTERFACES 3); `checks.x86_64-linux.spec-lint` is created by this campaign in the checks set at flake.nix:3455–3488; fleet-gate standing coverage verified at test/fleet-gate.sh:254; seam facts verified at examples/flows/spec-build.js:1050–1080 (goal maxLength 12000, specSections items maxLength 1000), crates/tally/src/cli/campaign.rs:6801 (`## Read first` rendered verbatim), campaign.rs:9200 (admission-schema coverage in cargo tests).

```
## Claims

### R1 — the linter and its bite
Why: a bar without a gate is not a bar (VD-5, F33); the lint is the layer's standing consumer.
1.1 `spec-lint --mode check` over a defect-free `specs/zeta-authority-plane` → exit 0 (given). [gate: cargo-tests]
1.2 `spec-lint --mode check` over `crates/spec-lint/tests/fixtures/must-fail` → exit 2 (given) with defect codes equal to `expected-defects.json`. [gate: cargo-tests]
1.3 a gated head where either side of 1.1/1.2 flips → the flake check attribute fails. [check: spec-lint]
1.4 a claim line with zero or two binding tokens → defect L9 (given). [gate: cargo-tests]
1.5 a numeral with no provenance on a non-BELIEVE claim line → defect L7 (given). [gate: cargo-tests]
1.6 a backticked identifier absent from tree, Vocabulary, and NEW set → defect L8 (given). [gate: cargo-tests]
1.7 `Status: ratified` with an outstanding GUESS or DECISION line → defect L10 (given). [gate: cargo-tests]

### R2 — the trace
Why: the freeze contradiction resolves only if the join lives beside the frozen file, append-only.
2.1 a trace row naming a claim id absent from `spec.md` → defect L14 (given). [gate: cargo-tests]
2.2 a release row with no prior sitting row for the same claim and task → defect L14 (given). [gate: cargo-tests]
2.3 in sitting mode, parent rows not a structural prefix of head rows → defect L17 (given). [gate: cargo-tests]
2.4 `trace.json` invalid against `contracts/trace.schema.json` → defect L14 (given). [gate: cargo-tests]

### R3 — the seams (the upward direction)
Why: the layer attaches with zero machinery change; these lines are falsified by the tree, not defended by it.
3.1 BELIEVE:examples/flows/spec-build.js — `specSections` items are free strings of maxLength 1000 → anchors like `specs/zeta-authority-plane/spec.md#r2` are admissible worklist bytes unchanged. [HUMAN-ATTENDED]
3.2 BELIEVE:crates/tally/src/cli/campaign.rs — the brief renders `## Read first` from `specSections` verbatim → the worker receives the anchor untouched. [HUMAN-ATTENDED]
3.3 BELIEVE:test/fleet-gate.sh — the ladder runs `nix flake check -L --keep-going` → the new attribute grades every fleet-gated head with zero fleet-gate edits. [check: spec-lint]

### R4 — the skills and docs
Why: doctrine in prose decays; the procedures land where agents execute them.
4.1 `skills/author-spec/SKILL.md` exists with the four sections and the sitting checklist → the sitting runs from committed bytes. [gate: spec-lint]
4.2 `specs/README.md` rule-index table and the implemented rule set diverge → a crate test fails. [gate: cargo-tests]

## Unchanged
U.1 worklist admission refuses unknown keys → the zeta worklist adds none and admits under schemaVersion 1 (given). [gate: cargo-tests]
U.2 the pre-existing checks (`module-layer`, `campaign-runtime`) build beside the new attribute → the flake stays green. [gate: flake-build-subset]

## Unknowns
UNKNOWN-1 whether an existing cargo test covers the `## Read first` brief rendering (3.2's would-be oracle) — the lint lane greps campaign.rs tests; if present, 3.2 rebinds from [HUMAN-ATTENDED] at the next sitting.
DECISION-1 identity name `zeta-authority-plane`? proposed: yes (given) — worklist filename must match; sequencing designer confirms.

## Forbidden
F.1 Do not add keys to the worklist schema.
F.2 Do not put model names in spec or worklist bytes.
F.3 Do not write under `specs/zeta-authority-plane/` from any lane.
F.4 Do not build an archive verb, a spec index file, or a specSha receipt stamp.
F.5 Do not bind any claim to a --list-only attribute.
```

Vocabulary declares `spec-lint (NEW)`, `trace.json (NEW)`, `sitting`, `binding`, `claim`, `mark` — so L8 passes on the bootstrap identifiers. The 3.x BELIEVE lines carry the exact paths; their numerals (1000) are sourced by the BELIEVE path per L7.

## FIELD: sitting_procedure

# THE SITTING — human-attended compile, spec → worklist stage

One operation, one commit, witnessed by the next gated head. Inputs: the observed tree, the ratified spec, the drained questions queue. The steps, in order:

1. **Census.** Run the edge census (EQ §2.4 steps, as committed by ext0's `authoring-doctrine-skills` into assign-tally) against the observed tree at the boundary (A12/F42), plus the oracle census over the spec: enumerate every claim's binding; `[HUMAN-ATTENDED]` claims get their attending moment named in the census report.
2. **Drain doubt.** Every outstanding `(GUESS)`, `DECISION-n`, `UNKNOWN-n [BLOCKING]` gets a typed operator answer (reply, steer, or commit — never a line edit); the author regenerates the affected lines, flipping `(GUESS)` to `(given)`. Derivation is impossible while any remain (L10 sitting mode).
3. **Ratify if this is the ratification boundary.** The operator, at a keyboard, flips the `Status:` line in an ordinary commit — the push credential is the trust root; no machinery hand touches the file. (May ride the same commit as step 6 when the first stage derives at ratification; the status-flip bytes are still typed by the operator's hand.)
4. **Author the stage.** Write or amend the worklist stage per `skills/assign-tally`: `readFirst` pointing at real number-derived anchors (`specs/<id>/spec.md#r2`), goals citing claim ids and evidence ids instead of restating them (the goal reverts to its D13 pre-digested role; lens-seams S10). Spec churn reaches budgets ONLY through these precise task amendments — the sitting is the filter that keeps the spec out of the epoch key.
5. **Append trace rows.** `kind: "sitting"` rows joining claim → task → acceptance ids (+ evidence ids), written by the authoring model's hand, seq continuing the file.
6. **Record the census.** `specs/<identity>/evidence/census-<stage>.md` — record-don't-fix: deviations recorded, never patched into frozen inputs (A17).
7. **Rehearse.** Run `spec-lint --mode sitting` (adds L17 append-only and hard L10) and the admission rehearsal per assign-tally (every preflight argv verbatim in a pristine worktree). Fix worklist or host until clean.
8. **One commit, push.** Worklist stage + trace rows + census report in a single commit on the base.
9. **Arm.** Pre-ext1: `tally campaign arm OWNER/REPO silent-factory-worklists/<identity>.json` — the deliberate doorbell (R2). **Post-ext1: this step deletes — the push of step 8 IS the arming act** (poll re-admission admits the new committed worklist sha; `armSerial` becomes a derived counter; only the campaign's very first `run` stays a deliberate doorbell). The sitting collapses to: sit, author, commit, push, walk away.
10. **Witness.** Nothing to do: the next gated head runs `checks.x86_64-linux.spec-lint` via fleet-gate's flake-check step; the sitting's own output is bitten by the same ladder as everything else. At the campaign's close boundary, the close sitting runs `spec-lint --coverage --append` to write the release rows (machine-rendered, human-committed), and hands release the Destination section plus the rendered coverage table as the operator-authored intent it already renders verbatim.

Trace rows are written at steps 5 (sitting rows) and 10 (release rows); no other moment writes them. No mid-run human gates exist anywhere in this list — every step sits at a boundary (A19).

