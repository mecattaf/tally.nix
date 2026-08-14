# The full learnings set — a house spec layer for tally, aimed at agency

*Synthesis of the five threads: the tally lineage, spec-kit's anatomy,
Kiro's conventions, the grind protocol, and the SSSF/agency recovery.
2026-08-14. This is the design argument; nothing here is ratified.*

## The thesis in one paragraph

Tally is a spec-driven system missing its top half. The worklist is a
stronger tasks artifact than anything the frameworks offer — machine-
admitted, sha-keyed, epoch-bearing, graded by gates — but everything above
it lives in decaying prose: day-numbered run records, scratchpad ledgers, a
ratified program document, and worklist `goal` fields doing triple duty as
requirements, design rationale, and evidence citations. Meanwhile agency
already has the opposite problem: twenty frozen spec-kit domains with
byte-level contracts and *no* downstream cadence, explicitly waiting for
tally to be released and documented. The house spec layer is the bridge
between these two halves: **spec → worklist → receipts → release, one
lineage, every link committed bytes.** Epsilon-extension proves the format
by recasting; agency is what it is for; sodimo is the proving load in
between.

## What each thread contributes

**The lineage** contributes the foundation and the veto. Foundation:
doctrine beats code measurably (PA-34: authority corrections 41% → 11% from
census-authoring alone), and doctrine survives only as committed, consumed
bytes. Veto: the placement law — every mechanism must delete operator
rules, so the spec layer is legitimate only if it retires the day-doc
sprawl and scratchpad ledgers it replaces; and no human gates mid-run,
ever — ratification and stage authoring are the only operator acts, both at
boundaries. Its authoring laws (ownership, observed-tree, worker-context,
executable acceptance) transfer into spec doctrine unchanged.

**Spec-kit** contributes the substrate pattern and one loop. Take: the
directory-per-unit shape, the constitution as *wired* authority (checked at
every derivation, not merely present), converge's append-only
assess-against-spec stance, and the minimal-install discipline. Reject: its
tasks template (the worklist wins), its numbering, mid-flow gates, and
prose-only analyze — which agency's D13 pilot caught reporting zero
findings while three contract-vs-contract defects waited.

**Kiro** contributes exactly four document conventions, each landing on a
wound the lineage already paid for: EARS criteria with stable N.M IDs (the
missing intermediate representation between a ruling and an argv);
task-to-criterion traceability (completing the house "closes" column into a
two-directional join); **SHALL CONTINUE TO** unchanged-behavior sections
(the edge census's contract, declared before the census instead of
discovered by it); and the four-class analyze taxonomy plus per-criterion
checks. Plus one design idea: inclusion-scoped doctrine with live file
references — cite the artifact, never restate it.

**The grind** contributes the verification protocol and the anti-rot law.
Protocol: for specs of consequence, implementation and acceptance derive
blind from the spec as single intent source, converge by collision,
disagreements escalate as spec defects — never absorbed; the bar is shown
to bite before it is trusted; failure evidence routes to workers as
concrete evidence only, never the reasoning it must independently satisfy.
Limits as rules: shared inputs are the method's blind spot; unlit territory
pays out serially — budget the tail. Law: **a bar without a gate is not a
bar** — every artifact of the spec layer names its standing consumer and
joins a gate, or it is deleted. The grind's own aftermath is the proof: its
commitments died as prose in an untracked file, and its bar rotted silently
for five days into a 61-minute gate cycle.

**SSSF/agency** contributes the destination and the hard requirements. The
D13 friction log, written as "the manual leg of the tally harness," is the
requirements document: byte oracle or nothing (the machine-gateable /
human-attended split is declared in the spec, not discovered mid-run); the
contract linter as a gate (resolvability, fixture producibility,
cross-contract agreement); pre-digested contracts in task bodies (the
two-budget model — D68 meeting its scaling test); the perturbation probe
(prove the gate can fail); record-don't-fix as law. Governance doctrines
adopted from agency's constitution: no later, no wall-clock, absence over
prohibition. And SSSF's thesis frames the payoff: tally is the shared
back-end; spec → worklist → receipts is the lineage a unified front-end
renders, and the requirement a lane discharges is the "why" a human
watching the factory wants.

