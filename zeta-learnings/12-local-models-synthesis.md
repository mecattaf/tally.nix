# Small models, clanker, and the capability axis — synthesized to tally

*Four retrievals over the notes corpus — the pi/qwen/kitty-harness lineage,
the runtime substrate, role-fit and economics, and the clanker thread —
distilled to philosophical nuggets and contextualized to tally while its
spec layer is still malleable. Per the standing steer: no models pinned;
the subject is decomposing agentic work across capability tiers.
2026-08-14.*

## The clanker correction, and the pattern it reveals

Clanker was never house code. It is another engineer's private local-LLM
fuzzing agent, known only through press coverage — and the May 21 research
session established exactly that, then did the characteristic house move:
*"In my case it won't be for kernel bugs but it's about the MECHANISM."*
What was transplanted, three times over, was the epistemics: into CADE's
commit-analyzer stream, into agency's "spec-as-benchmark plus
micro-improvements" product framing, and — the one that shipped — into the
academic-OCR drain, where a three-year, four-times-scoped, never-built
want finally became a 24/7 service the moment it acquired clanker's shape:
small local models hammering continuously on owned hardware, ensemble
disagreement as the signal, git as the audit substrate, the human as sole
signer.

Two things deserve naming. First, the method itself is the house's
crystallization doctrine applied outward: extract the converged decisions
from someone else's practice — from a *report* of it, code never seen —
and re-derive them against your own substrate. It is the same relationship
agency has to wlroots and sway. Second, the clanker philosophy is tally's
spiritual ancestry in miniature: the "agent inbox" (git + email as
intake/queue/audit, judged more practical than schedulers) walked straight
into tally's poll and the E8 inbox; the provenance posture — machine
labors, ledger records, human signs — is the receipts-and-release model
before tally existed. The lineage was already coherent before it was
designed.

## The capability nuggets, contextualized

**1. Capability is a property of the model×scaffold pair.** The measured
core of the kitty-harness SILVER track: hold a small model fixed, change
only the scaffold, and benchmark accuracy went 19% → 46%; the same
scaffold with a larger model, 79%. Two independent codebases converged on
the same six harness mechanisms — convergence marking load-bearing, not
taste. For tally: **the spec layer is a capability amplifier.** A weaker
implementer under a stronger contract beats a stronger model under a vague
one; pre-digested goals, verbatim contract blocks, and one-claim-per-line
are what widen the viable-implementer set downward. The format is not
courtesy for frontier authors — it is the decomposition mechanism itself.

**2. Decompose by token-flow shape, not difficulty.** The quota incident's
enduring lesson: a lane worker is bulk ingestion (hundreds of turns,
300–400k fresh input); a judge re-reads the whole surface every round; a
utility call is one bounded request. A slow, deep local model is fine
where thinking is long but reading and output are short, and fatal in
read-everything roles — hence the standing rule that evaluators stay off
metered capacity. The kernel-community corroboration surfaced in the
clanker chat says the same thing from the other side: task decomposition
beats long context — "each page is a task; reconciliation is itself a task
with its own context." That is D68 and the two-budget model, discovered
independently at page scale.

**3. Independence is the currency; agreement is not evidence.** The
sharpest clanker-descendant finding: both mechanical OCR engines
linearized tables the same wrong way, *"so the agreement gate can't catch
it"* — two cheap oracles sharing a failure mode agree their way past any
ensemble. This is the grind's measured limit (anything both derivations
inherit is invisible) confirmed in a second domain. The house rule it
yields: verification budgets buy *independent derivations*, never
redundant ones — and when independence can't be had, add a third signal of
a different *kind* (there, a caption-regex and a bare-numeric-line
detector; in tally, a deterministic gate beside two model opinions).

**4. Disagreement is the router for expensive compute.** The
fuzzing-epistemics transplant, stated once: don't trust any single oracle;
use disagreement between independent passes as the gradient for where to
spend the expensive tier — and when disagreement is high, *mutate the
input* (re-rasterize, crop, deskew) rather than adjudicate by fiat. Tally
already embodies the static version — the grind escalates collisions as
spec defects; the judge gates attempt two — but the dynamic version is
worth carrying into doctrine: a disagreement is first an invitation to
perturb the input, and only then a tie to break. The epoch model is
adjacent kin: new input, fresh budget, by derivation.

