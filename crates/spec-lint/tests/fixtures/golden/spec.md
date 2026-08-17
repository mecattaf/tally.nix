# golden — the clean minimal spec

Status: proposed
Governs: silent-factory-worklists/golden.json
Consumers: the spec-lint corpus test; the flake check that re-runs it
Supersedes: none

## Outcome

The linter carries one committed spec that every rule passes over. Before this
file, a silent lint run proved only that nothing had been read. After it,
silence over the golden fixture is a fact about the rules rather than a fact
about the reader. The must-fail corpus proves the other half of the same claim.

## Vocabulary

- golden fixture (NEW) — the clean spec this file is, kept small enough that a
  new rule which breaks it is visible in one diff.
- evidence note (NEW) — the file under `evidence/` this spec believes.
- spec-lint (NEW) — the enforcement engine this fixture feeds.

## Rulings

| id | decision | ruling |
|---|---|---|
| G1 | fixture identity | the directory name is the identity; the title line carries it |
| G2 | fixture size | one claim group, one unchanged line, one prohibition per shape |

## Claims

### R1 — the clean pass
Why: a linter with no clean fixture cannot tell silence from success.
1.1 `spec-lint` reads this file → the run reports zero defects (given). [gate: cargo-tests]
1.2 BELIEVE:evidence/example.md — the evidence note names the `golden fixture` → the believed path resolves at the lint revision. [check: spec-lint]
1.3 a rule lands with no mechanical oracle → the golden fixture names its human moment. [HUMAN-ATTENDED]

## Unchanged

U.1 a rule joins the set → this file stays clean (given). [gate: cargo-tests]

## Unknowns

Omitted: no doubt outstanding.

## Stages

### S1 — the fixture
Rulings G1, G2; claims R1.

## Forbidden

F.1 Do not add a second spec to this directory.
F.2 Never bind a claim here to an oracle this tree cannot name.
