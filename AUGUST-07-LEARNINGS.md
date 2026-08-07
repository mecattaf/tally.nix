# August 7 learnings — the first real-world wave, and what an afternoon of it costs

Companion to `JULY31-LEARNINGS.md`, `AUGUST-01-DESIGN.md`, `AUGUST-02-LEARNINGS.md`
and `AUGUST-06-LEARNINGS.md`. August 6 named the project's standing defect:
four convergences without a release, a board refilled by its own audits. This file
records the day that partially answered it — the first wave whose top issues came
from *use*, not from review — and two operational lessons that cost real money to
learn: what an agentic worker fleet does to a metered token plan, and what a
provider dying mid-wave does to a dispatch. Written by the 2026-08-07 orchestrator
session while the wave's last lane was still in its repair loop. Nothing here
overrides an issue's text.

## 1. The day in numbers

One wave (phase 3 of the #410 consolidation, run dir `2026-08-07-p3`), four lanes
over 13 member issues plus the #430 umbrella. Dispatched twice: once on pi/qwen
workers (killed by quota, §3), once on claude workers (d1 on the deep-lane model,
the rest one tier down, all effort high). Loop totals at time of writing: f1
merged after 2 eval rounds / 1 repair / 2 gates; s1 cleared after 3 evals / 2
repairs / 4 gates; d1 cleared after 2 evals / 1 repair / 2 gates; a1 in repair
after 1 eval that found the wave's only HIGH. Issues filed: #432/#433/#434
(atomic children of #430, per its own instruction) and #439 (a pre-existing
shipped defect surfaced by an eval, §6). Board path: 19 at dawn → 23 after
filing → 15 mid-wave — and the arithmetic of the remainder is in §7.

Every eval round in the wave found something real, and nothing found by an eval
shipped. Seventh consecutive wave for that property.

## 2. The headline: real-world feedback breaks the convergence loop

August 6 §2's diagnosis was that the board converges and refills because every
issue is *authored* — an audit with a budget always finds something true to say,
so "board empty" is a state the process cannot hold. Today's top of the board was
different in kind: #429, #430 and #431 came out of the crm-call-drain campaign —
a real operator, driving a real ad-hoc campaign, on the deployed pin, who could
not get tally to dispatch a single agent. The mechanism failed a *mission*, and
the mission wrote the issues.

The difference showed everywhere downstream:

- The issues carried **operator pain, measurements, and byte-exact proofs**
  (#429 arrived with the one-line fix already verified against the live digest;
  #431 arrived with an afternoon of stall timings and its own ruled-out
  hypothesis). Lanes built on them needed almost no investigation phase.
- They carried a **definition of done that lives outside the board**: the wave is
  finished when dotfiles#163 can re-arm and dispatch, not when a reviewer runs
  out of findings. That is the artifact-shaped stopping criterion §2 of the
  August 6 file asked for, arrived at not by cutting a release but by having a
  user.
- The umbrella (#430) fanned out cleanly into atomic children because the
  operator's ledger already separated mechanism defects from operator error —
  the seven-rule operator ledger became a skill draft
  (`skills/campaign-operator/`), not seven more issues.

The lesson is not that audits were wasted — today's evals caught a HIGH that use
had not yet reached (§6). It is that **use generates the board a project can
actually finish**. The fastest route to that state is the one taken today by
accident: point the mechanism at a real job and let it fail honestly.

## 3. The qwen lesson: an agentic fleet is a different usage regime, with numbers

The wave was first dispatched on pi workers (qwen3.8-max, the metered weekly
token plan) per operator preference. **The plan's entire weekly allowance —
1,373,257 fresh input tokens — was consumed in roughly eighteen minutes**, and
all four workers died on `429 insufficient_quota` before pushing anything. The
per-session accounting, reconstructed from pi's own usage records:

| lane | model turns | fresh input | cache reads | output |
|---|---:|---:|---:|---:|
| d1 | 174 | 389,325 | 33.0M | 119,270 |
| s1 | 245 | 365,440 | 32.8M | 143,258 |
| f1 | 171 | 279,127 | 20.9M | 92,718 |
| a1 | 171 | 324,014 | 25.7M | 101,995 |
| harness smoke | 5 | 5,697 | 16.8k | 529 |
| **total** | **766** | **1,363,603** | **112.4M** | **457,770** |

Nothing looped and nothing misbehaved — s1's 245 turns produced three real
commits with tests. The arithmetic is just what autonomous implementation *is*:
every tool call is a model turn; every turn's tool output (an issue body at 3–6k
tokens, a source file, a cargo run) lands as fresh input; the re-sent transcript
rides the cache (112M cache reads — unmetered) while the **fresh side is what
the plan meters**. Four workers in parallel, each contractually required to read
its member issues, the design doctrine and the contributing guide before writing
code, is ~30–50k of ingestion per worker before the first edit.

Rules of thumb now on the record:

- **A lane-sized autonomous worker costs 300–400k fresh input.** The weekly plan
  therefore funds about three lane workers per week — total, not per day.
- **Interactive intuition does not transfer.** An interactive session is tens of
  turns with small outputs; a worker is hundreds of turns with bulk ingestion.
  The operator had never approached the cap because the regime, not the volume
  of use, changed.
- **If pi/qwen is to be a standing worker pool**: one worker at a time, small
  lanes (1–2 issues), and keep the read-everything roles — evaluators above all,
  which adversarially re-read the whole surface every round — off the metered
  plan.

## 4. Provider death mid-wave, and the suspect-draft protocol

The quota killed four workers mid-implementation. Two had committed real work
(s1: three members; f1: all four); two left only uncommitted edits. The recovery
protocol, improvised today and worth keeping: reset the uncommitted lanes clean;
for the committed ones, the successor worker inherits the branch with an explicit
clause — **suspect draft, not baseline; verify every claim by running commands;
anything kept is owned; the evaluator does not grade on a curve.**

It worked better than either discard-everything or trust-everything would have.
f1's successor kept the draft's four mechanisms only after re-deriving them,
and in the keeping found a real hole in one (the draft's table-separator check
accepted a horizontal rule — a shape that renders as literal pipes, i.e. the
draft would have blessed the defect its member exists to reject). s1's successor
kept two members, then *replaced* the draft's configuration seam after proving
the env-var route it used is structurally unreachable from a daemon-dispatched
campaign pass — the draft's documented knob would have been a false sentence on
the exact surface #432 exists for. Keep-means-own held: both successors' reports
attributed what was kept, fixed, and rewritten, per member.

## 5. The round-1 signature of this wave: correct mechanism, unbound wiring

Every lane's round-1 eval converged on one defect class, distinct from the
August 6 waves' false-sentence class: **the mechanism is right and nothing pins
the wiring that reaches it.** The mutation that found it, three lanes out of
four, was wholesale deletion: delete s1's failed-agent treeDelta block from the
flow — workspace green; delete s1's registration→argv push — green; empty d1's
trace-decoration index and delete its other call site — green twice. Each was
implemented correctly and each could regress silently, which for a safety gate
(#424) or an operator remedy (#432) is precisely the "advertised, provably
inert" pattern #430 documented from production.

Adopted for future evals as a standing clause alongside the mutation ladder:
**for every new wiring — a call site, an argv push, a dispatch edge — run the
deletion mutation, not just the corruption mutations.** Corruptions test the
mechanism; deletion tests that anything binds the mechanism to the path that
reaches it.

The class's aristocrat was a1's F1, which graduated from unbound to untrue: a
durable diagnostic view claiming "read-only, never creates, locks, or repairs"
on four surfaces, whose membership read went through a helper named `preflight`
that creates the directory tree and opens the ledger for append — in the data
dir of the possibly-live daemon it exists to diagnose, reachable exactly during
the #431 stall window the feature was built for. The fix is one call site (the
pure read sat unused one function above), but the lesson generalises: **a
no-write claim requires a no-write test** — assert the diagnostic creates
nothing under the tree it touched — because reading the code's intent is how
four surfaces came to state the opposite of its behaviour.

## 6. Evals against the wave's own author, twice

Two moments today validated the never-self-certify rule beyond its usual scope.
s1's repairer, asked to bind a ruling, discovered the ruled state
(`treeDelta:ungated`) is unreachable through the flow, corrected its own three
claim surfaces — and the round-2 eval then found the *adjacent sentence in the
same paragraph* made the same unreachability error in the opposite direction,
promising an owned-paths fallback the flow can never take. Pulling that thread
surfaced #439: a pre-existing, shipped behaviour defect (serial campaigns that
omit `conflictDomains` breach on their own certified work; the #386 fallback is
dead code through the flow) that no audit had found because no audit had asked
what the schema makes reachable. The evaluator attributed it correctly to main,
not to the lane — blame hygiene that kept the lane's loop bounded while the
defect got its own issue.

