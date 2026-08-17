# joined — the clean worklist and trace pair

Status: proposed
Governs: silent-factory-worklists/joined.json
Consumers: the spec-lint resolution test; the coverage golden it renders
Supersedes: none

## Outcome

The golden fixture proves the single-spec rules stay silent over a clean file.
This one proves the same of the cross-artifact half, which no single file can
show: it is a joined tree, with a governing worklist whose pointers resolve, a
trace whose rows name claims and tasks and acceptance ids that exist, and a
claim group left to an unauthored stage. Before it, silence from the resolution
pass proved only that no worklist had been found.

## Vocabulary

- joined tree (NEW) — the miniature working tree this fixture is: one worklist,
  one spec, one trace, one evidence ledger.
- spec-lint (NEW) — the enforcement engine this fixture feeds.
- unauthored stage (NEW) — a stage that lists claim groups and rulings and
  names no task, so its claims are owed no trace row yet.

## Rulings

| id | decision | ruling |
|---|---|---|
| J1 | fixture shape | the fixture is a tree, not a directory: the join needs a root to resolve against |
| J2 | release row | one release row, so the coverage render is proven to fold a witness into its evidence cell |

## Claims

### R1 — the resolved join
Why: a pointer checked by eye is the phantom-pointer class waiting to happen.
1.1 the worklist reads first from `specs/joined/spec.md` and its evidence ledger → every pointer resolves at this revision. [gate: corpus-tests]
1.2 BELIEVE:trace.json — the trace rows name `the-first-task` and `the-second-task` → the join resolves each to a worklist task. [check: spec-lint]

### R2 — the unauthored remainder
Why: a claim nobody has scheduled is legal; a claim nobody can find is not.
2.1 a claim group listed under an unauthored stage → the trace owes it no row yet. [HUMAN-ATTENDED]

## Unchanged

U.1 the single-spec rules run first → this file passes every one of them. [gate: corpus-tests]

## Unknowns

Omitted: no doubt outstanding.

## Stages

### S1 — the joined stage
Order: the-first-task, then the-second-task. Claims R1; rulings J1, J2.

### S2 — the unauthored stage
Unauthored. Claims R2.

## Forbidden

F.1 Do not add a second spec to this tree.
F.2 Never point this worklist at a file outside the tree.
