# August 15 — session findings and the map of the record

Written 2026-08-15, during ext0's run (8/10 merged, authoring-doctrine-skills
retrying its cargo-tests gate after the cap raise decision). Standing
consumer: **the operator's post-ext0 step-back evaluation** — this file is
the comprehensive index of everything this session found, decided, and
recorded, plus the findings that until now existed only in conversation.
It follows the day-doc lineage (AUG14-LEARNINGS.md and predecessors).

## 1. The map of the record — where everything lives

| artifact | location | contents |
|---|---|---|
| The vestige ledger | `specs/substrate/evidence/vestige-sweep.md` | V-1..V-17 verified at file:line, severity = bite-likelihood x masquerade; interaction map I-A..I-E; cleared list; the complete self-contained A10 substrate-repair package (6 task groups, verification pass, urgent flags); the adapter-switch addendum with the A7 conditional |
| The zeta program | `ZETA.md` | Compiler rulings from the two-Fable tandem collision (identity, evidence-only epsilon dir, anchor grammar, fixture home, gate set, trace timing); sequencing ruling; stage plan; the five task specs verbatim (the A7 authoring source); operator acts A1–A9; risks |
| The spec layer | `specs/README.md` v2, `specs/constitution.md` v2 (A22 added, A2 amended), `specs/zeta/` (spec.md proposed, trace.json, contracts/), `skills/author-spec/SKILL.md` | The authority plane as authored for the zeta campaign |
| Epsilon evidence | `specs/epsilon-extension/evidence/` | The eight excavation ledgers (PA/VD/CA/EQ, final-shape, history-replay, intern-audit, intern-lineage), recovered from the epsilon session scratchpad — the live worklist's citations resolve here |
| The exploration record | `zeta-learnings/00–13 + raw/` | The authority-plane derivation (00–11), the local-models capability synthesis (12), the final-state portrait with the v0.0.1 blessing appendix (13) |
| Printed series | `~/Paper/jobs/2026-08-14-print-*` (CUPS 45–57) | Paper copies of the learnings series, the final-state portrait (job 56), the v0.0.1 appendix (job 57) |

## 2. Findings previously conversation-only, recorded here

### 2.1 Gate execution model, verified at file:line

A command gate is a pure flow node with **zero model involvement**:
`runGate` (examples/flows/spec-build.js:2299) renders `sh(gate.argv, …)` —
role GATE, pool campaign-control, judged solely by `evidence: ["exit:0"]`.
It dispatches through the same daemon path as every node: `spawn_execution`
stamps the **one global** `self.settings.unit_limits`
(crates/tally-core/src/daemon/completion.rs:323) onto every transient job
unit. Implications: (a) the 8 GiB cap binds deterministic verification
nodes — pure compilation — where there is nothing to contain, converting
verification into false negatives; (b) no per-node-kind limit distinction
exists anywhere in the plumbing; (c) gates are unsteerable by construction
(no agent in the loop; a steer fixed the lane's build parallelism but can
never reach a gate argv frozen in worklist bytes). This is the cleanest
demonstration that the vestige class cannot be fixed by prompting.

### 2.2 Memory-cap decision state (as of this writing)

Recommendation given and hardened across the night: **runtime drop-in on
tally-daemon.service at 24 GiB at a quiet moment**, as a bridge only —
A10 group 1 deletes the cap from the modules entirely, so no declared
dotfiles change is needed mid-campaign. Rationale: fourth bite; now binds
unsteerable gates; the chapter gate (fleet-gate + executing final bar,
cold worktree, 3h clock) is the heaviest compile of the run and at least
as exposed; zeta's five gates run the same class tonight.

### 2.3 The defect-topology assessment (the "whack-a-mole" question)

Given in answer to the operator's re-scoping question; the analysis:

- **The core has not failed once.** Admission, preflight, gates as merge
  criterion, ownership containment, squash, receipts, release: eight PRs
  merged tonight with zero false verdicts, zero manual product-code
  interventions, zero corrupted state. Every defect is peripheral —
  substrate constants, classification legibility, adapter portability.
  Things that waste time, not things that lie. The failure class that
  would justify re-architecture (false green, wrong merge, misrecorded
  receipt) has not occurred.
- **The moles are not random and the population is not unbounded.** All 17
  findings sit in families no audit had ever examined (cgroup limits,
  sandbox mechanisms, truncation, adapter defaults); the caps replay had
  already adjudicated the timeout/attempt family; the cleared list is
  large. This is one unexcavated seam — **the substrate** — excavated for
  the first time under production load, which was deploy-3's stated
  purpose.
- **This is the legacy of the iterations, and it was already ruled on.**
  AUGUST-3-morning-thoughts.md prescribed the baggage-reduction run before
  the v0.0.1 cut. What exists now that Aug 3 lacked: a mechanical census
  of the class (the V-ledger) and, once zeta closes, a linter that keeps
  it dead. The whack-a-mole ends when the class-killers land, not when the
  last mole is whacked — and every class-killer is scheduled (zeta:
  L7/L12/L16; A10: portability matrix, substrate-numeral check, OOM
  legibility, adapter-terminal outcomes).
- **Recommendation: no full re-scope.** One scope *addition*, which is the
  honest kernel of the worry: tally spec'd its code but never its
  substrate — that is the gap every mole crawled out of. `specs/substrate/`
  becomes a governed identity at the A10 sitting.
- **What actually cost the night was masquerade, not caps** — roughly five
  hours went to misattribution. Prioritize A10's legibility groups (2 and
  6) accordingly.
