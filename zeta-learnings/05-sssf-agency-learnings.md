# Learnings V — SSSF and agency: the surface and the destination

*Two threads recovered from the notes tree and ~/agency. One correction, one
confirmation, and the reframing that changes what the tally spec layer is
for.*

## SSSF, corrected and mined

Correction first: supersimplesoftwarefactory is not a house project. It is
IndyDevDan's MIT-licensed Claude Code skill, downloaded Aug 4 and mined as
reference material — twice. What the memory got right is why it mattered:
the four-line note in `tom-notes.md` is the thesis, and its last two lines
are load-bearing — "the one thing it does wrong is that you have to
'install' it onto a new github repo … as i am writing tally.nix this would
be just the 'back-end' for a more complete unified front-end for tracking
and measuring the ai agent factory." SSSF's UI is the right shape; its
per-repo stamped install is the mistake; tally is the shared back-end.

The mining produced real obligations: the Aug-4 ruling that tally's store be
a **strict superset of SSSF's and Anthropic managed-agents' data contracts**
(eleven issues filed under "yes to all"; the audit found tally already a
superset everywhere except per-attempt token/cost breakdown and context
*occupancy* — window fullness as distinct from spend, independently
nullable). Design lines worth keeping verbatim: "agent proposes, code
disposes"; success must be earned (`status DEFAULT 'fail'` — the column
default is the doctrine); gates verify claims, never predictions;
correction, not restart ("a cold restart throws away everything the agent
learned; a correction costs one message"); one data path, no push transport.
And the one mechanism explicitly marked to steal: the diff-snapshot
permission model — "permission is verified the way every other claim in this
system is — after the fact, against the repo itself" — which is
retrospective certification, E2's exact shape, found independently in a
third-party codebase.

For the spec layer, SSSF contributes the *rendering* claim: spec → worklist
→ receipts is a lineage a unified front-end can draw — swimlanes per lane,
phases pre-declared (the worklist DAG) rendering as queued before they run,
evidence one click deep. The spec layer is also the front-end's data source
for "why": the requirement a lane discharges is what a human watching the
factory actually wants to know.

## Agency: you already ran spec-kit at scale

The reframing fact: `~/agency/spec` is a spec-kit repository — twenty frozen
domain specs (~15k lines), twenty contracts directories, an 823-line
constitution with ten principles, four quality gates, two human-gated
autonomy exclusions, and 37 binding rulings, one of which (R37) pins
byte-level wire formats down to HKDF label strings and 104 exact key
bindings. And it is *deliberately half-built*: zero plan.md, zero tasks.md,
zero implementation cadence across all twenty domains. The corpus froze at
"design-source-freeze-2026-07-19" and the README says the specs enter the
implementation cadence at clarification. Two honesty flags from the ground:
the contract linter the README calls a committed freeze gate is untracked,
and FRONT-02 records 17 linter findings across 12 domains still open — the
corpus is red, not sealed.

The dependency chain is stated in FRONT-12, not inferred: sodimo v1 is
tally's live large-scale test; the docs chapter and a real release follow;
"Agency stays frozen at the 23 July repair-wave state until the tally docs
give an agent something authoritative to wrap tally functionality against."
Tally is the substrate; sodimo is the proving load; agency is the workload
tally is being built to be capable of running.

## The D13 pilot is the requirements document

The pilot ran one domain (theming) through the downstream cadence by hand,
explicitly framed as "the manual leg of the tally harness — what
Claude-steering-Codex-by-hand teaches about what the eventual workflow must
automate." Its friction log converts directly into requirements on the
spec-to-campaign bridge:

- **Byte oracle or nothing.** "Oracle presence, not effort, decided what
  could complete." The mint predicate for a machine-gateable task is: does a
  byte fixture exist? If yes, campaign territory; if no, human-attended by
  declaration. This cleanly splits agency's corpus into what tally can run
  unattended and what it cannot — and demands the split be *marked in the
  spec*, not discovered mid-run.
- **Precision demotes the donor.** Where the contract was byte-crisp, the
  reference implementation was wrong against it and was rebuilt fresh
  ("LIFT: none"). The reference-corpus call is only as good as the
  contract's precision.
- **The missing gate is a contract linter, not a better prompt.** Spec-kit's
  analyze reported zero findings; implementation then hit three
  contract-vs-contract defects. Every reference must resolve, every fixture
  must be producible from the stated rules, every cross-contract bound must
  agree. Called "the single most valuable thing this pilot surfaced."
- **Pre-digest contracts into the task body** — the two-budget model: a
  small reasoning budget for real work, a large transcription budget for
  contract soak. Don't make every lane re-read 300 KB of schema; this is
  D68 (goal + readFirst is the whole context) meeting its scaling test.
- **Don't trust the converge report.** Re-run the suite independently,
  recompute byte hashes against the frozen fixture, and run a perturbation
  probe proving the gate is non-vacuous. Tally's version: gates already are
  the merge criterion; the perturbation-probe idea (prove the gate can
  fail) is new and worth adopting.
- **Record, don't fix.** Codex could have edited the frozen schema to
  unblock itself; it recorded deviations and proceeded with the producible
  slice — additions only, zero modifications. This must be a law, because
  under automation the temptation becomes a failure class.

Plus three governance doctrines the agency constitution states better than
the house ever wrote down: **no later** ("the spec freezes ONE scope; build
ORDER exists, softer scope does not"), **no wall-clock** (no dated
milestones anywhere in implementation artifacts; progress altitude is
checkboxes, evidence, and append-only reports), and **absence over
prohibition** (an excluded surface receives no positive artifact of any
kind). And one named drift class: the **synced triad** — when a contract
lives in a type, an example, and a call-site declaration, "change one,
change all three in the same edit" — which a spec system should mechanically
enforce, not remember.

## What this means for the tally spec layer

The exercise is not "give tally a spec format." It is **build the bridge
that lets a frozen spec corpus become campaigns** — the missing downstream
half of agency's cadence, mechanized. Scale check, honestly: D13, one of
the smaller domains, cost ~585k tokens for 1,545 lines by hand; twenty
domains at agency's byte-precision is campaign-ladder territory, which is
exactly the machine tally already is. Epsilon-extension is the right first
instance because it is small, already ratified, and already has its
worklist — the format is proven by recasting, then pointed at the thing it
was actually built for.