And d1's repairer, fixing its own storm-test flake, changed the acceptance
instrument itself — the correct fix, but the round-2 eval independently re-proved
the binding (deafness now caught by the acceptance bound, not a precondition)
and analysed what the weakened floor structurally can no longer see before
clearing it. An acceptance test edited by the lane it accepts is a seam commit
in the August 6 §5 sense: nobody else has read it until an eval does.

## 7. What the wave buys, and the arithmetic of the remainder

For the crm mission specifically, once merged and pin-advanced: reconcile digest
parity (#429, with a CI test that already caught a second skew in `repository`
defaults), stall-survivable campaign passes (#431 fixed at the mechanism, #432
as the flow-side classification plus an armable `--projection-wait-ms`), a
digest receipt that names its evidence (#433), three-valued smoke verdicts and a
durable `query run` view (#434), and an enforced pi resume invariant (#425).
The re-arm of dotfiles#163 remains one command, per #430; the workaround driver
in `~/.local/share/tally/drivers-diagfix/` is deleted on the pin advance, per
the same.

Board arithmetic at wave end: the phase-A merges close 13 (#414 #416 #418 #427 /
#429 #432 #433 #424 / #431 #428 #420 / #425 #434), #430 closes with the wave's
close-out, #419 stays open **by design** (population bounded, not closed — the
day's honest numbers: 1 failure in 605 pooled post-fix runs at three-plus
concurrent suites, ~0.17%, under the historical 0.74% but not zero). That leaves
nine: the five rollup-lane members (#402 #403 #408 #409 #415, probe-gated to
Aug 8), the #410 umbrella that closes when they do, #419, #426
(deferred-until-consumer by its own text), and #439 (needs a design ruling).
The endgame after the rollup lane is a board of three or four, each open for a
stated reason rather than by neglect — which is as close to "done as an
artifact" as this project has yet been.

## 8. What to carry forward

1. **Use generates the finishable board.** Prioritise running tally at real
   missions over auditing it; file what the mission breaks.
2. **Deletion mutations are a standing eval clause.** Corruption tests the
   mechanism; deletion tests the wiring. Three lanes' round-1 HIGHs/MEDIUMs were
   deletion-survivors.
3. **A no-write claim needs a no-write test.** Assert "creates nothing", never
   read it off the code.
4. **Budget agentic workers on metered plans**: 300–400k fresh input per lane
   worker; parallel dispatch multiplies; evaluators are the most read-hungry
   role and belong off metered plans.
5. **The suspect-draft protocol is the provider-death recovery**: inherit
   commits explicitly as unverified, verify-or-rewrite, keep-means-own.
6. **An acceptance test edited by its own lane is a seam commit** — independent
   re-proof before merge, every time.
7. **Blame hygiene in evals**: a defect found underneath a lane's diff but
   present on main gets its own issue attributed to main, keeping the lane's
   loop bounded (#439 is the template).