**5. Cheap invariants outrank expensive verifiers.** The word-count floor,
not the semantic Dice threshold, "did the real safety work"; reference
counts, cell counts, parseable LaTeX "catch ~80% of failures without
needing an LLM verifier"; the calibrated two-lane gate shortcut 78% of
pages with 0.3% residual disagreement. This is tally's cheap-fails-first
gate ordering and the oracle census generalized into a spending rule:
every criterion should be discharged at the cheapest tier that can falsify
it — structural check before model check, model check before human. And
pay ensembling only where it pays back: the corpus splits into an easy
bulk and a hard tail, and the expensive machinery is for the tail.

**6. Closed shapes are the capability equalizer.** Every small-model slot
that already works — the drain's bounded JSON, the print decision, the
schema-forced evaluator, the judge's typed verdict — emits a closed shape.
Small models are reliable on closed shapes and unreliable open-ended. The
spec layer's typed artifacts (claims, verdicts, envelopes, trace rows) are
what make every slot tier-flexible later without format changes now. The
adjacent prompt-framing lever from the kernel practitioners — calling the
task "deep dive regression analysis" rather than "review" changed
compliance — is the same point from the input side: the name of a task
binds it to the model's internal definition, so house skills should name
tasks for the behavior wanted, not the genre.

**7. Witnessed operation produces the eval corpus for free.** The Aug-1
procedure — replay the journaled corpus against a smaller candidate,
downgrade only on the numbers — generalizes: because every tally slot is
schema-forced and receipted, every role accumulates its own entrance exam
as a by-product of operation. Slots are defined by measured adequacy;
occupants are catalog facts ("which model answers is a host-catalog fact,
never worklist bytes" — the no-pinning steer, already ratified law). The
procedure has still never been run; ext2 makes it runnable, and it is the
keystone that makes every tier decision empirical forever.

**8. The downgrade ladder ends in code.** The narrator slot went 0-for-35,
the failure was later proven to be the harness's, and the slot was deleted
anyway — replaced by deterministic subject adoption. Two rules: verify the
harness before judging the tier (measurement can indict the wrong layer),
and the preference order is deterministic code over any model over a
bigger model. A role must first justify being a model slot at all.

**9. Seams are contracts; fallback is a violation.** One seam, N prompts;
consumers target a slot name; the utility tier is fail-closed — "fail
loud, never bill the paid model." A silent fallback changes the privacy
class, the cost regime, and the trust tier in one unlogged move. Slot
occupancy is a contract property. The same posture holds the other
direction: sovereignty is load-bearing where regulated data lives, and
deliberately non-doctrinaire for tooling — local and cloud providers
coexist by ruling, routed per role and per data class, not by ideology.

**10. Derived state is disposable; canonical state is the asset.** The
drain's own storage triage — embeddings "expensive to regenerate, the
valuable half"; the index "rebuildable, disposable" — is tally's
canonical-versus-derived law (the rebuild verb, the read-model lane)
appearing as an economics fact. Capacity planning, like verification,
follows the regeneration cost, not the byte count.

## What this protects in the malleable spec

Nothing new to add; four things to hold: **model-freeness** (bindings in
the catalog, certification in receipts, never a name in authority bytes);
**pre-digestion as a format duty** (it is the mechanism that decomposes
work across tiers); **closed shapes wherever a model slot exists**; and
**independence as the verification currency** — with disagreement treated
as a router for expensive effort and an invitation to perturb inputs, not
merely a tie to break. The graveyard note stands guard over all of it: the
kitty-harness verdict — twenty percent load-bearing, eighty percent
infrastructure in search of a job — is the placement law learned at full
price, and the spec layer answers to it like everything else.

Open items surfaced and left with the operator: the SILVER-track harness
mechanisms were never built; the judge-tier corpus replay has never run;
the table backfill (dotfiles #156) is written but deliberately unexecuted;
FRONT-07's bulk-resume ruling awaits; and whether metered small-model
workers return as a standing pool after the quota reset remains, as the
notes put it, an unmade decision rather than a pending action.
