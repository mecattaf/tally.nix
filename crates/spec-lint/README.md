# spec-lint

The spec layer's enforcement engine. `specs/README.md` is the rule list; this
crate is that rule list executed. One line-oriented parser reads a
`specs/<identity>/spec.md` — never a markdown AST — and three modes read what it
produces: the check pass, the census, and the coverage render.

```console
$ spec-lint specs/zeta specs/eta
specs/zeta/spec.md:93: L10: a `(GUESS)` is outstanding; doubt is resolved by a typed operator answer before `Status: ratified`
```

## Contract

- `spec-lint DIR...` — each `DIR` is a `specs/<identity>/` directory.
- A directory without a `spec.md` is skipped silently: an evidence-only
  identity directory is legal (`specs/README.md` §2).
- Each defect prints exactly one stderr line: `<file>:<line>: <rule-id>: <message>`.
  The check pass writes nothing to stdout; the census and coverage modes write
  their table there and their defects on stderr as usual.
- Exit 0 when every rule passes, 1 on warnings only, 2 on any blocking defect.
- `--mode check` is the default: the single-spec rules, then the cross-artifact
  resolution pass. `--census` and `--coverage` are shorthands for
  `--mode census` and `--mode coverage`.
- `--root DIR` sets the working tree that paths resolve against. Without it the
  root is inferred: the parent of `specs/` when the directory sits under one,
  and the directory itself otherwise — so `spec-lint specs/zeta` resolves
  against the repository, and a fixture corpus resolves against itself wherever
  the lint runs from.
- `--worklist FILE` names the governing worklist. Without it each directory
  reads `<root>/silent-factory-worklists/<identity>.json`, which is the only
  form that stays right for more than one directory at a time.

## The rules

`src/rules.rs` carries the §7 index — every rule id and its severity cell — and
a crate test asserts parity with the README table, which is why the README names
this crate its standing consumer. Rules marked `Stage::Resolution` are evaluated
by the cross-artifact pass rather than by the single-spec one: L13 and L14 run
today, and L17 (trace append-only against the parent revision) waits on sitting
mode, which is the only place a parent revision exists.

Three judgments the README leaves to the implementation, fixed here as bytes:

- **Out-of-context identifiers (L8).** A backticked span is split into tokens,
  and a token is judged only when it looks like an identifier rather than an
  English word: it carries `/`, `_`, `::`, a dotted or hyphenated join, or
  camelCase. Flags (`--mode`, `-L`) are shell surface, not identifiers. A token
  is in context when it names a path under the root or beside the spec, when the
  Vocabulary section carries it (including under a `(NEW)` flag), or when it
  appears in the bytes of a file the spec BELIEVEs.
- **` and ` joining two verbs (L4).** The word after the ` and ` must be
  verb-shaped (`-s`, `-es`, `-ed`) and the left half must already carry a verb.
  A comma before the ` and ` marks an enumeration, which is a list rather than a
  second claim. The heuristic is deliberately blunt: a noun pair that reads as
  two verbs is a line worth splitting anyway.
- **Model names (L16).** `src/lexicon.rs` holds the families. `codex` is
  deliberately absent: this tree ships `skills/steer-codex`, and a spec citing
  that path names a committed directory rather than a host-catalog row.

A numeral is sourced when its line carries a `BELIEVE:` mark, a `(given)`
suffix, or a `(GUESS)` suffix. A numeral welded to an id — `R2`, `F.2`,
`UNKNOWN-1`, `#r2`, or a dotted pair like `1.2` — is a cross-reference and
exempt. A long line wrapped with an indented continuation is one logical line:
wrapping at the margin is typography, not a second claim.

## The cross-artifact pass

§1 fixes the layer's whole claim as one chain — spec → worklist → receipts →
release — and every link committed bytes. A pointer checked by eye is the
48-phantom-pointer class waiting to happen, so `src/resolution.rs` resolves the
chain mechanically as part of the default check:

- **L13.** Every `readFirst.specSections` string in the governing worklist that
  starts with `specs/` names a file that exists, and, where it carries an
  anchor, an anchor that file offers. Anchors are number-derived — `### R2 —
  the trace` offers `#r2` and nothing else, which is what makes a retitle safe
  (§3) — and every other heading offers its slug. Evidence citations on trace
  rows resolve the same way.
- **L14.** `trace.json` validates against its committed contract, names the
  spec it sits beside, and every row names a claim the spec declares, a task
  the worklist declares, and acceptance ids that task declares. A release row
  with no earlier sitting row for the same claim and task is a defect, and so
  is a claim traced to no task and listed under no unauthored stage.

