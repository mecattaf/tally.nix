# The grind: converge agent/desired-state onto the conformance bar

Context for this session: while you built `agent/desired-state`, a separate blind session
built the definitive conformance suite for this repository on branch `agent/final-bar`
(pushed to origin). It asserts intended behavior at public boundaries — 26 black-box
cases covering every non-meta issue, conformance corpora for the parser and adapter
boundary pairs, and a full arm→digest pipeline check. It was mutation-validated and its
failure matrix on pre-campaign `main` traced 23 expected failures to their issues.

Your mission now: iterate `agent/desired-state` until the full bar passes against it.
The bar is the acceptance contract. Your worker threads, worktrees, and ledger from the
implementation campaign remain your instruments — resume the exact owning thread for
each defect, per the steer-codex skill.

## Running the bar

From a checkout of `agent/final-bar` (use a dedicated worktree; never your writers'
trees):

- Full pass: `test/final-bar/run /absolute/path/to/desired-state-worktree`
  (equivalently `nix run .#final-conformance-bar -- <tree>`)
- The runner exposes a case inventory; use it to re-run only affected cases during
  iteration. Reserve full passes for confirming convergence — a full pass includes a
  480-second soak wave and building a pinned N−1 release binary.

## The loop

1. Run the full bar against the `agent/desired-state` head. Record the failure matrix.
2. Group failures by issue, and map each to its owning worker thread via your ledger.
3. Resume the owning thread with the **concrete failing evidence only**: the case name,
   the command the harness ran, expected vs. actual output. Do not paste the suite's
   source, the spec document, or its rationale — the worker fixes the behavior, not the
   test. Tests and spec are read-only to this entire session.
4. Fixes land by amending the original atomic commit for that issue (rebase-fixup on
   `agent/desired-state`), preserving one-commit-per-issue and the issue→commit mapping.
   Re-gate: every commit in the rebased train must be re-verified green under
   `nix flake check` — bisectability survives every grind iteration or the iteration
   isn't done. Re-gate in parallel, not serially: check each rebased commit out into
   its own worktree and gate the lanes concurrently (the machine sustains six
   concurrent full-suite lanes; only the final gate on the train head runs last).
   Commits at or before the earliest amended commit are unchanged by the rebase and
   keep their prior verification — only the rewritten suffix needs re-gating.
5. Re-run the affected cases; when they pass, continue to the next group. When all
   groups pass individually, run the full bar again from step 1.
6. Converged means: one full pass, all cases green, zero harness errors, on a fully
   re-gated `agent/desired-state`. Push the final branch.

## Disagreement protocol

