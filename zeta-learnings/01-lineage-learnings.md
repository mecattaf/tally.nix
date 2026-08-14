# Learnings I — The tally lineage, July 31 → August 14

*What fifteen days of day-numbered run records teach about the spec layer
tally is about to grow. Distilled 2026-08-14 from the full lineage recovery.*

## Tally is already spec-driven — from the tasks downward

The strongest single fact in the lineage: tally never lacked spec-driven
development, it lacked the top half of it. The worklist is an executable
specification — every acceptance criterion an argv, ordering as dependencies,
invariants as gates, ownership as path domains, with the standing rule "do
not put an operational requirement only in prose." The plan document is
machine-consumed, not documentation. What never existed is the altitude
*above* the worklist: a committed surface for destination, rulings,
requirements, and evidence with stable identifiers.

The pressure evidence is inside the live worklist itself. The `goal` field of
`epoch-scoped-budgets` is a full requirements document compressed into one
paragraph — behavior, rationale, evidence citations (CA-2, PA-05, F37), and
design constraints, all in prose, because there is nowhere above the worklist
for that material to live. Every task in `epsilon-extension.json` does this.
The spec layer's job is to relieve exactly that compression: the worklist
carries the what-and-prove-it; the spec owns the why-and-from-what-evidence,
citable by ID instead of restated.

## Doctrine beats code, measurably

PA-34 is the load-bearing measurement: the ladder's most measurable
improvement came from doctrine, not code — authority corrections fell from
41% to 11% through census-authoring and H1, not through any machinery change.
The equipment ledger sharpened it: on the epsilon record, a well-equipped run
needed zero ownership corrections, zero gate cycles, zero steers. This is the
spec-kit thesis (templates as the enforcement surface) proven independently,
on tally's own numbers, before anyone proposed adopting a spec framework.

The corollary the lineage states outright: **doctrine in prose decays;
doctrine in committed bytes does not.** The day documents are the decayed
form — fifteen days of hard-won rules living in files git does not track.
As of today, *everything* from `aug10-midday-session.md` forward is
untracked, including `epsilon-extension.json` itself, and the five Aug-14
excavation ledgers (PA/VD/CA/EQ plus the design pass) live in a session
scratchpad outside the repository entirely. E6 — commit the record — is
listed as a one-line operator pre-step, but it is really the founding act of
the spec layer: citations only resolve when the cited documents exist at the
authority revision (the worker-context law, D68, applied to specs).

## The rules that transfer directly into spec authoring

Four lineage laws are spec-authoring doctrine already, needing only a new
home:

**The ownership law.** A task must own every file its change makes false.
Learned across four distinct defect classes (textual, semantic-schema,
host-state leakage, test-asserts-replaced-behavior), and only the first is
lintable — H2's textual preflight scored 0-for-4 against real grants. The
working replacement is a question, not a lint: "which existing assertions
does this change make wrong, and does it own them" — plus: take the
machine's enumerated list verbatim, never re-derive a narrower one.

**Author against the observed tree, never a predicted one.** F42. Only the
current stage is authored in full; each next stage is authored at the
boundary, with the edge census run at the same sitting. The ε2 census counted
the real driver — 7,492 lines, 17 actions, 84 tests — before a single task
was written. This is the house's native staged-authoring discipline, and it
is what a spec's "Stages" section must encode: build order, not calendar.

**No mid-run human gates.** The lineage consumed spec-kit once already and
kept exactly one negative lesson: "phase done, awaiting operator" is the
single worst state an unattended campaign can enter. Human acts belong at
authoring boundaries — ratification, stage derivation — never inside a run.

**Transcription acts as the ceremony metric.** The sharpest concept of the
extension: any operator act whose text the system had already printed. The
machine computes the conclusion, then instructs the human to type it back
(`campaign.rs` literally prints "run tally campaign resume to unblock").
The spec layer must be judged by the same metric — D58, the placement law:
every mechanism must delete operator rules; anything that asks the operator
to tend a new artifact is forbidden. The spec layer passes only if it
retires the day-doc sprawl it replaces.

## The failure mode a spec layer must answer

August 6 named the standing defect: the project had converged four times and
never shipped, because the board was refilled by its own audits and the only
act that ends a cycle — release — was reserved to a human who was never
prompted to perform it. August 7 found the answer: **use generates the board
a project can actually finish** — the first wave whose top issues came from
real missions, not review, was the first that closed. A spec's Destination
section (measurable close conditions, epsilon-extension style) is the
formalization: a campaign is closed by its release receipt, never by
consensus that the work feels done.

## The evidence culture is the citation infrastructure

F1–F44 numbered continuously across five documents "so they stay linkable";
VD/PA/CA/EQ as typed excavation ledgers; the decision register separating
the operator's verbatim words from ratified proposals. The house already
writes evidence the way a spec format needs it — the only missing piece is
committing the ledgers so the IDs resolve. Requirement IDs (the Kiro
borrowing) slot into this culture as one more typed series, not a foreign
convention.

## What this means for the exercise

The spec layer is not an adoption, it is a completion. Every element the
frameworks offer either already exists in stronger form (tasks.md → the
worklist, machine-graded and sha-keyed), exists in decayed form (the
constitution → tombstones and D-rulings scattered across day docs), or
exists as practice without a surface (requirements → the goal-field
compression; evidence → scratchpad ledgers). The construction task is to
give existing practice committed, consumed, citable form — and the lineage's
own anti-rot warning applies to the new layer with full force: an artifact
nothing consumes will decay exactly the way the day docs did.
