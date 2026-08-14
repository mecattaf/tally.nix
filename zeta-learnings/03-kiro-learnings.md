# Learnings III — Kiro, the better documents

*What AWS's Kiro contributes to the house format. Framing caveat first:
Kiro is a proprietary IDE whose `.kiro/` layout is owned by its binary — it
is not an adoptable substrate. Its value is borrowable document conventions,
and they are the best in class. Sourced from kiro.dev's docs, the leaked
spec-agent system prompt, and a worked example corpus.*

## EARS: acceptance criteria as a grammar

Kiro stores requirements as numbered sections — `### Requirement N`, a user
story, then numbered acceptance criteria — which manufactures stable `N.M`
identifiers. Each criterion is an EARS sentence: an antecedent guard and a
required response.

- `WHEN [event] THEN the system SHALL [response]` — event-triggered
- `WHILE [state] THE SYSTEM SHALL [behavior]` — state-dependent
- `WHERE [configuration] THE SYSTEM SHALL [behavior]` — feature-scoped
- `IF [precondition] THEN the system SHALL [response]` — with one explicit
  style rule: **error and unwanted-path conditions always use IF–THEN**
- bare `THE SYSTEM SHALL` — ubiquitous behavior

The formal model is simply `antecedent ⇒ consequent`, which is why the house
can compile it: an EARS criterion is one deterministic step from a tally
acceptance argv — the guard becomes the test setup, the SHALL clause becomes
the assertion. For tally this is not a style preference; it is the missing
intermediate representation between a ratified ruling ("E3: escalation latch
via epoch keying") and an executable check.

## Traceability: the highest value-per-byte convention

Every Kiro task ends with `_Requirements: 2.1, 3.1, 3.4_` — a pointer at
specific acceptance criteria, not at a story or a phase. This is what turns
coverage from a guess into a computation: which criteria have no task, which
tasks discharge nothing. The house already half-does this — epsilon-
extension's task table carries a "closes" column pointing at evidence IDs
(CA-3, VD-5, F33). The Kiro convention completes it into a two-directional
join: task ↔ requirement discharged ↔ evidence closed. Direction matters for
tally: the spec points at task IDs; the worklist schema (which refuses
unknown keys) does not change.

## SHALL CONTINUE TO: the brownfield jewel

Kiro's bugfix format has three moods — current behavior, expected behavior,
and **unchanged behavior**: `WHEN [condition] THEN the system SHALL CONTINUE
TO [existing behavior]`. Nothing in spec-kit captures this, and for a
brownfield repo it matters more than any other convention on this page: in
an existing codebase the dominant risk is not "did we build it" but "what
did we break." It is also precisely the clause the house ownership law and
edge census have been chasing empirically — "which existing assertions does
this change make false" is the census question; an Unchanged Behavior
section is its declared answer, written down before the census instead of
discovered by it. Every house spec should carry one.

## Analyze Requirements: a defect taxonomy worth stealing whole

Kiro's analysis pass detects four classes of requirement bug, each crisply
defined: **wrong level of detail** (too abstract to test, or so prescriptive
it encodes implementation); **ambiguity** (one sentence, two plausible
readings, two developers would build different things); **inconsistency**
(two requirements, each sensible alone, that cannot both hold);
**incompleteness** (behavior specified for part of the input space, the rest
left to undisclosed decisions). Plus three per-criterion checks: EARS
pattern misuse, vague qualifiers, implementation-level language. This
taxonomy costs nothing to adopt and immediately sharpens any analyze pass —
though on the agency evidence it is necessary, not sufficient: none of these
classes catches a contract-vs-contract defect. Prose analysis and contract
linting are different gates.

## Steering: doctrine with inclusion modes

Kiro's constitution-analogue is `.kiro/steering/` — `product.md`, `tech.md`,
`structure.md` loaded always, plus custom files with YAML-fronted inclusion
modes: `always`, `fileMatch` (glob-scoped), `manual` (invoked by name),
`auto` (description-matched). Steering files can embed live workspace files
by reference — `#[[file:api/openapi.yaml]]` — so doctrine points at the real
artifact instead of restating a copy that drifts. Two house translations:
scoped doctrine maps naturally onto the skills surface (a skill is a
manually-included steering file with a description-matched trigger), and the
live-reference idea is worth imitating in prose — the constitution should
cite `flake.nix` and the worklist schema, never restate values from them.

## Property-based testing from EARS

Kiro derives properties directly from criteria — "authenticated users can
view active listings" becomes a universally quantified property, generated
inputs, shrinking on failure. This maps cleanly onto Rust: `proptest` is
idiomatic, and EARS criteria convert almost mechanically for anything
touching parsing, state machines, or config resolution — which describes a
lot of tally's core. Worth a standing line in authoring doctrine rather than
a mechanism: when a criterion quantifies over inputs, its acceptance argv
should be a property test, not an example.

## What not to port

The approval gates (interactive modals after each phase — the house rule is
gates at authoring boundaries only, and the artifacts here are files, not
chat sessions); Quick Spec (the house equivalent is simply a small spec);
the workflow-choice-is-irreversible rule (an IDE constraint, not a
methodology); and the `.kiro/` layout itself. One Kiro process fact worth
keeping in mind rather than porting: its docs recommend running analysis
"particularly after auto-generated requirements" — an honest admission that
generated specs are where the defect classes concentrate. The house
generates specs too; the same humility applies.

## Summary judgment

Spec-kit is the better substrate; Kiro has the better documents. The house
take from Kiro is exactly four conventions — EARS with stable N.M IDs,
task-to-criterion traceability, SHALL CONTINUE TO sections, and the analyze
taxonomy — plus one design idea (inclusion-scoped doctrine with live file
references). All four conventions are format-level: they cost nothing
mechanically, they slot into documents tally already writes, and each one
lands on a wound the lineage has already paid for.
