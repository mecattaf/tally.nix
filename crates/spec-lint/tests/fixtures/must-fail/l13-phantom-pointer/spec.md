# l13-phantom-pointer — a worklist pointing at a spec that is not there

Status: proposed
Governs: silent-factory-worklists/l13-phantom-pointer.json
Consumers: the spec-lint must-fail corpus test
Supersedes: none

## Outcome

This spec exists to fail one rule class. Every other rule passes over it. The
corpus test counts the defect it produces. The break is in the worklist beside
it rather than in these bytes: one task reads first from a file that does not
exist, and one reads first from an anchor its target does not offer.

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

Omitted: no doubt outstanding.

## Stages

Omitted: the corpus spec is not built.

## Forbidden

F.1 Do not repair this file.
