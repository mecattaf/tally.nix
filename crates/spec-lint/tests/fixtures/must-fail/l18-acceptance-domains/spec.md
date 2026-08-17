# l18-acceptance-domains — a worklist whose acceptance writes outside its boundary

Status: proposed
Governs: silent-factory-worklists/l18-acceptance-domains.json
Consumers: the spec-lint must-fail corpus test
Supersedes: none

## Outcome

This spec exists to fail one rule class. Every other rule passes over it. The
corpus test counts the defects it produces. The break is in the worklist beside
it rather than in these bytes: one task is graded by acceptance criteria that
require a file outside its declared write boundary to exist and another to be
written. The tasks beside it stay clean — one reads outside its boundary and
writes inside it, and one declares no boundary at all.

## Vocabulary

- corpus spec (NEW) — the deliberately broken spec this file is.

## Rulings

| id | decision | ruling |
|---|---|---|
| C1 | breakage | this corpus spec breaks one rule class |

## Claims

### R1 — the single breakage
Why: one broken rule per file keeps the defect map readable.
1.1 the linter reads this corpus spec → the run reports the named defects (given). [gate: cargo-tests]

## Unchanged

Omitted: the corpus spec changes nothing.

## Unknowns

Omitted: no doubt outstanding.

## Stages

Omitted: the corpus spec is not built.

## Forbidden

F.1 Do not repair this file.