## The format, as it stands in draft

One directory per campaign identity — `specs/<identity>/`, identity-named
because the identity is the join key to the worklist. One required
artifact, `spec.md`: status block (with named standing consumers),
Destination (measurable close conditions), Rulings (self-contained; the
predecessor surface freezes on ratification, as SILENT-FACTORY-PLAN froze
at E7), Requirements (EARS, N.M IDs, `[HUMAN-ATTENDED]` marking for
criteria without an executable oracle), Unchanged Behavior (SHALL CONTINUE
TO), Stages (build order only; only the current stage authored in full,
per F42), Traceability (task ↔ requirements discharged ↔ evidence closed —
the spec points at tasks; **the worklist schema does not change**).
Optional: `evidence/` (committed ledgers so citations resolve at the
authority revision) and `contracts/` (byte oracles, with the contract lint
joining the gates where they exist).

Lifecycle: proposed → ratified (operator act; predecessor freezes; spec
joins the R3 deny-list — the machinery never writes it) → staged
derivation (edge census + worklist authoring, one sitting per boundary) →
closed by the campaign's release receipt, never by hand.

The constitution merges three legal traditions into one committed surface:
house law (authority-is-bytes, gates-are-the-merge-criterion,
campaign-is-state, placement, ownership, observed-tree, boundaries-only,
frozen-flow/stale-pin, judge-adversarial-by-position, disarm-is-terminal),
grind law (standing-consumer-or-delete, dual derivation, record-don't-fix),
and agency law (byte-oracle-or-nothing, no-later, no-wall-clock,
absence-over-prohibition). Each article cites the ruling or finding that
paid for it; the citation is the argument.

## What stays deliberately out

No new machinery is proposed inside tally for the spec layer itself: the
machinery's authority remains the worklist alone, and specs are consumed by
humans and authoring agents at sittings. Post-ext1, "commit a new worklist"
is already the machine trigger via poll re-admission, so the human act
moves up to spec altitude with zero new ceremony. Whether the spec sha ever
joins the receipt stamps (beside armSerial and worklistSha256) is an
ext-era decision, not a format requirement. No spec-kit CLI, no `.specify/`
in tally, no slash-command surface beyond possibly one authoring skill —
and that skill's creation should respect ext0's `authoring-doctrine-skills`
task, which owns the doctrine-into-skills move.

## Open decisions — all operator's

1. **Ratify the format?** The drafts exist (`specs/README.md`,
   `specs/constitution.md`, both marked proposed); the epsilon-extension
   recast and the authoring skill are unwritten, stopped for this
   conversation.
2. **Supersession.** Does `specs/epsilon-extension/spec.md`, once written
   and ratified, freeze `EPSILON-EXTENSION.md` the way E7 froze the
   silent-factory plan? Two live planning surfaces would be a G2 defect at
   the meta level.
3. **The ledgers.** The five Aug-14 excavation documents live in a session
   scratchpad. Citations resolve only if they are committed —
   `specs/epsilon-extension/evidence/` is the natural home, and E6 is the
   natural moment.
4. **The dialect question.** Agency's corpus is spec-kit v0.12.18 layout;
   the house format is deliberately different. Either the house dialect
   states how it reads agency's layout (a derivation sitting can consume
   spec-kit spec.md + contracts directly), or agency migrates when it
   thaws. Undeclared, this is a twice-implemented contract waiting.
5. **Timing.** The spec layer touches nothing ext0 grades, but
   `authoring-doctrine-skills` will rewrite the two skills the spec layer
   cites. Landing the format before or after ext0 changes which document
   teaches the other.

## The one-sentence version

Give the practice tally already has a committed surface, borrow only the
four Kiro conventions and the spec-kit shapes that survived contact with
agency's real corpus, verify specs the way the grind proved works, subject
every new artifact to the anti-rot law — and aim the whole thing at the
twenty frozen domains waiting behind a released tally.
