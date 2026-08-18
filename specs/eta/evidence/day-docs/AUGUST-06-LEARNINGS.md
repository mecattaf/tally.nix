# August 6 learnings — convergence without shipping, and who checks the checker

Companion to `JULY31-LEARNINGS.md`, `AUGUST-01-DESIGN.md` and
`AUGUST-02-LEARNINGS.md`. August 2 recorded why the last stretch felt slower
than the middle and named the verification bill. This file records what the
*next* four days taught: that the project has now converged four times without
shipping, why the loop cannot terminate on its own terms, and what the day's
evals found about who is allowed to be wrong. Written by the 2026-08-06
day-steward session while phase 2's three lanes were in flight. Nothing here
overrides an issue's text.

## 1. The day in numbers

Eight issues closed (#388, #389, #395, #396, #400, #401, #404, #411). Board
20 → 18. Phase 1 finished with lane L7 merged as `e2bf348` after **five eval
rounds and three repair rounds** — the longest single lane in the project's
history. Phase 2 dispatched three lanes in parallel over eight issues; all three
returned real findings on their first eval.

Phase 1's measured economics, which triggered the doctrine amendment recorded in
#410: **closed 8, opened 6, net −2.** Four of the six opened were
LOW/latent/decide-class.

## 2. The finding that subsumes the rest: this project converges and does not ship

Board size, by day, since the first issue:

```
07-28    2
07-29    0     <- empty
08-01   32     (+49 opened that day)
08-02    5
08-03    2
08-06   20
```

**The board reached zero on July 29.** It reached ≤2 on three other days. In
seventeen days the project has converged four times, and there is no release.

The issues are not leaking from defects found in production — there is no
production. They are *authored*, in bursts, by audit and consolidation passes:
49 in one day, 23 in another, 22 in another. Every one is real. That is the
problem, not the excuse: an adversarial reviewer with a large budget aimed at a
maturing codebase will always find something true to say, so "the board is
empty" is not a state the process can hold.

And the loop is structurally unable to end. Every worker prompt in the harness
carries, correctly, **"Do NOT create tags, releases, or GitHub Actions
workflows."** That rule stops an agent shipping unilaterally. Its consequence is
that the only act which ends a cycle is one no agent may perform. So the
process's operative stopping criterion is "board empty" — while the same process
that empties the board is what refills it.

**The lesson is not "review less."** The reviews are load-bearing; §4 and §5 are
defects they caught that a green gate could not. The lesson is that *the
definition of done must be an artifact, not a number.* A shipped version, cut at
a declared date from whatever is green, with the remainder becoming
post-release backlog. Until that exists, every convergence is followed by
another audit wave, and the four-time pattern repeats.

## 3. The severity floor, and its first measurement

The #410 amendment of this date added a floor: a finding becomes a standalone
issue only if it is (a) wrong behaviour reachable on a shipped or deployed
surface, or (b) a weakening of a safety mechanism. Everything else — LOW, latent
config space, test-internal, doc wording, unrequested design questions — goes to
a `RESIDUE.md` in the phase run directory, promoted only on a second hit, a lane
opening on that surface, or an operator ask. Decide-class findings ("decide what
X should mean") go to the operator as a close-out decision and never to the
board; they were never implementable contracts.

Two supporting changes went into the harness scripts rather than into prose,
which is the only reason to believe they will hold: eval prompts now require a
disposition (`REPAIR-NOW` / `ISSUE` / `RESIDUE` / `DECIDE`) on every finding, and
the repair prompt authorises fixing sub-write-up, non-mechanism residues in-lane
under a ~15-line cap. Mechanism-touching fixes keep full doctrine — that rule has
caught two repair-introduced HIGHs and is not negotiable.

The eval quota line ("every eval since wave 2 has found at least one") was
**retired**. It has been replaced by: *no findings is an acceptable verdict if it
is true; a LOW that would not change an operator's decision is not worth writing;
credibility spends on reproductions, not volume.* A prompt that tells a reviewer
its predecessors always found something is a prompt that asks for something to be
found.

Under this floor, phase 1 would have filed 2 issues instead of 6. Phase 2's first
three evals filed **zero** new issues and routed their non-blocking findings to
residue lines — the first evidence the floor works.

## 4. The defect class of the week: correct code, false sentence

Three of L7's five rounds found a defect that was a **sentence**, not a
mechanism. Each time the code was right and the prose about it claimed one level
more, always in the reassuring direction:

- Round 1 → HIGH-1 fixed, HIGH-9 born. Round 2 → HIGH-4/HIGH-11 fixed, HIGH-14
  born. Round 3 → HIGH-14 fixed, and the fix's own prose became HIGH-17.
- HIGH-14's shape: a conservation check whose enumerator was a hand-written list,
  so a removal from a collection the list omitted changed *neither side* of the
  comparison. Three claim surfaces stated the guarantee unqualified. The fix — an
  exhaustive destructure, so a new field is a compile error — is ~12 lines and
  makes the sentences true.
- The trap in that fix, pre-registered in the repair triage and avoided: a
  compile error makes the decision *forced and visible*, it does not make it
  *correct*. Binding the new field to `_` is one keystroke away. The honest
  invariant is only "a new field does not compile until this function names it."
  Writing "now it truly generalises" would have been the fourth repetition.

**Recommendation, adopted for phase 2:** the claim-surface sweep — *is every
sentence this diff wrote true of what the code does?* — becomes a standing eval
scope item, not a per-lane addendum line. Phase 2's first evals found the same
class in two more lanes: a test comment asserting a binding that does not exist
(reverting the shipped preset left 670 tests green), and a preset guard whose
four claim surfaces stated a property it did not deliver.

## 5. The orchestrator is not exempt, and integration code is the least-reviewed code

HIGH-17 was written by the orchestrator, not by a worker, in the seam commit that
integrated two lanes. It generalised a *produce-side* invariant into a
*read-side* rule and stated it as a universal on four surfaces — including
`doc/src/reference/rpc-protocol.md`, the contract external readers implement
against. The mechanism was correct and its test bound; the paragraph
contradicted itself, and the lane's own passing test read exactly the payload the
sentence said could not exist.

Two things follow, and both are general:

1. **Seam and integration code is structurally under-reviewed.** It is written
   after every worker has finished, so no lane's eval and no lane's gate has ever
   seen it. It had exactly one reader — its author. Any integration edit that
   resolves a cross-lane conflict must be evaluated as its own unit, with the
   mutation that reverts it pre-declared.
2. **The author of a wrong sentence is the worst reader of its correction.** The
   correction was sent for an independent narrow read rather than self-certified,
   even though doctrine required no re-eval for a doc-only change. That read
   found a *new* false universal introduced by the correction itself (recorded as
   a residue, not blocking). Self-certification would have shipped it.

A related, cheaper observation: the exhaustive destructure from §4 made the
integration **fail to compile** until the incoming field was named. A structural
fix that converts a memory obligation into a compile error pays for itself the
first time an unrelated merge touches it.

## 6. Fixtures that cannot see what they were written for

The recurring failure named in `AUGUST-02-LEARNINGS.md` appeared twice more, and
should now be treated as this codebase's signature defect rather than a
recurring coincidence:

- A digest fixture built by serialising a *current* payload and deleting one key,
  standing in for a payload from an *older* producer — which emits the opposite
  shape. The prose it under-pinned was true, but nothing tested it.
- A pi adapter fixture that splices a bare `message_end` to represent an aborted
  turn. pi emits three records per assistant message (`message_start`,
  `message_update`, `message_end`), all carrying the same model, with
  `stopReason: pending` mid-stream. So a guard excluding `aborted`/`error` on a
  *descendant* filter simply relocates the read to the same excluded turn's
  `message_update` and resolves the identical model — and the fixture could not
  show it, because the spliced shape is one the real producer cannot emit. The
  evaluator rebuilt the fixture in pi's real lifecycle shape and reproduced it
  immediately.

The question that finds these is not "does the fixture pass" but **"what can this
fixture structurally not see?"** It belongs in every eval addendum.

## 7. A green gate is not evidence for a load-dependent defect

#419 recorded two flaky tests. A third was found during this day's work
(`executor::tests::launcher_failure_without_visible_unit_preserves_error_promptly`),
reproduced 1/16 on unmodified `main`. The lane fixed the two it was given and its
report carried an honest bound; the PR body nonetheless said `Closes #419` and
the CHANGELOG framed the population as two.

The eval's measurement is the part worth keeping:

```
at the PR head, quiet host:                     0 failures / 34 runs
at the PR head, two concurrent full suites:    10 failures / 1,446 runs
```

**Thirty-four consecutive clean runs proved nothing**, because the population
only fires under the load condition the issue names. Neither a green gate nor a
60-run clean batch is evidence of closure for this defect class. Any claim about
a flake population must state the load condition, the run count, and a
reverted-baseline comparison on the same host — and must not be attached to a
closing keyword until it does.

The corollary already in force: **a red gate is suspect before the diff is.**
Reproduce on `main` before attributing a failure to a lane. That rule saved a
lane cycle today — a red gate on a merged head turned out to be this same
population, on a diff containing zero occurrences of the failing test's subject.

## 8. Dispatch is one-shot, and that has a price

The third flake was found at 13:21. The lane that owned #419 was dispatched at
13:01 and committed its fix at 13:12 — nine minutes earlier — then ran until
14:15 on a two-test specification.

A headless worker reads its issues at startup. There is no channel to steer a
running lane. So a fact learned at minute 20 could only reach the worker after
minute 75, through a full eval → triage → repair cycle. For a one-line fact that
is a poor exchange rate, and the right call at 13:21 would have been to stop the
lane and re-dispatch it with the third test in scope rather than let it finish on
a stale spec.

Two options for the next phase, neither yet adopted: a mid-run steering channel,
or a standing rule that **new evidence on an in-flight lane's issue triggers an
explicit re-dispatch decision** rather than defaulting to "let it finish". The
second is free and should probably be the default.

## 9. The harness mirrors what it is building

The machinery driving this project — `dispatch.sh`, `eval.sh`, `repair.sh`,
`gate.sh`, a lane table and a ledger — is a job dispatcher, a witness ledger, a
gate, and per-unit worktree isolation. That is tally's own domain, reimplemented
in bash, to build tally.

This is worth stating plainly and then **not acting on**. Replacing it with
dogfooding cannot shorten the work: tally cannot drive tally until tally does
this job, and making it do this job is more work than the harness saves. The
bootstrap is legitimate. What the duplication actually indicates is a design
signal for later — the harness's own gaps (§8's missing steering channel, the
serialized gate queue) are gaps a mature tally would need to have closed anyway,
and they are being discovered for free.

## 10. What to carry forward

1. **Define done as a cut release, not an empty board.** Every convergence so far
   has been followed by an audit wave because nothing declared the work finished.
2. **Claim-surface sweep is a standing eval scope item**, not a per-lane line.
3. **Evaluate integration and seam commits as their own unit**, with a
   pre-declared revert mutation. Nobody else has read them.
4. **Never self-certify a correction to your own wrong claim.**
5. **"What can this fixture structurally not see?"** in every eval addendum.
6. **A flake claim must carry its load condition, run count, and reverted
   baseline** — and no closing keyword until it does.
7. **New evidence on an in-flight lane's issue forces a re-dispatch decision.**
8. **The severity floor holds only because it lives in the scripts.** Doctrine in
   prose decays; doctrine in a prompt template does not.

---

## Postscript — added at the phase-2 close (same date, closing orchestrator)

Phase 2 completed the evening of this document's writing: L2 merged `b4fa724`
(#379, #407, #378 closed; #419 held open with four members fixed and the
population explicitly not declared closed), L5 merged `38486f9` (#406 closed;
#405 closed under operator ruling D-2 with item 5 carved to #425), L6 merged
`c95e4b6` (#385, #386 closed). Seven issues closed in the phase, three filed
(#424 from an eval finding that cleared the floor, #425 and #426 from the two
operator rulings). Board 18 → 14, all three phase-2 lane milestones at zero.

Two of §10's items got their first data points the same day. The severity
floor held across six more eval rounds (one issue filed, everything else
residue or repair-now — the residue ledger carries thirteen lines, one already
closed in-lane by fix-forward). And §7's discipline paid out in the other
direction: the serialized gate queue was retired only after the #419 fixes
merged, verified by running the two remaining lanes' fleet gates concurrently,
both green under mutual load. §8's re-dispatch rule and §2's release question
remain open for phase 3.