- **Strategic frame:** every one of these constants would have bitten
  agency at 30M-line scale, where a chapter gate is far heavier and a
  masquerade costs days. Tally's own body, on a self-hosted shakedown
  night, is the cheapest place in the program this excavation could have
  happened.

### 2.4 Attribution and craft notes from the night

- The supervisor discovered V-1 live (the 14 OOM kills); the sweep was
  commissioned in response and adjudicated it plus 16 more. Independent
  convergence, correctly ordered.
- Two supervisor misattributions occurred (adapter flakiness for OOM
  downstream symptoms; transient machinery fault for terminal quota) —
  both are now V-class findings whose D58 price is exactly the manual
  rule the misattribution taught ("read the capture archive first").
- The adapter switch (codex quota → claude-code/Opus) was exemplary craft:
  zero-attestation adapter smoke-tested with --assert-commit before a live
  campaign touched it; policy rejection path verified in source; model
  name kept out of worklist bytes (host-side resolution); one pre-existing
  L16 instance (the `Sonnet` token, epsilon-extension.json judge-verdict
  goal) recorded, not fixed — evidence the zeta linter has bite before it
  exists.

### 2.5 The compute-economics ruling (learnings must crystallize or die)

The operator's closing concern, made structural: a learning that lives in
prose is a liability — derived at frontier-model cost, re-derived when the
conversation dies, paid a third time when it bites unenforced (the caps
replay adjudicated timeouts in prose; the memory cap still bit five days
later). A learning that crosses into enforcement is **crystallized
compute**: a lint rule, gate argv, fixture, or schema is paid for once and
then runs free forever with no model in the loop. The audit test to apply
to every learning artifact at the step-back: **"what committed byte
enforces this, and what reads that byte?"** (A15 applied to learnings
themselves). Three buckets as of tonight: *converted* (constitution + citing
skills, receipts machinery, ext0's eight merged mechanisms, the spec-lint
rules); *conversion-scheduled* (V-ledger → A10 worklist; zeta spec → flake
check; evidence ledgers → goal citations — fine only if the consumer
lands); *still prose* (parts of zeta-learnings/, the day-docs, sections of
this file — each names a standing consumer or is deleted at the v0.0.1
cleanup). The step-back should open with this audit; this session offered
to run it: every learning artifact mapped to its enforcement byte or
flagged unconverted.

### 2.6 The two-layer split and the baseline-parity law (the trust question)

The operator's final concern, recorded honestly: in at least three
instances tonight tally made execution strictly worse than a bare agent in
a terminal (no cgroup around a bare compiler; no jailer on a bare
diagnosis; a bare run shows the quota error instead of laundering it into
a projection timeout). The harness violated its own prime directive —
don't get in the agent's way — while enforcing discipline on everyone
else. Diagnosis: tally is two things fused with opposite records. The
**designed core** (worklist authority, gates, ownership, receipts,
release) was adversarially derived and has the month's best numbers —
eight unattended merges tonight, zero false verdicts, zero silent
corruption ever; the bare-agent counterfactual fails *quietly* (the
pre-tally record: confident wrong merges, phantom pointers). The
**accreted substrate** (systemd plumbing, caps, sandbox defaults,
classification fallbacks) was never derived by anyone — July-20
scaffolding no excavation touched — and it is where every wound of the
night lives. Operator distrust of tally is properly distrust of the one
layer never put through tally's own discipline.

**Proposed law for the substrate spec (step-back candidate, then an A10
claim): baseline parity.** Whatever an agent can do bare in a terminal —
write temp files, use /dev/shm, compile at full parallelism, see its own
error output — it must be able to do inside a lane; every deliberate gap
is a documented containment ruling with a named justification, or it is a
defect. Mechanically checkable: extend the existing smoke genre
(`tally adapter smoke --assert-commit`) into a bare-vs-laned parity probe
run as a standing check, so "the harness does not fight the agent" becomes
a witnessed property of every gated head rather than a hope. The paranoia
ends the way everything ends here: not with reassurance, with a gate.

## 3. Open decisions for the step-back (consolidated)

1. **A7 adapter choice** — new codex subscription vs claude-code (now
   1-for-1 on attestations). Conditional recorded in the vestige addendum:
   codex requires the `diagnosisSandboxPolicy: "workspace-write"` override;
   claude-code with nulled policies does not.
2. **DECISION-1** (steward field value post-ext0) and **UNKNOWN-1** (3.2's
   would-be oracle) — drain at the A7 sitting, per specs/zeta/spec.md.
3. **A10 timing** — self-contained act after A9, per the ledger's package;
   the memory-cap drop-in is its bridge.
4. **The v0.0.1 blessing** — ratchets and shape in
   zeta-learnings/13-final-state-portrait.md (appendix); the V-ledger and
   AUGUST-3-morning-thoughts.md jointly seed the cleanup worklist.
5. **R4** (forge:"local" remote-semantics one-word ruling) — still open,
   gates nothing until ext1.
6. **Supervisor's standing flags** — fleet-deploy timer disarmed; dotfiles
   deploy uncommitted (what makes the timer a trap).
7. **Judge-tier corpus replay** — runnable after ext2, never yet run.
8. **Repo hygiene** — everything this session wrote is untracked until the
   A-act commits land; `specs/substrate/` rides A10's sitting commit, this
   file and zeta-learnings/12–13 + raw/tandem-* ride the next record
   commit.
9. **The learnings-to-enforcement audit** (§2.5) — the recommended opening
   move of the step-back itself: every learning artifact mapped to its
   enforcement byte or flagged unconverted.
10. **The baseline-parity law** (§2.6) — adopt as a substrate-spec claim at
    the A10 sitting; the parity probe joins the smoke-test genre.
