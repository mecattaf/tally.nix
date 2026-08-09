# Mission: bring tally.nix to its desired state — complete code change campaign

You are a Claude Fable orchestrator session. Your job is to drive the repository
`/home/tom/mecattaf/tally.nix` from its current state to the desired state described by its
open issue board, as **production code changes in atomic commits**. You do this by invoking
the `steer-codex` skill and operating as the control plane for autonomous Codex CLI workers,
per that skill's contract: Codex owns each task from investigation through delivery; you
launch, preserve, observe, resume, and verify outcomes. You do not implement code yourself
and you do not prescribe implementations to workers.

Run until completion. Do not stop at a plan, a partial wave, or a status report. The session
ends only when the deliverable described at the bottom exists and is pushed, or you hit a
genuine blocker that only the user can resolve.

## Ground truth

- Repo: `/home/tom/mecattaf/tally.nix`. Default branch `main`. No GitHub Actions; the gate
  is local `nix flake check` (~45 targets, including four VM tests).
- Current branch `agent/fix-campaign-git-ai-validation` at `eb62b8f` carries PR **#449**
  (DRAFT, mergeable, 1 commit, closes #441). It is verified green and blocks all campaign
  execution.
- The file `latest.md` (untracked) is a stale Aug 5 close-out. It is **not** the plan of
  record. Issue **#410** is the doctrine source.
- 18 open issues: #402 #403 #408 #409 #410 #415 #419 #426 #439 #440 #441 #442 #443 #444
  #445 #446 #447 #448. Read every one in full before planning.

### Standing diagnosis (established, do not relitigate)

The defects on this board are almost all **cross-boundary contract skews**: each module is
locally rigorous, but contracts disagree across boundaries, because each authoring session
held one module's invariants perfectly and nothing exercises a boundary end-to-end. Nine
issues are of this class and collapse to five boundary pairs:

1. Rust arm CLI ↔ Python `spec_build_driver.py` — #444, #446, #439 (one manifest grammar,
   two independent parsers)
2. Nix preset/module ↔ Rust adapter argv — #442, #443, #445
3. Attribute producer ↔ attribute validator — #441 (fix already exists as PR #449)
4. Registry schema vN ↔ vN−1 reader — #447
5. Store-path producer ↔ Nix GC — #448