Four judgments this pass fixes as bytes:

- **What counts as absent.** Either artifact may be missing. A spec proposed
  before its boundary sitting governs no worklist and has no trace rows yet;
  absence is a lifecycle state, and the pass is silent over it. The pointer
  rules need the worklist, the row rules need the trace, and the coverage rule
  needs both.
- **What owes a trace row.** A claim line under `## Claims` does. An unchanged
  line does not: §3 binds it to an oracle that already passes, so it belongs to
  no task. A claim whose group is listed by an unauthored stage does not either.
- **Which stage is unauthored.** One that names no task the governing worklist
  declares. §3 says an unauthored stage lists ruling ids and claim-group refs
  and nothing more; a stage that names its tasks has been authored, and its
  claims are owed rows.
- **Where the trace contract comes from.** `contracts/trace.schema.json` beside
  the spec first, then `specs/zeta/contracts/trace.schema.json`, which the whole
  layer shares. A trace with no contract in reach is itself a defect: a file
  reported clean by an oracle that was never found is the `--list-only` flake
  attribute again.

`src/schema.rs` is that oracle. It implements exactly the JSON Schema 2020-12
keyword set the trace contract uses and refuses to run over any other: an
unimplemented keyword is a hard error naming itself, never a silent pass, so the
verdict can never be narrower than the contract. (The workspace carries a
general `jsonschema` dependency, which this crate does not take: adding it
rewrites `Cargo.lock`, and `Cargo.lock` is outside this crate's write boundary.
The contract file stays the single authority either way — nothing here copies
it, and a crate test pins every fixture copy to it byte for byte.)

## The census and the coverage render

`spec-lint --census DIR` enumerates the oracle binding of every claim and
unchanged line: `check`, `gate`, `HUMAN-ATTENDED`, or the `L9` defect of zero or
two. A gate is witnessed against the governing worklist, so the row renders the
argv that will actually run, and a gate id the worklist does not declare is the
other half of `L9`. This is what §6 means by coverage being an enumeration
rather than a judgment — the question "is this tested anywhere" stops being
asked and starts being read down a column.

`spec-lint --coverage DIR` renders the claim ↔ task ↔ acceptance-id ↔ evidence
join as one markdown table on stdout, one row per claim ↔ task pair, ordered by
the trace's own append order, with untraced claims present and dashed. The
operator hands it to `tally campaign release` verbatim as the close-out proof,
which is only honest if it is a rendering and not a retelling: every cell comes
from `trace.json` and the spec beside it, nothing reads a clock or a directory
listing, and `tests/fixtures/joined.coverage.md` is the golden the render is
compared against. This is what deletes the hand-rendered close-out table.

## The fixtures

A linter never shown to bite is the `--list-only` flake attribute reborn
(VD-5, F33), so the corpus is part of the crate:

- `tests/fixtures/golden/` — one clean minimal spec. Every rule passes over it,
  and it governs no worklist, so it is also the proof that the cross-artifact
  pass stays silent over an identity that has not been derived yet.
- `tests/fixtures/joined/` — the clean cross-artifact fixture. It is a whole
  miniature working tree rather than a directory, because a join has nothing to
  resolve against without a root: one governing worklist, one spec, one trace,
  one evidence ledger, and the trace contract. `joined.coverage.md` and
  `joined.census.md` beside it are the goldens the two render modes reproduce.
- `tests/fixtures/must-fail/` — one directory per rule class, each breaking
  exactly the rule its name carries, plus `expected-defects.json`: the exact
  `{rule-id: count}` map the whole corpus reproduces. `l10-blocking-doubt/`
  carries a one-line worklist file because its spec is ratified, and a ratified
  spec's `Governs` target must exist. `l13-phantom-pointer/` and
  `l14-orphan-trace-row/` carry the worklist and trace that break, with clean
  spec bytes: the cross-artifact rules break beside the spec, not inside it.
- `tests/fixtures/evidence-only/` — an identity directory with no `spec.md`,
  which the lint skips silently.

Every fixture is crate-local, so `cargo test -p spec-lint` proves the bite
wherever the crate builds. Lint the corpus by hand with
`spec-lint tests/fixtures/must-fail/*/`.

Three tests read the committed `specs/` tree instead: the §7 rule-index parity
test, the accept/reject corpus at
`specs/zeta/contracts/claim-line.fixtures.json`, and the byte-for-byte pin of
every fixture `trace.schema.json` against `specs/zeta/contracts/`. All three
SKIP with a printed note when no ancestor of the crate carries
`specs/README.md`, because the workspace derivation builds from a filtered
source that does not include it.
