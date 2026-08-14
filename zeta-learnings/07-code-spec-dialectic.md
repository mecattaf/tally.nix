# The code⇄spec dialectic

*Not a destination — the concept itself: three weeks of converged code, and
the question of what a specification that crystallizes it is. 2026-08-14.*

## The two directions have different truth-makers

Spec→code is the solved direction — it is what tally mechanizes:
conformance is decided by gates and bars, and the grind proved the strong
form of it. But code→spec — crystallization — has a different correctness
problem, and it is the hard one: separating essence from accident. Three
weeks of iteration produced a codebase where some behaviors are converged
decisions (the epoch model, receipts-as-refs, the writer's tuple) and some
are accidents of the path (whatever shape Python left behind, the narrator
seam's vestiges). A crystallized spec that captures accidents as law
fossilizes them; one that misses essentials is incomplete in exactly the
way that bites four issues later.

The grind's rule — never assert observed behavior — seems to forbid
crystallization outright. It doesn't, and the distinction is the whole
concept: **you crystallize decisions, not code.** The code is the record of
*what*; the run records are the record of *why*; the spec is the
compression of why into law. The house has been unknowingly building the
crystallization input format all along — the decision register separating
the operator's verbatim words from ratified proposals, F1–F44 numbered
continuously so they stay linkable, the typed excavation ledgers. A
behavior earns spec status when there is a ruling or finding that *paid*
for it. That is the essence/accident test, and it is why the lineage
matters so much more than the diff.

## The grind runs in reverse as the crystallization check

Once a spec is crystallized from converged code, how do you know it is the
real spec? Derive blind from it and collide with the actual code.
Divergences classify exactly two ways — the code does something essential
the spec failed to capture (spec omission), or the code does something the
spec rightly ignores (confirmed accident, now deletable). The two-Fable
final-shape/history-replay pass was already this shape.

So the back-and-forth is not a vague oscillation — it is alternating
reconcile passes, each witnessed, and **"final specification" means
quiescence**: the fixed point where another crystallization pass changes
nothing and another derivation pass changes nothing. This is also what
makes E1 the deepest task in the extension — the exact oracle is the
mechanical coupling between spec identity and code identity. When release
proofs run through the writer's tuple, the spec has an actual grip on the
bytes rather than a narrative about them.

## Agency: the same dialectic at one remove, twice over

Agency's spec was not written from nothing — it is a crystallization of
*other people's converged code*: wlroots and sway are decades of compositor
decisions, and the corpus crystallizes their semantics into byte-precision
contracts, to be re-derived inside a substrate not our own. The D13 pilot
proved the crystallization worked: "precision demotes the donor — LIFT:
none." Once the essence is captured at byte precision, the donor code
itself becomes a shape hint and an anti-reference.

Meanwhile the Igalia discipline is the *other* half of the dialectic,
applied to Chromium: the patch series **is** a specification — the spec of
divergence from upstream — and the constitution's principles (incremental
landing, rebase-as-patch-series, the modifying-delta budget,
don't-drag-//chrome-in, permanent downstream carry) are laws about keeping
that divergence-spec small, legible, and re-derivable against a moving
substrate. G1 — flag-off behaves stock at the pinned Chromium commit — is
one gigantic `SHALL CONTINUE TO` clause covering thirty million lines. And
the modifying-delta gate is literally a `git diff --shortstat` budget,
which is an argv — tally's gate model expresses it today, unchanged.

## The ideal shape

Desired state exists at three altitudes — spec (law), worklist (change),
code (bytes) — kept honest by two reconcilers and one instrument.

Downward reconciliation is the campaign: tally owns it — mechanized,
witnessed, graded. Upward reconciliation is crystallization: today it is
the manual ritual of day docs → excavation ledgers → design pass → ratified
document → printed PDF, and the open design question is how much of *that*
tally should eventually witness the same way — the ledger culture is
already its input format. The honesty instrument between them is the
bar/grind, run in whichever direction moved last. The constitution is the
fixed point — the laws that survive both directions, which is the real
test for what belongs in it.

Tally's three weeks and agency's Chromium relationship are the same problem
at different magnitudes: **a spec is always a delta-of-intent against a
substrate**, whether the substrate is your own last month or someone else's
last thirty years. What tally is two overnights from proving is the full
cycle at small scale: code converged through use, crystallized into a final
spec, with the exact oracle coupling them — which is precisely the
capability agency's twenty frozen domains are waiting on.

## Two open questions

**The atomic unit of crystallization.** Is it the ruling (E/R/D-style), the
contract (agency-style byte oracle), or the invariant (SHALL CONTINUE TO)?
Do those three unify, or are they a typed triple — law, oracle, inheritance
— each with its own truth-maker?

**Does "final" exist?** For a living system, a final spec may be exactly
*quiescent-until-next-use* — since Aug 7 taught that use, not review,
generates the real board. That would make the spec's finality precisely as
durable as the last mission that exercised it, and "ratified" the honest
maximum a living spec can claim.
