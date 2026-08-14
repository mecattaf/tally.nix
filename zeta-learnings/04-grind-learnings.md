# Learnings IV — The grind: the mechanism worked, the artifact rotted

*The Aug 8–9 adversarial dual-derivation episode, reconstructed from the
three prompt files, the grind ledger, and the later record. This is the
house's native verification protocol for specs of consequence — and its
aftermath is the sharpest anti-rot lesson in the lineage.*

## The protocol, compressed

AUGUST-08-HYPOTHESES diagnosed the whack-a-mole board down to two
generators: **G1, serial discovery** (no check ever walked past reconcile,
so discovery rate was pinned to fix rate — one mole per whack, by
construction) and **G2, twice-implemented contracts** (each session held one
side of a contract perfectly; nothing forced the sides to agree). The grind
was the experiment against both, run in one day.

Two mutually blind sessions derived from the same intent source — the same
18-issue board, the same standing diagnosis, the same two design questions
each side had to **decide with rationale, never punt**. The code side drove
the repo to its desired state as one atomic commit per issue, forbidden from
authoring its own acceptance. The bar side wrote the definitive suite "as if
tally.nix already conforms," under the one rule that defines the mission:
**expected values come from intended behavior, never observed behavior** —
asserting current output back "would re-import the very defects this suite
exists to catch. Where current behavior and intended behavior differ, the
test asserts intended behavior, and its failure against current HEAD is
expected and is the point."

Then the grind proper: iterate the code branch until the full bar passes.
Bar frozen, read-only. Failures routed to the owning thread carrying
**concrete failing evidence only** — case name, command, expected vs actual;
never the suite source or rationale, so the worker satisfies the contract
independently instead of reverse-engineering the assertion. Fixes amend the
owning commit; bisectability survives every iteration or the iteration
isn't done. And the tie rule: if an assertion encodes a wrong *decision*
rather than catching a code *defect*, stop that group and escalate — never
work around it, never weaken the code to mimic it, never touch the test.

## What the run proved

Seven bar runs to convergence (8/18 failing at baseline → 26/0). Zero
disagreement escalations reached the operator — the pre-ruling that pure
lexicon divergence is not a tie (the bar's public vocabulary wins by
contract) disposed of the only free-choice divergence in one ledger line.

The catch that justifies the whole method: the code side, executing the plan
*designed to kill contract skew*, re-created a skew — the packaged driver
rejecting the arm's own canonical brief. Only independently derived
acceptance could see it; the ledger calls it "the whack-a-mole generator
caught in the act." Beyond that: four latent defects in territory nothing
had ever walked, one real query bug, and a dozen token-level vocabulary
alignments — two blind derivations agreeing on substance, diverging on
lexicon. The next day's missions confirmed the deeper claim: every failure
was operational, none was contract-skew. The word "skew" disappears from
the learnings files after Aug 10. G2 died.

## The three measured limits

These are authoring rules now, not caveats. **Anything both sides inherit
is invisible to the method** — the shared standing diagnosis was itself
partly wrong (the skew class was 3 issues, not 5), and neither side could
see it because both were told not to relitigate it; the correction came
from ordinary use a day later. Minimize shared inputs; run shared empirical
probes independently on each side (both sides ran the rehydration probe 31
seconds apart and converged on the verdict from different numbers — that
agreement is evidence; a shared artifact is not). **Batch discovery is
bounded by reachability** — the full-pipeline case paid out its debt one
newly-reachable defect per run for four straight runs, G1's shape contained
to one case but not eliminated; what ended it was a tactic change, auditing
the whole path against the known-good grammar at once. Budget for the tail.
**Worker-local validation passed while the bar failed, three times**, all on
exactly-shaped public-boundary details. The stated lesson: evidence must
pin the consuming expression, not the intent.

## The rot, priced

The §4 commitment list had three items; follow-through failed on all three.
"The bar joins the permanent gate" — it did not: to this day the bar's only
flake attribute runs `--list` and executes zero cases, and fleet-gate has no
bar step. The consequence arrived on schedule: chapter 2's deletions broke
four bar call sites "and nothing noticed, because the bar had no gate
coverage at all"; by epsilon, 12 of 24 cases asserted pre-deletion
contracts, and the gate cycles cost 61 and 74 minutes. The ratchet rules
(corpus-first boundary changes; no fix without a case that failed before it)
were never encoded anywhere — six days later they still live only in an
untracked file. Even the branch/worktree cleanup never happened: thirteen
grind-era worktrees are still checked out.

The verdict splits cleanly. The mechanism worked — every collision was a
vocabulary alignment or a real defect, including the one class the repo was
constitutionally prone to, caught alive twice. The artifact rotted — and it
rotted *silently*, which is the same structural failure the bar was built to
kill (an unexercised contract), relocated from the product into the test
suite. **A bar without a gate is not a bar.**

## What the house format takes

1. The grind is the verify phase for specs of consequence: implementation
   plan and conformance bar derived blind from the spec as single intent
   source; converge by collision; disagreements escalate as *spec defects*.
   This is what "Analyze Requirements" looks like when it is executable.
2. The bar must be shown to bite before it is trusted — run against pre-fix
   HEAD, publish the failure matrix, chase every suspicious pass, mutation
   spot-check. A test that cannot fail is a defect in the deliverable.
3. The evidence-channel rule generalizes far beyond the grind: when routing
   failures to any worker, send the concrete evidence, never the reasoning
   it is meant to independently satisfy. This is prompt-isolation doctrine.
4. The anti-rot law, in the constitution with full force: every
   verification artifact — bar, spec, checklist, census — names its standing
   consumer and joins a gate, or it is deleted. The spec layer being
   proposed is itself subject to this: a spec nothing derives from is the
   day-doc sprawl with better formatting.
5. The follow-through failure is the argument for mechanized close-out: the
   grind's commitments died because they were prose in an untracked file.
   Commitments belong in the worklist of the next campaign — which is
   exactly where `final-bar-executes` now sits, five days late.
