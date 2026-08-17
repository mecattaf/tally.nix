# l10-blocking-doubt — doubt outstanding at ratified

Status: ratified 2026-08-17
Governs: silent-factory-worklists/l10-blocking-doubt.json
Consumers: the spec-lint must-fail corpus test
Supersedes: none

## Outcome

This spec exists to fail one rule class. Every other rule passes over it. The
corpus test counts the defect it produces.

## Vocabulary

- corpus spec (NEW) — the deliberately broken spec this file is.

## Rulings

| id | decision | ruling |
|---|---|---|
| C1 | breakage | this corpus spec breaks one rule class |

## Claims

### R1 — the single breakage
Why: one broken rule per file keeps the defect map readable.
1.1 the linter reads this corpus spec → the run reports the named defect (given). [gate: cargo-tests]

## Unchanged

Omitted: the corpus spec changes nothing.

## Unknowns

UNKNOWN-1 [BLOCKING] which rule class this corpus spec breaks — ask the operator.
DECISION-1 which steward signs the corpus? proposed: the narrator (GUESS)

## Stages

Omitted: the corpus spec is not built.

## Forbidden

F.1 Do not repair this file.
