# The tally spec layer — house format v2

Status: **supersedes v1 (2026-08-14, same day — the accepted critiques applied;
the removed prose lives in `zeta-learnings/`, unlinted prose has no consumer
here).** Standing consumer: the `crates/spec-lint` crate test that parses §7's
rule-index table and asserts parity with the implemented rule set — drift
between this file and the linter fails `cargo-tests`. Discipline: every
sentence below is either a lint rule (annotated `[L#]`) or a pointer to one.

## 1. Position

The spec sits above the worklist and never replaces it. The worklist stays the
only machine-admitted authority, schema closed (constitution A2). The spec
points at tasks; the worklist schema does not change (A2). The lineage is one
chain: **spec → worklist (derived per sitting) → receipts → release**, and
every link is committed bytes (A1). Procedures — authoring, the falsity pass,
the grind, the sitting — live in `skills/author-spec`, not here.

## 2. The artifact set

One directory per campaign identity — `specs/<identity>/` — matching the
worklist filename in `silent-factory-worklists/`. No numeric prefixes:
identity is the join key. A spec-less identity directory (evidence-only, e.g.
`specs/epsilon-extension/`) is legal and skipped by the lint.

- **`spec.md`** — required; grammar in §3–§5; frozen at ratification except as
  constitution A22 enumerates.
