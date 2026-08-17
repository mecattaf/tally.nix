# spec-lint

The spec layer's enforcement engine. `specs/README.md` is the rule list; this
crate is that rule list executed. One line-oriented parser reads a
`specs/<identity>/spec.md` — never a markdown AST — and every rule the §7 index
marks as a single-spec rule reports at most one line per defect.

```console
$ spec-lint specs/zeta specs/eta
specs/zeta/spec.md:93: L10: a `(GUESS)` is outstanding; doubt is resolved by a typed operator answer before `Status: ratified`
```

## Contract

- `spec-lint DIR...` — each `DIR` is a `specs/<identity>/` directory.
- A directory without a `spec.md` is skipped silently: an evidence-only
  identity directory is legal (`specs/README.md` §2).
- Each defect prints exactly one stderr line: `<file>:<line>: <rule-id>: <message>`.
  Nothing is written to stdout.
- Exit 0 when every rule passes, 1 on warnings only, 2 on any blocking defect.
- `--mode check` is the pass this crate implements and the default.
- `--root DIR` sets the working tree that paths resolve against. Without it the
  root is inferred: the parent of `specs/` when the directory sits under one,
  and the directory itself otherwise — so `spec-lint specs/zeta` resolves
  against the repository, and a fixture corpus resolves against itself wherever
  the lint runs from.

## The rules

`src/rules.rs` carries the §7 index — every rule id and its severity cell — and
a crate test asserts parity with the README table, which is why the README names
this crate its standing consumer. Rules marked `Stage::Resolution` (L13, L14,
L17) are catalogued but evaluated by the cross-artifact pass, which resolves
worklist pointers, trace rows, and history.

Three judgments the README leaves to the implementation, fixed here as bytes:

- **Out-of-context identifiers (L8).** A backticked span is split into tokens,
  and a token is judged only when it looks like an identifier rather than an
  English word: it carries `/`, `_`, `::`, a dotted or hyphenated join, or
  camelCase. Flags (`--mode`, `-L`) are shell surface, not identifiers. A token
  is in context when it names a path under the root or beside the spec, when the
  Vocabulary section carries it (including under a `(NEW)` flag), or when it
  appears in the bytes of a file the spec BELIEVEs. A trailing `#anchor` is a
  pointer into a file, not part of its name; resolving the anchor belongs to the
  cross-artifact pass.
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

## The fixtures

A linter never shown to bite is the `--list-only` flake attribute reborn
(VD-5, F33), so the corpus is part of the crate:

- `tests/fixtures/golden/` — one clean minimal spec. Every rule passes over it.
- `tests/fixtures/must-fail/` — one directory per rule class, each breaking
  exactly the rule its name carries, plus `expected-defects.json`: the exact
  `{rule-id: count}` map the whole corpus reproduces. `l10-blocking-doubt/`
  carries a one-line worklist file because its spec is ratified, and a ratified
  spec's `Governs` target must exist.
- `tests/fixtures/evidence-only/` — an identity directory with no `spec.md`,
  which the lint skips silently.

Every fixture is crate-local, so `cargo test -p spec-lint` proves the bite
wherever the crate builds. Lint the corpus by hand with
`spec-lint tests/fixtures/must-fail/*/`.

Two tests read the committed `specs/` tree instead: the §7 rule-index parity
test and the accept/reject corpus at
`specs/zeta/contracts/claim-line.fixtures.json`. Both SKIP with a printed note
when no ancestor of the crate carries `specs/README.md`, because the workspace
derivation builds from a filtered source that does not include it.