The remaining issues: rollup evidence semantics (#402, #403, #408, #409 — one coherent
design area, "L1"), test hygiene on existing tests (#419, #440), an open design decision
(#415), a deferred-by-design item (#426), and the meta umbrella (#410).

A fix that repairs one instance of a skew without making the two sides share one contract
merely resets the clock. The planner must resolve, for each boundary pair, **which side is
canonical**, and the changes must make both sides conform to that single contract.

## Operating mode

- Invoke the `steer-codex` skill first and follow it exactly: detached `codex exec` with
  full-access flags, stdin prompts, JSONL logs, recorded thread IDs, resume by thread ID.
  You have standing authority for full-capability Codex orchestration for this entire
  mission — do not re-ask per worker.
- Run root for all worker artifacts: `/home/tom/mecattaf/tally-codex-runs/desired-state/`
  (create it; one subdirectory per worker).
- Every concurrent writer gets its own git worktree and branch. Never point two writers at
  one working tree, and never run writers directly in `/home/tom/mecattaf/tally.nix`'s main
  working tree — the user may have other terminals open on this repository.
- Do **not** comment on GitHub issues, do not open new PRs, and do not merge anything to
  `main` — with the single exception of PR #449 in Phase 0. Delivery is a pushed branch.

## Phases

### Phase 0 — preflight

1. Un-draft and merge PR #449 (`gh pr ready 449 && gh pr merge 449`, squash or merge per
   repo convention). This closes #441 and unblocks campaign execution. Update local `main`.
2. Run the date-gated empirical probe for #403: `/home/tom/mecattaf/tally-codex-runs/probe-403/probe.sh`.
   It prints `REHYDRATES` or `FRESH-START`. Record the verdict — it is a mandatory design
   input for the L1 issues (#402/#403/#408/#409) and no L1 code may be planned or written
   before it is read.

### Phase 1 — the planner

Launch **one** Codex planner worker whose task is: read the full text of all open issues,
the codebase, and #410's doctrine, and produce a complete written change plan that takes
tally.nix to the desired state. The plan must:

- cover **every** open issue (except the meta #410 itself, which the board owner closes),
  mapping each to concrete changes — files, contracts, behavior;
- for each of the five boundary pairs, state the canonical contract and how both sides
  will conform to it;
- design the L1 evidence model once, coherently across #402/#408/#409, incorporating the
  probe verdict for #403;
- **decide** the open decisions: #415 (what a filtered view's aggregates mean) and #426
  (whether the deferred exit code now has a consumer or is implemented alongside this
  campaign) — a decision with rationale recorded in the plan, not a punt;
- order the work into atomic units, **one unit per member issue**, with an explicit
  dependency/parallelism structure.

You review the plan only for completeness (every issue covered, every decision made, probe
verdict incorporated) — not to second-guess implementation choices. If incomplete, resume
the planner thread with the specific gap.

### Phase 2 — implementation

Drive Codex implementation workers through the plan:

- Parallel workers for independent units, each in an isolated worktree branched from
  updated `main`; serialize only real dependencies (e.g. the L1 units share one design).
- **One atomic commit per member issue**, commit message referencing the issue number.
  Bisectability is non-negotiable: every commit must leave `nix flake check` green.
  Where a fix invalidates an existing test that encoded the old, skewed behavior, updating
  that test to the intended contract is part of the same atomic commit.
- The test-hygiene issues #419 and #440 are in scope (they change existing tests). Beyond
  that, do not commission new acceptance or end-to-end suites in this session — the scope
  here is the production-code desired state; verification beyond keeping `nix flake check`
  green per commit is someone else's deliverable.
- Integration of completed worker branches into the single delivery branch is itself a
  Codex-owned task (per the skill), not something you hand-edit.

### Phase 3 — verify and deliver

1. Assemble everything onto one branch: `agent/desired-state`, based on updated `main`,
   containing the full ordered sequence of atomic commits.
2. Run the full `nix flake check` on the final branch head; all targets green.
3. Push `agent/desired-state` to origin. Do not open a PR.

## Deliverable and done-criteria

Done means all of the following are true and reported to the user:

- PR #449 merged; #441 closed.
- Probe #403 verdict recorded (`REHYDRATES` or `FRESH-START`) and reflected in the L1 design.
- Branch `agent/desired-state` pushed, green under full `nix flake check`, containing one
  atomic commit per remaining open issue (board minus #410 and #441), each commit
  bisectable.
- A final report: the issue → commit mapping, the canonical-contract decision for each of
  the five boundary pairs, the recorded decisions for #415 and #426, and any issue the
  planner concluded requires user input rather than code (state exactly why).

Report faithfully: failed checks are reported as failures with output, not smoothed over.

---

# APPENDIX — Handoff (written 2026-08-09, campaign complete)

## Outcome

The mission above is **done**. Delivered on 2026-08-09 after a ~19-hour run:

- PR **#449** un-drafted and squash-merged (repo convention: squash; main history is
  linear). Issue **#441** auto-closed. `main` = `4a99f64` (unmoved for the whole
  campaign; re-verify with `git fetch` before building on this handoff).
- Branch **`agent/desired-state` pushed to origin** (no PR opened, per mission): 16
  atomic commits on `4a99f64`, one per member issue, **every commit individually green**
  under full `nix flake check -L`, bisectable, final head gate green.
- Head: `9c800ef`. Full order (oldest first):
  `996b09c` #419, `2d5f6be` #444, `497c60a` #447, `82ef56b` #445, `992500b` #426,
  `a1e556e` #415, `63d5918` #446, `6d61864` #448, `b615b10` #443, `5f5792b` #439,
  `c6ba9ec` #440, `b221a5d` #442, `a714ffa` #403, `528ea12` #402, `4a79855` #408,
  `9c800ef` #409.

## Probe #403 (do not re-run)

Ran 2026-08-08. Verdict **REHYDRATES**: resumed thread's first `turn.completed.usage`
(input_tokens=32834) already contained the fresh run's cumulative (16204). Raw JSONL:
`/home/tom/mecattaf/tally-codex-runs/probe-403/{fresh,resumed}-20260808T092702.jsonl`.
Reduced fixtures are committed at `test/fixtures/usage/codex-resume-{fresh,cumulative}.jsonl`;
the arithmetic (fresh 16,209; delta 16,636; combined 32,845; forbidden sum 49,054) is
pinned as test assertions in `crates/tally-core/src/usage.rs`.

## Canonical-contract decisions (now implemented, binding for future work)

1. Campaign manifest grammar: `tally_core::campaign_contract` (Rust) is the single
   parser/defaulter/digest; Python consumes `CanonicalCampaignGraphV1` only.
2. Adapter argv: typed Rust invocation contract is canonical (`AdapterLaunchConfig`
   workload-head policy; `AdapterJobOptions` model overrides; `CampaignHost` argv prefix).
3. Git-AI attributes: seven-name bounded vocabulary (#441 fix, already in main).
4. Campaign registry: schema-2 authority frozen to the N−1 field set; host tuning in a
   versioned sidecar under `campaigns/extensions/`; one-shot quarantined migration.
5. Store assets: registration lifecycle owns GC leases — indirect roots + immutable
   snapshots, assets published before authority.

Recorded policy decisions: **#415** filtered views describe the visible view (explicit
`--flow-run` annotates archived rows instead of hiding; standup aggregates recomputed
over retained tasks; `QUERY_PROTOCOL_VERSION` 4→5). **#426** implemented exit 4 +
`coverage=zero-covered` now, precedence 1 > 3 > 4 > 0, no manufactured consumer.

## Board state after this campaign

- Closed by this campaign's merge of #449: #441.
- Implemented on `agent/desired-state` (close when that branch merges): #402 #403 #408
  #409 #415 #419 #426 #439 #440 #442 #443 #444 #445 #446 #447 #448.
- Still open, intentionally: **#410** (meta umbrella — board owner closes it manually).
- Nothing was commented, and no PR exists for `agent/desired-state`.

## Notable facts a successor should know

- Integration repaired three defects **inside their owning commits** (bisectability
  preserved): a stale CLI-local `registry_dir` reference in rebased U446 (now
  `CampaignRegistry::registrations()`); a lifecycle-publication race in U403's tests
  (bounded positive-progress backstop); a Home Manager user-unit activation race in the
  `flow-multi-host` VM harness (explicit user-unit reload + daemon start), fixed in U402.
- #419's soak evidence: 1,041 aggregate `tally-core --lib` runs, three concurrent
  suites, zero failures; soaked binary SHA-matched the committed diff. The first soak
  wave was discarded because unrelated host load (parallel worker builds) produced one
  37 ms miss on a semantic latency test — measurement condition is the concurrent suites
  themselves.
- Re-gate protocol used (user directive, keep for future train rebases): after any
  rebase, check out each rebased commit into its own worktree and run full
  `nix flake check -L` in ~6 parallel lanes; only the train-head gate is serial.
  One full gate ≈ 23 min on this host when warm.
- `latest.md` in the main working tree is a stale Aug 5 close-out (untracked, ignored).

## Artifacts and cleanup

- Worker ledger (task → worktree → branch → thread ID → PID → logs):
  `/home/tom/mecattaf/tally-codex-runs/desired-state/LEDGER.md`. All Codex JSONL
  transcripts and task prompts live in per-worker subdirectories next to it. The plan of
  record: `/home/tom/mecattaf/tally-codex-runs/desired-state/planner/PLAN.md`.
- Campaign worktrees still present (branches intact, safe to remove with
  `git worktree remove` once `agent/desired-state` merges):
  `ds-planner ds-u419 ds-track-a ds-track-b ds-track-c ds-u415 ds-u426 ds-u440
  ds-u442 ds-l1 ds-integration` under `/home/tom/mecattaf/tally-worktrees/`.
  Pre-existing `w6-*` worktrees were never touched.
- All Codex worker processes have exited. Foreign codex sessions unrelated to this
  campaign were observed running on the host during the campaign (user's other task) —
  they were never touched; do not `pkill codex` broadly, ever.
- The main working tree `/home/tom/mecattaf/tally.nix` was never modified; it still sits
  on branch `agent/fix-campaign-git-ai-validation` (now merged into main and safe to
  switch or delete).

## Verify-from-scratch recipe

```bash
git fetch origin
git log --oneline origin/main..origin/agent/desired-state   # 16 commits, one per issue
# per-commit bisectability (expensive): for each SHA, worktree-add + nix flake check -L
```
