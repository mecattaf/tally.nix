# l11-vocabulary-drift — a term defined twice

Status: proposed
Governs: silent-factory-worklists/l11-vocabulary-drift.json
Consumers: the spec-lint must-fail corpus test
Supersedes: none

## Outcome

This spec exists to fail one rule class. Every other rule passes over it. The
corpus test counts the defect it produces.

## Vocabulary

- corpus spec (NEW) — the deliberately broken spec this file is.
- corpus spec — the same noun, declared a second time.

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

Omitted: no doubt outstanding.

## Stages

Omitted: the corpus spec is not built.

## Forbidden

F.1 Do not repair this file.
