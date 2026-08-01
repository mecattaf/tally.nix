# August 1 design synthesis — the reconciler era, the steward, and the quiet surface

Companion to `JULY31-LEARNINGS.md`. That file recorded what the first codegen
campaign taught; this one records the target model that emerged from the
post-mortem wave (#253–#262) and from a parallel design session that audited the
codebase, its two ancestors (the Claude Code workflow dialect, spec-kit's
`specify` runner), and the estate's live working patterns. Written while wave 2
was in flight; nothing here overrides an issue's own text — where an issue is
more specific, the issue wins.

## 1. A campaign is a state object, not a process

The #253 pivot restates what a campaign *is*: durable desired state plus a
convergence loop, not a long-lived run.

- **Desired state** — the worklist DAG: tasks, dependency edges, conflict
  domains (#254), checkpoint barrier nodes (#256).
- **Actual state** — merged PRs and closed issues on the forge: witnessed
  checkboxes that cannot lie.
- **Execution** — short stateless reconcile passes: `remaining = worklist −
  merged`; `frontier = tasks whose deps are all merged`; dispatch the frontier
  into isolated worktree lanes, concurrent only where conflict domains are
  disjoint, re-gated on the rebased head before any concurrent merge (#255);
  exit. Every merge, mention, or timer triggers the next pass.

Consequences worth naming once: re-mention is always safe and is the entire
recovery procedure; args/script changes between passes are non-events; the 24 h
budget stops being a campaign concern; and the witness ledger is demoted to what
it should have been — the audit record, never the continuation mechanism.

## 2. The steward: an intern, not a certifier

The oversight model attached to a campaign is deliberately weak and deliberately
thin. The framing that survived several rounds of design: the frontier coding
model is the super-genius professor; the steward is the intern who keeps the
professor fed and the lab running. The intern is essential to output quality and
is never asked to analyze the professor's results.

Three consequences, in decreasing order of importance:

1. **Nobody second-guesses the coder — including the steward.** The coder's "I
   think it's done" is not an input to anything. The merge criterion is
   witnessed gate output, and gates are code. The case where the coder believes
   the work is ready but tests or lint disagree is detected mechanically, with
   zero model judgment. (The `/goal` evaluator's doctrine, upgraded: the
   worker's claim is evidence, not proof — but here proof is cheap and
   deterministic, so no model ever needs to doubt.)
2. **Routing is code.** "What runs next, given the map?" is set arithmetic over
   forge state. No model belongs in that loop; the reconciler is the router, and
   the "eventually replace the supervisor with a smaller model" limit is reached
   immediately for routing, because the smallest model is no model.
3. **The steward's whole duty roster is two slots**, both thin, both typed:
   - **Diagnose-and-steer** (#257): on a failed task node, translate the capture
     stderr + gate output + brief + diff into one steering note; the task
     re-dispatches once with steering visible; a second failure blocks only its
     subtree; escalation to the operator fires exactly once, at frontier
     quiescence. Typed verdict `{steering | blocked}` — the `blocked` arm is the
     `/goal` "impossible" hatch: the coder claiming impossibility is evidence,
     not proof.
   - **Narrate** at the publication boundary: conventional-commit messages,
     PR prose, the closing summary — text only, validated deterministically
     (commitlint-shaped grammar check, template fallback), never executing git.
     Model proposes, validator enforces, node executes.

Bind the steward as a catalog role, not a script choice. Start with Sonnet while
the mechanism stabilizes; the downgrade path is empirical, not aspirational —
every diagnosis and narration is journaled, so a smaller candidate can be
replayed against the Sonnet corpus and the disagreement rate measured before any
swap. Completeness-checking is *not* steward initiative: deeper questions belong
in the map as checkpoint barriers (#256) and standing constraint gates (#259),
authored at spec-freeze time by the model that wrote the plan.

**The mechanization ladder** (this is the general pattern-accumulation method):
frontier stewards improvise behaviors unprompted → the recurring ones are
extracted into mechanism with thin model slots → the slots get cheaper. The
Opus steward transcripts from early campaigns are the pattern mine; #257 is the
first extraction.

## 3. The two surfaces, and GitHub as a disciplined dev's footprint

There are two surfaces, and they cross at exactly one node.

- **Internal** — tally's journal, worktrees, gate transcripts, capture files,
  reconcile passes, diagnosis attempts, retries, and the coder's raw working
  commits. Queryable via the run-status verb (#261); never mirrored outward.
- **Exposed** — what a competent human collaborator would leave behind: one
  campaign issue with live tasklist checkboxes (maintained by tally, flipped by
  merged PRs — #258's projection half), tidy PRs (squash-merged, one
  conventional commit per task, steward-authored message), rare steering
  comments on real failures, exactly one escalation at quiescence, one closing
  summary, issue closed.

The crossing point is the publish node — the same place a real dev squashes WIP
before opening a PR. Everything before it is private by default. The operating
rule: **process lives in the journal; outcomes live on the forge.** GitHub
projection duplicated from the journal is not just noise — it trains operators
and steering agents to debug from the wrong surface (the first campaign's
supervisor did exactly this while the answer sat in `capture/<task>.err`).

Target end-state of a campaign as seen from GitHub: indistinguishable from a
disciplined solo developer on a good week.

## 4. The mention, redefined; and the amendment to July 31

Under the reconciler, an at-mention stops being a command and becomes an
**idempotent nudge to reconcile**. It keeps exactly two properties worth
keeping: authorization (exact grammar, allowed actors, audit trail) and
remoteness (the only trigger surface that works away from a terminal). Locally,
`tally campaign arm` / a flow run is equivalent and unceremonious.

July 31 ruled "issues never define work." #258 amends this, and the amendment
should be understood precisely: the July failure was never issues-as-data — it
was **drift** (two copies of the truth) plus **puppeteering** (a supervisor in
the dispatch loop). With tally owning the projection and the reconciler as the
only dispatcher, the forge becomes a database that cannot lie. The refined rule
is a split by campaign class:

- **Ad-hoc campaigns are forge-native** (#258): the master issue is the
  container — config and worklist DAG in its body, per-task briefs as
  sub-issues, armed from the issue with no nix change and no deploy. Right
  weight class for a one-night buildout.
- **Spec-corpus campaigns keep the repository as work source** (the agency
  shape): the worklist artifact lives in the spec repo, the master issue is its
  projection. This requires the two-repo seam — worklist and evidence from a
  spec repo, worktrees and PRs on a code repo — which the audit classified as
  additive (the repository config is already a per-node brief field).

## 5. Heterogeneous compute, one witnessed graph

The role table the whole design serves:

| Role | Who | When |
|---|---|---|
| Author the map (worklist DAG, conflict domains, checkpoints, briefs) | Frontier orchestrator (Fable-class), with the human | Spec-freeze — where irreplaceable effort belongs |
| Implement | Frontier coder (Opus/Fable/GPT-class, max thinking), one per lane | Reconcile passes |
| Prove | Deterministic gates + constraint gates + checkpoint barriers | Every increment |
| Route, merge, escalate | The reconciler (code) | Every pass |
| Diagnose, steer, narrate | Steward (Sonnet now; smaller later, by measurement) | Failure paths and the publish boundary |

Cross-harness composition is native because every node is a harness-agnostic
`job(spec)` — the one capability neither ancestor dialect could express.

## 6. What stays frozen, and where patterns accumulate

The audit's clearest finding: the enqueue kernel is the settled layer. The
two-part key — `dedupKey` (identity) × `payloadHash` (work-equality) — resolving
to five dispositions (created / attached / reused / terminal / conflict), with
reuse evidence-probed against artifact drift, predates flows and is what makes
re-mention idempotent and lanes cheap. None of this design touches it.

Equally settled by lineage: **no JS module system, ever** — the determinism and
replay guarantees rest on the dialect having no loader. Proven workflow patterns
accumulate at four sanctioned layers instead:

1. bootstrap combinators (`parallel`, `pipeline`, `quorum`, `dissent`, …);
2. parameterized generic flows — the campaign artifact is data, not code;
3. worklist-schema enrichment (dependency edges, conflict domains, checkpoints —
   the parallelism intelligence lives in the data);
4. cookbook documentation.

Ancestry, for the record: the Claude Code workflow dialect contributed the
authoring format (deterministic JS, the combinators, structured results — kept
because models are RL-trained on the idiom) while its primitive (`agent(prompt)`,
LLM-only) and its local scheduler were deliberately replaced (`job(spec)`;
the daemon, never the runner, arbitrates). spec-kit contributed its step-type
vocabulary as a completeness checklist and nothing else — a fresh audit of its
runner confirmed the disqualifiers (sequential-only, coarse resume, agent
stdout discarded) and one deliberate non-port: its mid-run human gate.
"Phase done, awaiting operator" remains the single worst state an unattended
campaign can enter; bounded machine judgment (#256, #257) is what makes that
refusal viable at scale.

## 7. git-ai: the dormant fourth proof axis

The codebase carries a complete authorship integration that has never been
armed: `services.tally.gitAi` (default `enable = false`, never set by the
estate) runs a settlement barrier at code-result completion — waiting for the
externally provisioned `git-ai` binary's notes (`refs/notes/ai`) on the result
revision — and binds session, model, and note content into the witness record,
cross-linked by task/attempt/lease/flow-run. `mode = "required"` makes a
missing binding fail the result; `tally authorship verify` re-checks the
binding later with typed failure statuses. The estate packages the binary
(dotfiles) but no config enables the binding; no flow references it.

Where it sits in this design: it is the **fourth proof axis**, not a
replacement for anything model-shaped. Gates prove behavior; the witness proves
execution; checkpoints prove the accumulated system; git-ai proves authorship —
in the repository itself, which is the quiet-surface doctrine done properly:
prose provenance claims in PR bodies become verifiable repo-native metadata
plus the `Assisted-by: <adapter>:<model> (tally:<taskUuid> witness:<seq>)`
trailer (a pointer, never the proof).

One hard interaction before arming it: **notes do not survive squash-merge.**
A squash mints a new commit with no note and the noted working branch is
deleted after merge. Under campaign squash semantics the binding point must
move to the publish node — bind/rebind on the final squash commit, with the
steward-authored message carrying the trailer. Arming git-ai (advisory first)
is therefore one work item with the `mergeMethod` option, not a separate one.

## 8. Wave-3 backlog owed by the August 1 audit

To be minted as issues once wave 2 lands; audit evidence lives in the session
record.

1. **Quiet projection bundle** — split `postReceipt` (accepted vs duplicate
   ack); an off-switch for the driver's per-merge progress comment; a
   run-scoped digest + markdown renderer (the nightly driver's report renderer
   is prior art); a closing summary that also fires on failure; `mkDefault` on
   the campaign producer's projection literals.
2. **Steward seam** — narrator/standardizer adapter (config-only: Nix adapter
   table, curl shim, `scrape.finalMessage` precedent); the `steward` catalog
   role in campaign options; `mergeMethod` option (squash default for
   campaigns) with steward-authored messages **including git-ai note
   propagation and the Assisted-by trailer** (§7).
3. **Two-repo campaigns** — spec-repo worklist/evidence, code-repo PRs;
   additive per the audit; prerequisite for the agency campaigns.
4. **Defect list** — flow `codex()` nodes get the right process cwd but lose
   their `-C <worktree>` argv (post-#232 asymmetry); `requestReview` serializes
   a boolean instead of requesting a review; unset `closeOnPass` silently
   inherits `postEvidence`; hardPreempt doc/code divergence on co-allocated
   pools; worklist reads skip `fetch` wherever a repo-file worklist remains.