If a worker or you conclude that a failing assertion encodes a *wrong decision* — not a
code defect — do not work around it, do not weaken the code to mimic it, and do not
touch the test. Stop that group and report to the user: the case, the asserted
expectation, the behavior you believe is correct, and the evidence. The user arbitrates
(the spec's authoring session is resumable on its side). Continue grinding all other
groups meanwhile.

Likely candidates for genuine ties, if any arise: the decided semantics for #415 and
#426, and canonical-side choices on the boundary pairs. Both sessions converged
independently on most of these, so expect few — but escalate real ones rather than
absorbing them.

## Done

- Full bar green against `agent/desired-state`, zero harness errors.
- Every commit in the final train re-gated green; issue→commit mapping intact.
- Branch pushed. Final report: iterations taken, defects fixed per issue, any
  escalated ties and their resolutions. Merging to `main` is the user's call — do not
  merge or open a PR.

---

# FINALITY — the grind converged, merged, and closed the board (2026-08-09)

**Status: DONE.** The mission above completed in one day, and then went two steps
further on the user's direction: `agent/desired-state` was merged to `main`, the entire
issue board was closed, and the conformance bar itself was merged so the repository now
carries its own acceptance contract. No release was cut — deliberately. This appendix is
the complete record.

## Final state

- **`main` = `66a3d1c`**, linear history on `4a99f64`:
  - 16 atomic commits (`4a99f64..9e56cea`), one per member issue, fast-forward merged —
    never squashed; per-commit bisectability under full `nix flake check -L` was
    re-verified after every grind rebase and survives on `main`.
  - 6 conformance-bar commits (`9e56cea..66a3d1c`), rebased clean (zero conflicts) and
    fast-forward merged. `nix flake check` now includes the bar harness;
    the full bar runs via `nix run .#final-conformance-bar -- <tree>`.
- **Final bar verdict: 26 PASS / 0 FAIL / 0 harness errors** — confirmed twice at the
  end: run 7 against the final train head, and run 8 from the rebased bar tree against
  merged `main`, after a full green `nix flake check -L` on the merged result.
- **Board: zero open issues.** All 16 member issues closed with their owning commit
  SHAs; #410 (meta umbrella) closed with the campaign summary. #441 had already closed
  via PR #449.
- **Daily-drive pin: `github:mecattaf/tally.nix` @ `66a3d1c`.** No public release yet —
  the user daily-drives first.

## How convergence went: 7 bar runs, 6 integration rounds

| Run | vs head | Result | What fell |
|---|---|---|---|
| 1 | 9c800ef | 8/18/0 | baseline: 18 failures → 8 owning-thread groups |
| 2 | 97de58d | 20/6/0 | 12 groups converged in one iteration |
| 3 | 16bfd93 | 24/2/0 | manifest corpus, registry N−1, both usage cases |
| 4 | 71f8ffe | 25/1/0 | reader-state explicit identity |
| 5 | e89f244 | 25/1/0 | pipeline: issue now closes, merge lands |
| 6 | 024512e | 25/1/0 | pipeline: digest posts to the right forge |
| 7 | **9e56cea** | **26/0/0** | converged |

Mechanics each cycle: resume the exact owning campaign worker thread with concrete
failing evidence only (case, command, expected vs. actual — never suite source); fixes
land as `fixup!` commits on `grind/<worker>` branches; the integration thread autosquashes
into the owning commits and re-gates the rewritten suffix in ~6 parallel worktree lanes
(tmpfs scratch isolation after disk-pressure reds), serial head gate last. One commit per
issue survived every round; the integration worker additionally repaired two historical
ownership misplacements (#446's decoder use inside #444; #439's conflictDomains
assertions) found by the per-commit gates.

## What the bar caught (the defects that mattered)

1. **Pair-1 skew reintroduced by the campaign itself**: the packaged driver rejected the
   arm's own canonical brief over `armedManifest` — the whack-a-mole generator caught in
   the act by an independently-derived bar. Fixed by making the brief
   (`worklist.graphDigest` + normalized `armedManifest`) sufficient for reconcile
   through the single admission grammar, digest cross-verified.
2. **Four latent defects past reconcile** — territory no check had ever walked, exactly
   the bar's declared known-gap. Surfaced one per run as each fix lit the next stage:
   the poll guard treated "forge unchanged" as "nothing to do" while durable local state
   had advanced (campaign stalled forever with completed work); closeout used
   `--body-file -` stdin convention; the closing summary followed the *code* forge
   (`local`) instead of the *issue* forge (github) so digests never reached the issue;
   a comment-dedup lookup used a `--jq` form outside the recorded forge grammar. All
   fixed in #442's commit (campaign continuation ownership) with VM regressions and
   recorders that now reject unsupported forge forms.
3. **A real query bug**: membership-only archived tasks invisible to explicit run
   lookups — durable membership never fed the jobs anchor set, and on the second pass an
   empty reconciliation-scoped `tasks` array shadowed the durable `items` projection.
4. **Surface-contract alignment** (two blind derivations agreeing on substance,
   diverging on lexicon): `verification=present|none` tokens; `campaigns/host-tuning/`
   sidecar path; `--model` before the thread-id positional; typed Pi pre-launch refusals
   with `option-like-workload-head`/`index 0` tokens; `counterScope=session-cumulative`
   and `launch.rejectOptionLikeWorkloadHead` declarations; first-class
   `coverage.declaredByField`/`reportedByField`/`attemptsMissingAttestation`/`isComplete`;
   fresh-zero/delta/baseline-missing checkpoint admission with predecessor verification
   (no double-charging, 10101/22016/11/32128 pinned); finer caveat vocabulary
   (`total-only-attempts`, `declared-surface-unknown`, `cumulative-baseline-missing`);
   four exact-name launch-cwd producer regressions; the probe's explicit-binary
   interface.

## Decisions made without escalation (flagged per protocol)

- **Zero disagreement-protocol invocations reached the user.** H2 held completely: both
  blind sessions agreed on every decided semantic (#415, #426, all five boundary
  canonical sides). The one worker stop (u419) dissolved with harness-argv evidence; the
  one Track A stop (pipeline digest) was correct out-of-scope discipline, re-routed.
- **Orchestrator routing**: the pipeline liveness + terminal-closeout defects lived in
  pre-campaign code owned by no train commit; they were attributed to #442 (campaign
  continuation, closest owner) to preserve the 16-commit issue→commit mapping.
