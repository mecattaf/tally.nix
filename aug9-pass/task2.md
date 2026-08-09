# Mission: author the final conformance bar for tally.nix

You are a Claude Fable orchestrator session. Your job is to produce the **definitive test
suite** for the repository `/home/tom/mecattaf/tally.nix`: the executable statement of what
this system does when it is fully correct — written **as if tally.nix already conforms to
its desired state**. You do this by invoking the `steer-codex` skill and operating as the
control plane for autonomous Codex CLI workers, per that skill's contract: Codex owns each
task from investigation through delivery; you launch, preserve, observe, resume, and verify
outcomes. You do not write the tests yourself and you do not prescribe implementations to
workers.

Run until completion. Do not stop at a spec document or a partial suite. The session ends
only when the deliverable described at the bottom exists and is pushed, or you hit a
genuine blocker that only the user can resolve.

## The one rule that defines this mission

**The bar is written from intended behavior, never derived from current code behavior.**
Workers read the code to learn interfaces, entry points, file formats, and harness wiring —
but every expected value and every asserted behavior comes from the issues' intended
semantics, repository doctrine, and recorded real external-tool behavior. Observing what
the current implementation outputs and asserting that back is forbidden: it would re-import
the very defects this suite exists to catch. Where current behavior and intended behavior
differ, the test asserts intended behavior, and its failure against current HEAD is
expected and is the point.

## Ground truth

- Repo: `/home/tom/mecattaf/tally.nix`. Default branch `main`. No GitHub Actions; the gate
  is local `nix flake check`.
- The file `latest.md` (untracked) is a stale Aug 5 close-out — not authoritative. Issue
  **#410** carries the doctrine: pre-merge evals on held branches, re-eval on mechanism
  touch, **mutation-tested assertions**. That doctrine is non-negotiable here.
- 18 open issues define the gap between current and desired state: #402 #403 #408 #409
  #410 #415 #419 #426 #439 #440 #441 #442 #443 #444 #445 #446 #447 #448. Read every one in
  full. Each non-meta issue's resolution is a behavior this suite must pin.

### Standing diagnosis (established, do not relitigate)

The system's defects are almost all **cross-boundary contract skews**: modules are locally
rigorous, but contracts disagree across boundaries, and nothing in the existing checks
exercises a boundary end-to-end. Five boundary pairs generate most of the board:

1. Rust arm CLI ↔ Python `spec_build_driver.py` — one manifest grammar, two independent
   parsers (#444, #446, #439)
2. Nix preset/module ↔ Rust adapter argv (#442, #443, #445)
3. Attribute producer ↔ attribute validator (#441)
4. Registry schema vN ↔ vN−1 reader on rollback (#447)
5. Store-path producer ↔ Nix garbage collection (#448)

The suite's highest-value artifacts are the ones that make skew at these pairs impossible
to reintroduce silently — shared contracts exercised on both sides, and end-to-end paths
nothing currently walks.

### Existing verification surface (what the bar extends, not duplicates)

- ~45 `nix flake check` targets; four VM tests: `stockHostTest`, `systemSocketExecutionTest`,
  `retentionTest`, `flowMultiHostTest`.
- Python driver tests in `test/`: `spec_build_driver_test.py`,
  `spec_build_conflict_domains_test.py`, `spec_build_two_repo_test.py`,
  `spec_build_checkpoint_receipts_test.py`.
- **Known gap:** no existing check takes a campaign past reconcile.
  `test/campaign-github-e2e.sh` and `campaign-nixos-activation` both stop short. Everything
  downstream of reconcile — dispatch, execute, sweep, digest — is unpinned.

## Operating mode

- Invoke the `steer-codex` skill first and follow it exactly: detached `codex exec` with
  full-access flags, stdin prompts, JSONL logs, recorded thread IDs, resume by thread ID.
  You have standing authority for full-capability Codex orchestration for this entire
  mission — do not re-ask per worker.
- Run root for all worker artifacts: `/home/tom/mecattaf/tally-codex-runs/final-bar/`
  (create it; one subdirectory per worker).
- Every concurrent writer gets its own git worktree and branch. Never run writers directly
  in `/home/tom/mecattaf/tally.nix`'s main working tree — the user may have other terminals
  open on this repository.
- Do **not** comment on GitHub issues, do not open PRs, do not merge to `main`, and do not
  modify any production code. This session's writes are tests, fixtures, and harnesses only.

## Phases

### Phase 0 — empirical inputs

1. Run the date-gated probe `/home/tom/mecattaf/tally-codex-runs/probe-403/probe.sh`. It
   prints `REHYDRATES` or `FRESH-START` and settles whether a resumed `codex exec` reports
   thread-cumulative usage (#403). The verdict determines the *intended* usage-accounting
   semantics the suite asserts for the rollup issues (#402/#403/#408/#409). Record it
   before any rollup test is specified.
2. Where a boundary involves a real external tool's grammar (e.g. real CLI argv/JSON
   behavior for adapters), intended behavior is grounded in the real tool's recorded
   behavior — capture what is needed as fixtures.

### Phase 1 — the specification

Launch **one** Codex specification worker whose task is: from the full issue texts, #410's
doctrine, repository documentation, and the empirical inputs above, write the desired-state
behavior specification the suite will encode. It must:

- state, for every non-meta open issue, the intended behavior once the issue is resolved —
  as observable, assertable behavior at a public boundary;
- state one canonical contract per boundary pair, with both sides' conformance obligations;
- **decide** open design questions where the issue leaves the desired state undecided —
  #415 (what a filtered view's aggregates mean) and #426 (semantics of the
  "declared surfaces accounted for, but nothing covered" exit code) — a decision with
  rationale recorded in the spec, not a punt;
- for the rollup/evidence issues, define one coherent evidence model consistent with the
  probe verdict.

You review the spec only for completeness (every issue mapped to assertable behavior, every
decision made) — not to second-guess its design choices.

### Phase 2 — build the suite

Drive Codex workers to implement the bar from the spec. Required shape:

- **Black-box at public boundaries.** Assert at surfaces that survive refactors: CLI argv
  and exit codes, manifest files, registry files on disk, schema documents, driver
  stdin/stdout, store paths, rendered service commands. Avoid coupling to internal APIs —
  the bar must remain valid whatever shape the conforming implementation takes.
- **Conformance corpora for boundary pairs 1 and 2**: checked-in fixture sets that both
  sides of each pair must agree on (manifest bodies → parsed value + digest for the
  arm/driver pair; preset argv → rendered command validated against recorded real-CLI
  grammar for the adapter pair), with a runner that feeds each fixture through both sides
  and diffs.
- **One full-pipeline end-to-end check**: arm → poll → reconcile → dispatch → execute →
  sweep → digest against the packaged driver, so every unlit downstream section fails in
  one run instead of serially.
- **Per-issue conformance tests** for the remainder (registry rollback #447, GC-rooting
  #448, validator/producer agreement #441, rollup evidence semantics per the spec, the
  #419/#440 flake and seam-binding hygiene assertions, the #415 and #426 decided semantics).
- **One runnable entry point** that can execute the whole bar against an arbitrary
  tally.nix working tree passed as a parameter (plus wiring to run it via the flake).
  The suite must build and run on current HEAD — individual assertions failing is expected;
  the harness erroring out is not.

### Phase 3 — validate the bar itself

The bar must be able to bite. Per #410 doctrine, mutation-test it:

1. Run the full suite against current `main` HEAD. Record the failure matrix.
2. Every test tied to an open issue is expected to **fail** on current HEAD. Any such test
   that passes is suspect: either the issue is already fixed on HEAD (verify and note it)
   or the test does not actually assert the intended behavior (the worker must fix the
   test). Chase every suspect to one of those two conclusions.
3. Spot-mutate: for a sample of assertions, verify a deliberately wrong behavior is caught.

## Deliverable and done-criteria

Done means all of the following are true and reported to the user:

- Branch `agent/final-bar` pushed to origin (no PR), based on `main`, containing the spec
  document, the full suite, fixtures/corpora, and the parameterized entry point. No
  production code touched.
- Probe #403 verdict recorded and reflected in the rollup assertions.
- A final report: the test → issue mapping, the recorded decisions for #415 and #426, the
  failure matrix against current HEAD with each failure traced to its issue, every
  passing-but-tied-to-open-issue test resolved to "already fixed on HEAD" or "test
  repaired", and the mutation spot-check results.

Report faithfully: a harness that doesn't run, or a test that can't fail, is a defect in
this deliverable — say so plainly rather than smoothing it over.

---

# Appendix: mission worked to completion (handoff record)

**Status: COMPLETE, 2026-08-08.** All phases executed; the deliverable exists and is
pushed. Nothing in this file remains to be done. This appendix is the continuation
handoff: everything a future session needs is recorded here and in the artifacts below.

## What exists now

- **Branch `agent/final-bar` pushed to origin** (no PR), based on `main` @ `4a99f64`,
  six commits: `c590536` (spec) → `189d0e4`, `3a83e4d`, `5eba2ae` (suite) → `247d3d4`
  (suspect-pass repairs) → `c728aa2` (validation record). Local worktree:
  `/home/tom/mecattaf/tally-worktrees/final-bar`.
- **Spec:** `doc/final-conformance-bar.md` — 17 non-meta issues mapped to assertable
  public-boundary behavior; five canonical boundary-pair contracts (§3); rollup evidence
  model (§4); decisions in §10; issue→assertion→mutation ledger in §9.
- **Suite:** `test/final-bar/` — 26 black-box cases, manifest corpus (arm/driver),
  adapter argv corpus (grounded in recorded real Codex/Pi CLI behavior), hermetic
  full-pipeline fixtures, usage evidence corpus. Entry point:
  `test/final-bar/run /path/to/tally.nix-tree`, or
  `nix run .#final-conformance-bar -- <tree>`; flake check `final-conformance-bar-harness`.
- **Validation record:** `doc/final-conformance-bar-validation.md` — full failure
  matrix, suspect-pass resolutions, mutation results.
- No production code touched; only `flake.nix` changed outside `test/final-bar/` and
  `doc/`, and that diff is additive wiring (package + app + check).

## Key verdicts and decisions (binding for future work)

- **Probe #403: REHYDRATES.** Resumed `codex exec` usage is thread-cumulative (resumed
  first reading 32,117 ⊇ fresh cumulative 16,050). Rollups must charge resumed attempts
  by verified lineage deltas; missing baselines are caveated, never guessed. Record:
  `/home/tom/mecattaf/tally-codex-runs/final-bar/probe-403-verdict.md`; raw JSONL in
  `tally-codex-runs/probe-403/` (`*-20260808T092733.jsonl`).
- **#415:** filtering defines the view — aggregates describe visible rows (explicit
  hidden counts retained); explicit run-identity lookups still return archived rows.
- **#426:** accounted-but-zero-covered exits `4` with `verification=none`; any direct
  coverage exits `0`.
- Other recorded decisions (#439, #444, #447, #448, rollup model): spec §10.

## Validation outcome (the bar bites)

- Definitive run on HEAD `4a99f64`: **3 PASS / 23 FAIL / 0 harness errors** — all 23
  failures are expected desired-state failures, each traced to its issue in the
  validation doc. The #419 wave caught a real 1-in-15 `Elapsed(())` failure.
- All nine originally-passing cases resolved: 3 "already fixed on HEAD" (#441 both
  cases via `4a99f64`; one #440 restarted-capture path), 6 "test repaired" (`247d3d4`).
  The #440 audit exposed and fixed a genuine spec defect (public lookup rehydrates
  `session_cwd`, masking deleted producer bindings; spec now also requires focused
  pre-fallback regressions).
- Mutation spot-checks 3/3 caught; no missed mutations, no mutation residue.

## Orchestration record

- Single Codex thread across all phases: `019fe047-cb23-7fa3-b1ec-cc01ca4f5e08`
  (resumable via `codex exec resume` from the worktree). Run root:
  `/home/tom/mecattaf/tally-codex-runs/final-bar/` (prompts, JSONL logs, worker
  registry in `workers.md`).
- Caveat for successors: one worker authored spec + suite + validation, so the bar is
  one coherent worldview. Disagreements with a §10 decision should be changed in the
  spec first — the tests encode it.
- Environment note: codex 0.145 requires cwd inside a git repo; the probe dir was
  `git init`-ed to run the probe with the exact preset argvs.

## Natural next steps (not part of this mission)

- Drive fixes issue-by-issue until the bar goes green; each of the 23 failures names
  its issue and intended behavior.
- On any mechanism touch, re-run the bar per #410 doctrine (held branch, re-eval,
  mutation discipline).
