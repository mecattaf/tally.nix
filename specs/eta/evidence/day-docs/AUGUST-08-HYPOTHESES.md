# August 8 hypotheses — why the moles spawn, and what should kill the generator

Companion to `JULY31-LEARNINGS.md`, `AUGUST-01-DESIGN.md`, `AUGUST-02-LEARNINGS.md`,
`AUGUST-06-LEARNINGS.md` and `AUGUST-07-LEARNINGS.md`. August 6 named the standing
defect (a board refilled by its own audits); August 7 recorded the first wave whose
top issues came from use. This file is different in kind from its companions: it is
written *before* the evidence, on the evening the two-seam campaign was still
running, and it exists to be graded tomorrow, post-grind. Each hypothesis states
what would confirm it and what would refute it. Nothing here overrides an issue's
text.

## 1. The day's setup, for the record

Two mutually-blind orchestrator sessions ran in parallel against the same board:

- **Seam 1 (code)**: a planner produced a complete change plan for all 16 remaining
  member issues; Codex workers implemented it as atomic per-issue commits across
  isolated worktrees (9 of 16 delivered and verified at the last report, the rest
  gated behind the #419 soak). Deliverable: branch `agent/desired-state`.
- **Seam 2 (tests)**: a single Codex thread wrote the desired-state spec
  (`doc/final-conformance-bar.md`), then the full conformance suite
  (`test/final-bar/`, 26 black-box cases, corpora for both parser-pair boundaries,
  an arm→digest pipeline check), then mutation-validated it. Delivered and pushed:
  branch `agent/final-bar`. Definitive run on HEAD: 3 PASS / 23 FAIL / 0 harness
  errors, every failure traced to its issue.

Neither seam saw the other's artifacts, vocabulary, or branch. The #403 probe was
run independently by both; both got `REHYDRATES` (a resumed `codex exec` reports
thread-cumulative usage: 32,117 vs the fresh run's 16,050).

Tomorrow the two are ground against each other: the bar runs against
`agent/desired-state`, failures route back to the owning worker threads, until the
full bar is green on a re-gated train.

## 2. The diagnosis being tested

The last several days were whack-a-mole: every completed issue produced a new one.
The claim is that this had exactly two generators, both structural, neither of them
"the code is bad":

- **G1 — serial discovery.** No check ever took a campaign past reconcile, so
  everything downstream was first-execution territory. Each fix advanced the
  frontier one section and exposed the next section's blocker. Discovery rate was
  pinned to fix rate — one mole per whack, by construction.
- **G2 — twice-implemented contracts.** The recurring defect shape (locally
  rigorous modules, skewed contract between them: nine issues, five boundary
  pairs) exists because each authoring session held one side of a contract
  perfectly and nothing forced the two sides to agree. Fixing an instance leaves
  the generator alive; the next session touching either side re-skews it.

If the diagnosis is right, the countermeasures are not "fix more issues" but:
batch discovery (the pipeline check makes all unlit sections fail in one run) and
single-source contracts (corpora both sides must conform to). Both now exist on
`agent/final-bar`. Whether they work is what tomorrow measures.

## 3. The hypotheses

**H1 — batch discovery replaces serial discovery.** The 23-failure matrix is the
whole remaining pile, surfaced at once. *Confirmed if* the grind converges by
fixing against known failures, with at most incidental new findings, and no new
defect surfaces serially after convergence the way #429→#431→#441 did. *Refuted
if* the grind itself plays whack-a-mole — each fixed case exposing a fresh,
unmapped failure in territory the bar claimed to cover.

**H2 — the desired state is board-determined.** Two blind sessions reading the
same issues independently converged on the open design decisions (#415:
aggregates follow visible rows; #426: distinct exit code 4; #439: three-state
conflict domains; the same lineage-delta rollup model from the same probe
verdict). If the issues determine the design that tightly, the grind should be
about contract details, not design fights. *Confirmed if* the disagreement
protocol fires zero or once. *Refuted if* multiple failures escalate as genuine
design ties needing arbitration.

**H3 — the generator dies with the corpus.** After the grind converges, rerunning
the same audit posture that filed #442–#448 against the bar-green tree finds
nothing of the skew shape (a producer and consumer disagreeing about a shared
contract). *Confirmed if* the audit comes back empty of that class — whatever
else it finds. *Refuted if* it files new boundary-skew issues at pairs the
corpora cover, which would mean corpora do not actually pin the contract; or at
pairs nobody enumerated, which would mean the five-pair map was incomplete (a
softer failure: extend the map, not the method).

**H4 — post-grind arrivals are ordinary bugs at an ordinary rate.** With G1 and
G2 dead, new issues should arrive slowly, from use (the August 7 kind), each one
closeable without unmasking a successor — provided each fix lands with a bar case
that failed before it and passes after (the ratchet). *Confirmed over days, not
tomorrow*; the early signal is simply whether the board stays at zero for longer
than any previous convergence held.

## 4. What the hypotheses commit us to, if they hold

These are the standing changes the diagnosis implies; adopting them is the real
deliverable, the grind is just the evidence:

1. **The bar joins the permanent gate.** `test/final-bar/` wired into the checks
   that run on every change (the expensive members — the 480s soak, the N−1
   release build — may be a pre-merge-only or scheduled target). The structural
   problem was that no session holds the whole contract; the bar is the artifact
   that does, so it runs forever.
2. **Corpus-first boundary changes.** Any change to a shared contract — manifest
   grammar, adapter argv, registry schema, git-ai attributes — changes the corpus
   fixture in the same commit, and both sides conform to the fixture. Schema
   changes additionally carry an N−1 case in both directions (the #447 pattern).
3. **New issues ratchet the bar.** No fix merges without a case that failed
   before it. The class gets regression-tested, not the instance.
4. **The metric changes.** Issues-filed is not health; *unlit surface* is — the
   fraction of pipeline and boundary pairs exercised end-to-end by a standing
   check. Filed-count spikes when a new section lights up (the system working)
   and goes quiet when nothing unlit remains.

## 5. Grading note for tomorrow's session

Grade each of H1–H3 explicitly in the August 9 file with the grind's numbers:
iterations to convergence, new-vs-mapped failure counts (H1), escalation count
(H2), and the post-grind audit's findings by class (H3). H4 gets a line in each
subsequent day file until it is confirmed or the board refills. If any hypothesis
is refuted, say so plainly and name which generator survived — the countermeasure
to a surviving generator is a design change, not a bigger wave.