- **Sidecar location**: `campaigns/extensions/` vs `campaigns/host-tuning/` — semantics
  identical, bar's public path won per the grind contract.
- **Recorded-grammar discipline**: after two rounds of one-invocation-per-run
  convergence, the fix was to audit the entire terminal forge path against the
  proven-grammar list at once and tighten local recorders to reject unknown forms —
  that ended the long tail.

## Operational lessons (for the next orchestrator)

- Bar case runs must be **serial** and use **short artifact paths**: 7 concurrent runs
  starved `nix eval` past its 300s budget, and nested artifact dirs overflowed
  `SUN_LEN` on the daemon socket. One serial full pass ≈ 25 min warm.
- `setsid nohup ... & echo $!` records the wrapper PID, not the worker — resolve the
  real PID via `pgrep -f <thread-id>` before waiting on it. Plain background wait-shells
  get swept; persistent Monitor tasks survive.
- Parallel full-suite lanes can produce **environmental reds** (disk-pressure storage
  mutations in `fs5_live_acceptance_matrix`); tmpfs scratch isolation per lane fixed it
  without touching content.
- Worker-local validation can pass while the bar fails: three misses were exactly-shaped
  public-boundary details (tasks-vs-items read order, plain-poll vs token event, shim
  grammar). Evidence must pin the consuming expression, not just the intent.

## Hypothesis grades (per the user's prediction framework)

- **H1 held**: every failure across all 7 runs mapped to known issues or newly-lit
  known-gap territory; zero unmapped.
- **H2 held**: zero design-conflict escalations; divergence was lexicon, not decisions.
- **Prediction "converges in 2–3 iterations"**: true for 25 of 26 cases (3 iterations);
  the full-pipeline case took 4 more single-defect rounds — not iteration failures but
  the known gap paying out its debt one newly-reachable defect at a time.
- **H3/H4 remain open** by design: the next evidence is a real mission on the
  daily-driven pin. Predicted failure class: operational, not contract-skew.

## What deliberately did NOT happen

- No release, no version bump, no deployed-pin move (user daily-drives `66a3d1c` first).
- The §4 ratchet rules (corpus-first boundary changes; no fix without a failing case)
  are still only in the untracked `AUGUST-08-HYPOTHESES.md` — encoding them into repo
  doctrine is open work, and the `armedManifest` incident is the argument for doing it.
- The full bar remains an operator-invoked gate (`nix run .#final-conformance-bar`),
  not an automatic one — ritualizing it belongs to the deferred release step.
- Cleanup pending: `ds-*`/`grind-*` worktrees and local branches, remote
  `agent/desired-state` + `agent/final-bar` (both merged, deletable), stale `latest.md`,
  pre-campaign `w6-*` worktrees.

## Artifacts

- Grind ledger (full audit trail, all 7 runs, per-worker prompts/JSONL, integration
  reports): `/home/tom/mecattaf/tally-codex-runs/grind/GRIND-LEDGER.md`
- Bar run artifacts: `tally-codex-runs/grind/{bar-run-1,r2..r8}/report.json` + preserved
  failing-case state (daemon data dirs, forge recordings) per run.
- Campaign worker threads (all resumable): ledger table in
  `tally-codex-runs/desired-state/LEDGER.md`; grind prompts in
  `tally-codex-runs/grind/<worker>/grind-*.md`.

The two-blind-sessions → grind mechanism did what it was designed to do: implementation
and acceptance derived independently from the same intent, collided, and every collision
was either a vocabulary alignment or a real defect — including the one class of defect
(cross-boundary skew) this repository is constitutionally prone to, caught alive twice.
Whether this is "the one" is now an empirical question that only daily driving answers.