- **`trace.json`** — the append-only three-way join claim ↔ task ↔
  receipt/commit `[L14, L17]`; schema at `contracts/trace.schema.json`;
  sitting rows by the author's hand at sittings, release rows machine-rendered
  and human-committed (A2's rendering half).
- **`contracts/`** — byte oracles: schemas, accept/reject fixtures. Every
  schema is double-pinned: validated at lint runtime and by a crate test.
- **`evidence/`** — the graded ledgers the spec and the worklist cite;
  committing them is what makes citations resolvable at the authority
  revision `[L13]` (A9 applied to specs).

## 3. spec.md grammar

Line-oriented by design — the linter parses a line grammar, never a markdown
AST.

**Status block** (preamble, before the first `##`) `[L2]`:

```
# <identity> — <title>
Status: proposed | ratified YYYY-MM-DD | closed <release-ref>
Governs: silent-factory-worklists/<identity>.json
Consumers: <at least one — gate, check attribute, skill, or sitting>
Supersedes: <path> | none
```

`Consumers` non-empty is law (A15: a bar without a gate is not a bar) `[L2]`.
`Governs` must name an existing file once Status is ratified `[L2]`.

**Section set, exact order** `[L1]`:

1. `## Outcome` — `#outcome`. 3–8 sentences, observable before/after
   difference. Never omittable.
2. `## Vocabulary` — `#vocabulary`. Lines `- <term> — <definition>`, optional
   `(NEW)` flag on identifiers this campaign creates. One noun per concept,
   declared once, used identically forever `[L11]`.
3. `## Rulings` — `#rulings`. Table `| id | decision | ruling |`; ids match
   `[A-Z][0-9]+` and must not collide with the `R[0-9]+` claim-group
   namespace `[L3]`. Every ambiguity resolved while authoring gets a row; an
   empty or omitted Rulings section is a warning `[L15]`.
4. `## Claims` — `#claims`. Groups `### R<n> — <name>`, anchor derived from
   the number only: `### R2 — the trace` → `#r2` (retitle-safe by
   construction) `[L3]`. Group body: one plain-prose *why* line, then claim
   lines (§4).
5. `## Unchanged` — `#unchanged`. Flat arrow lines
   `U.<m> <condition> → <observable that continues to hold> [binding]`, bound
   to already-passing oracles `[L4, L9]`.
6. `## Unknowns` — `#unknowns`. Two typed line forms only `[L10]`:
   `UNKNOWN-<n> [BLOCKING]? <what could not be determined> — <action>` and
   `DECISION-<n> <question>? proposed: <answer> (GUESS|given)`.
7. `## Stages` — `#stages`. `### S<n> — <name>` → `#s<n>` `[L3]`. Build order
   only, no calendar (A13). Unauthored stages list ruling ids and claim-group
   refs, nothing more (A12).
8. `## Forbidden` — `#forbidden`. **Always the last section** `[L1]`. Lines
   `F.<m> Do not <...>` or `F.<m> Never <...>` — verb-first negation, one
   prohibition per line `[L4]`. The `F.` dot separates spec-forbidden ids from
   evidence-ledger finding ids (`F38`).

**Omission rule** `[L15]`: a section other than Outcome and Claims may be
omitted only by keeping its heading with the single body line
`Omitted: <one-line reason>.` — never by deleting the heading.

## 4. Claim lines

```
<g>.<m> [BELIEVE:<path> — ] <condition> → <observable> [check: <attr> | gate: <id> | HUMAN-ATTENDED]
```

- `<g>` equals the enclosing `### R<g>` group number; `<m>` ascending within
  the group; ids globally unique `[L3]`.
- Arrow `→` mandatory; exactly one claim per line; no ` and ` joining two
  verbs in the observable `[L4]`.
- Hedge lexicon banned in Claims/Unchanged/Forbidden (warning in
  Outcome/Rulings): should, ideally, typically, appropriately, robust,
  gracefully, as needed, if necessary, reasonable, properly `[L5]`. `e.g.`
  and `etc.` banned document-wide `[L6]`.
- Every numeral that is not a cross-reference (claim id, ruling id, stage id)
  must be sourced: on a `BELIEVE:<path>` line, suffixed `(given)` (operator
  supplied), or suffixed `(GUESS)` `[L7]`.
- Every backticked identifier must appear in the tree at the lint revision, in
  Vocabulary, or under a `(NEW)` declaration `[L8]`.
- No length maximum — no speculative rules; authoring guidance lives in
  `skills/author-spec`.

## 5. Provenance marks — the two-way valve

| mark | syntax | authoritative side | on conflict |
|---|---|---|---|
| DECIDE | *unmarked* (the default state of a claim line) | the spec | oracle fails → the code is wrong; the gate ladder mechanizes this direction |
| BELIEVE | `BELIEVE:<path> — ` after the claim id | the tree | tree disagrees → the spec is wrong `[L12]`: the path must exist, and every backticked identifier on the line must appear in the named file's bytes — drift is a build failure the day it starts |
| GUESS | `(GUESS)` suffix on a numeral or DECISION line | nobody — it blocks | outstanding at `Status: ratified` = blocking defect `[L10]`; resolved only by a typed operator answer, then rewritten `(given)` |
| HUMAN-ATTENDED | `[HUMAN-ATTENDED]` as the claim's binding | the named human moment | legal and enumerated by the census — an oracle gap declared, never discovered `[L9]` |

Certainty is the ground state; only doubt needs syntax.

## 6. Oracle bindings and lifecycle

Every claim and unchanged line carries **exactly one** binding: a named flake
check attribute `[check: <attr>]`, a witnessed gate argv `[gate: <id>]`, or
`[HUMAN-ATTENDED]`. Zero or two is a defect `[L9]` — byte-oracle-or-nothing;
coverage is an enumeration, not a judgment. `[check:]` must resolve in the
flake's checks set; `[gate:]` must resolve to a gate id in the governing
worklist `[L9]`.

Lifecycle: **proposed** (doubt legal, warnings `[L10]`) → **ratified** — an
ordinary operator commit flipping the Status line, keyboard only; doubt blocks
`[L10]` → derived per sitting → **closed** by the campaign's release receipt.
Post-ratification legality: constitution A22, and only A22 — no restatement
here (one authority per fact).

## 7. The lint rule index

| rule | catches | deletes (D58) | severity |
|---|---|---|---|
| L1 | section set, order, Forbidden-last, omission grammar | the structural half of the analyze pass | blocking |
| L2 | status-block grammar; empty Consumers; ratified Governs missing | the anti-rot audit glance (VD-5, F33) | blocking |
| L3 | anchor/id grammar; uniqueness; namespace collisions | hand-verifying citation targets after a retitle | blocking |
| L4 | claim-line shape; ` and `-joined verbs; non-verb-first Forbidden | the compound-criteria review | blocking |
| L5 | hedge lexicon in Claims/Unchanged/Forbidden | the vague-qualifiers sweep | blocking (warn elsewhere) |
| L6 | `e.g.` / `etc.` document-wide | the open-set enumeration review | blocking |
| L7 | unsourced numerals | the read-every-number confirmation sweep | blocking |
| L8 | out-of-context identifiers | the mechanical half of the falsity pass | blocking |
| L9 | zero-or-two oracle bindings; unresolvable check/gate refs | the "is this tested anywhere" judgment | blocking |
| L10 | outstanding GUESS / DECISION-n / UNKNOWN [BLOCKING] at ratified, or in sitting mode | the pre-derivation hedge re-read; doubt becomes a typed queue | blocking at ratified; warning at proposed |
| L11 | vocabulary defined twice; defined-never-used | the vocabulary-drift review | blocking / warning |
| L12 | BELIEVE path or identifier unresolvable | the "is the spec still true of the tree" re-read | blocking |
| L13 | dangling `specs/**` pointers in the governing worklist; unresolvable evidence ids | the 48-phantom-pointer class, checked by eye | blocking |
| L14 | trace rows referencing unknown claims/tasks/acceptance ids; release-before-sitting; schema-invalid trace | the hand-maintained trace table | blocking |
| L15 | missing section without an `Omitted:` reason; empty Rulings | template-completeness review; fabricated filler | blocking / warning |
| L16 | model names in spec or governing worklist bytes | the host-catalog leakage review | blocking |
| L17 | trace not append-only vs parent revision (sitting mode only — the flake sandbox has no history) | the trace-integrity re-read | blocking in sitting mode |
| L18 | acceptance-argv write targets outside the task's declared `conflictDomains` | the boundary refusal read back from a mid-flight lane transcript | blocking |

A task's `conflictDomains` are the one write boundary its lane is granted, and
its acceptance criteria are the oracle it is graded by. A criterion requiring a
path to exist, to be gone, or to have been rewritten outside that boundary
names a write the lane may not make, so the task cannot pass its own acceptance
`[L18]`. A path a criterion only reads — a pattern, a suite it runs, a build
target — settles nothing about who owns the byte and stays advisory where
admission already reports it.

Self-test is not a numbered rule but harness law: the crate ships a must-fail
fixture corpus with an exact expected-defects map, and the flake check re-runs
it inside its own derivation — the lint is the spec layer's standing consumer,
and the must-fail corpus is the lint's own.
