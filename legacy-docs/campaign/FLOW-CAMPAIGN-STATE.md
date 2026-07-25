# FLOW-CAMPAIGN-STATE — the only memory the next session has

## ⛳ CAMPAIGN COMPLETE (2026-07-25). Stop condition met: CP-B posted its GREEN seal on #52.
All 7 FS units MERGED (main=e7ae081) + CP-A GREEN + CP-B GREEN. MORNING-REPORT.md written at
repo root. Nothing deployed (Tom's morning act). No PENDING-TOM. 9 ORACLE-DELTAS accrued. If
you are a fresh session reading this: the campaign is DONE — do not re-dispatch anything. Read
MORNING-REPORT.md.


Campaign started 2026-07-24 (overnight). Orchestrator = Claude (coordinator). Codex writes
all impl code via tally dispatch. Frozen spec baseline: `main = f77ea8a` (04a6149 corpus +
28-resolution ambiguity splice). Feature-completeness law in force: no MVP tier, a unit is
done only when it implements its spec to the last exception path.

## Lane table + frontier

| Lane | Pool | Sequence | Frontier |
|---|---|---|---|
| A | `build` | ~~FS-1~~ → ~~FS-2~~ → ~~FS-3~~ → ~~FS-6~~ | **LANE A COMPLETE** (all 4 merged) |
| B | `coordinator-gpu` | ~~FS-4~~ → ~~FS-5~~ → ~~FS-7~~ | **LANE B COMPLETE** (all merged) |
| CP | `--detach` | ~~CP-A~~ **GREEN** · **CP-B (#52) running** (the seal) | CP-B is the last unit; campaign ends when it posts |

Dependency notes:
- FS-5 requires FS-1 + FS-3 + FS-4 merged (rebase worktree on fresh main before dispatch).
- CP-A after FS-5 merges; may overlap lane A's FS-6.
- CP-B last, after everything merges. Red ⇒ fix session `<unit>-fix1` in offending lane.

## Unit ledger

| Unit | Issue | Dedup key | Worktree | Status | PR/commit | Result |
|---|---|---|---|---|---|---|
| FS-1 | #44 | fs-1-attach | (removed) | **MERGED** | PR#54 / 7dc10b1 | GREEN, issue closed |
| FS-2 | #45 | fs-2-provenance-brief | (removed) | **MERGED** | PR#56 / dbbd351 | GREEN, issue closed |
| FS-3 | #46 | fs-3-concurrent-wire | (removed) | **MERGED** | PR#57 / 5f6ed4e | GREEN, issue closed, 3 ORACLE-DELTAS |
| FS-4 | #47 | fs-4-flow-crate | (removed) | **MERGED** | PR#55 / a6a95e0 | GREEN, issue closed |
| FS-5 | #48 | fs-5-replay-integration | (removed) | **MERGED** | PR#59 / 54b5abe | GREEN, closed, fixed FS-4 canon bug, 2 ORACLE-DELTAS |
| FS-6 | #49 | fs-6-semantic-truth | (removed) | **MERGED** | PR#58 / d5d99c2 | GREEN, closed, 4 ORACLE-DELTAS, byte-compat OK |
| FS-7 | #50 | fs-7-nix-flows | (removed) | **MERGED** | PR#60 / e7ae081 | GREEN, closed, 7 checks added (14→21), multi-host VM, 4 ORACLE-DELTAS |
| CP-A | #51 | cp-a-flow-live | (removed) | **GREEN** | comment on #51 | 6/6 live assertions, witness 40/40, cleanup OK |
| CP-B | #52 | cp-b-seal | (removed) | **GREEN — SEAL** | comment on #52 | 360 tests, forced multi-host VM rebuild, calendar-restart reconstruct, witness GREEN/RED |

## OPERATING RULES (Tom, 2026-07-24 — binding on every session)
1. **No in-flight dedup exists yet** (FS-1 is building it). Re-enqueuing a dedup key whose
   codex session is still running DOUBLE-LAUNCHES it. After any restart/compaction, before
   ANY re-dispatch: check `tally query status` + `ps` for a live session on that key.
   NEVER re-enqueue on suspicion.
2. **Window death ≠ failure.** A session that dies mid-unit leaves its commits in the
   worktree. Re-dispatch same unit with dedup `<key>-r2` and PREPEND to the prompt:
   "A prior session left partial work in this worktree. Run git log/status/diff first and
   continue to completion — do not restart from scratch, do not revert existing commits."
3. **rustc ≥ 1.91 required for Boa 0.21.1.** VERIFIED 1.96.1 OK. FS-4 caveat: boa.md brief
   cites main-branch line numbers — APIs must be verified against the 0.21.1 tag, not exact
   lines. (FS-4 already running WITHOUT this caveat in-prompt; apply via -r2 prefix only if
   it returns with boa API trouble — do not re-enqueue on suspicion.) MSRV fallback if ever
   broken = PENDING-TOM (bump nixpkgs / older boa / rquickjs — Tom's choice, not mine).
4. **Merge collision is MINE, not codex's.** FS-4 vs lane A collide on
   `crates/tally/src/main.rs` (subcommand wiring) and `Cargo.lock` (FS-4 adds boa_engine).
   Whichever lane merges SECOND: I do the mechanical rebase myself, keep both sides, re-run
   the full ladder before merging. No codex session for a trivial conflict.
5. **Spec frozen for codex too.** A session proposing to edit FLOW-SPEC/NIX-SPEC-FLOW to
   match its impl = BLOCKED → PENDING-TOM, never a doc commit. Only sanctioned spec-adjacent
   output = ORACLE-DELTAS obligations in FS-3's PR body.
6. **Stop-everything tripwires:** witness regression flip (valid GREEN/tampered RED) OR a
   byte-compat break on legacy hashes halts BOTH lanes until fixed. Nothing merges past a
   broken witness chain.
7. **CP-A adapter node stays cheap.** Heterogeneous-node assertion needs an adapter-path
   job, not an expensive one (trivial codex/pi or shell preset satisfies it). Keep
   `env -u TALLY_JOB_ID -u TALLY_SOCKET …` sanitization on EVERY dev-daemon-driving dispatch.
8. **Stagger simultaneous gate ladders.** Two concurrent `nix flake check` + `cargo test
   --workspace` on the coordinator slow both and muddy timing-sensitive tests. Worktrees
   have separate target dirs — contention is CPU, not files. Run them staggered.

## SCHEDULING NOTES (orchestrator decisions)
- **Lane B idles after FS-4 merges, until FS-3 merges.** FS-7 (#50) declares "Depends: FS-4
  merged" but its acceptance needs a live multi-host flow run (SSH executor + daemon-kill +
  replay) = FS-5 runner territory (FS-4 client seam is mocked). Handoff sequences FS-5→FS-7.
  Not pulling FS-7 forward (risk: incomplete unit). Not moving FS-3 to lane B (FS-2/FS-3
  share daemon.rs/wire.rs — serialized on lane A by design). Long pole = lane A:
  FS-2→FS-3→FS-6. FS-3 merge unblocks FS-5 (lane B) + enables FS-6 (lane A).
- **Rule-4 collision auto-resolved:** FS-4 rebased onto 7dc10b1 with NO conflict (FS-1's
  main.rs queue-retry vs FS-4's flow subcommand = disjoint clap regions; FS-1 untouched
  Cargo.lock). Verified both survived: core 273 + flow 22 tests green on merged tree.

## ENDGAME PLAN (after FS-5 merges)
- coordinator-gpu: dispatch FS-7 (#50, fs-7-nix-flows) — needs FS-5 merged; the 7 nix flake checks + multi-host VM.
- build (free, lane A done): dispatch CP-A (#51, cp-a-flow-live) IN PARALLEL — checkpoint only needs FS-5, not FS-7. --detach checkout tally.nix-cpa. MUST use `env -u TALLY_JOB_ID -u TALLY_SOCKET` on the codex exec (isolated dev daemon). Checkpoint may post EXACTLY ONE comment on #51.
- After BOTH FS-7 merged AND CP-A done → CP-B (#52, cp-b-seal) LAST, --detach, env-sanitized, one comment on #52. CP-B needs everything merged.
- Red checkpoint ⇒ fix session `<unit>-fix1` in offending lane before further merges.

## NOTABLE FINDINGS
- FS-5 found+fixed a latent FS-4 bug: singleton pool hashed `["alpha"]` vs kernel `"alpha"` →
  all attach/replay would falsely diverge on real deployments. Fixed in tally-flow/engine.rs +
  kernel-parity test + credential-pool hashing. Merged FS-4 was buggy in this path; FS-5 corrects it.

## PENDING-TOM
(none yet)

## ORACLE-DELTAS (accrued obligations for golden-oracle harness / morning report)
From FS-3 (#46, PR#57):
1. Assert one-connection multiplexing, response-ID correlation, unspecified cross-request
   response ordering, six blocked awaits + interleaved queries, 64-request FIFO overflow.
2. Assert default/configured frame limits both directions: exact accepted, limit+1 rejected
   by whichever peer observes it first, no negotiation.
3. Retain watch gap-free/duplicate-free cursor-resume + 48 KiB pagination oracles under
   concurrent serving.
From FS-6 (#49, PR#58) — LEGACY docs/NIX-SPEC.md diverges from frozen flow-era spec; Tom to reconcile:
4. NIX-SPEC.md §4 requires nonempty requiredGateIds + missing-manifest=failure; FLOW-SPEC §13
   requires empty preset defaults + absent-as-`not-run`. Implemented per FLOW-SPEC.
5. NIX-SPEC.md §5 lists only regex|jsonPath; FLOW-SPEC §13 requires new `jsonPathLast`. Implemented.
6. NIX-SPEC.md §2 silent on built-in-meter consumptionCap being token-denominated; live module docs now say so.
7. Pure-Nix hardening option rendering is FS-7-owned; FS-6 did only the Rust half.
From FS-7 (#50, PR#60) — spec-vs-spec reconciliations, resolved per amended flow-era spec:
8. Issue #50 body + NIX-SPEC-FLOW §4 opening say FS-7 ships catalog schema; amended §4 assigns
   it to FS-4. FS-7 consumes merged crate schema + adds goldens/checks.
9. FLOW-SPEC §11.5 mentions catalog path in runner env; amended NIX-SPEC-FLOW §4 requires
   `--catalog` and forbids `TALLY_FLOW_CATALOG`. Followed amended CLI contract.
10. NIX-SPEC-FLOW §1 defines budgetPool + existence assertion, but normative producer rendering
    fixes runner pool to `[ "flow" ]`. Validated budgetPool existence, no extra render channel.
11. NIX-SPEC-FLOW §§3/7: four rejection classes described but seven top-level checks required;
    reconciled by folding undeclared-pool + Nix closure failures under `flow-pool-closure`.
(Note: these are spec-vs-spec reconciliations resolved in favor of the flow-era authority — NOT
build blockers. No PENDING-TOM. Flagged for morning report.)

## Event log
- 2026-07-24: campaign start. main=f77ea8a, both pools GO/idle. Created state. Dispatching FS-1 + FS-4.
- 2026-07-24: worktrees tally.nix-fs1 (fs-1-attach) + tally.nix-fs4 (fs-4-flow-crate) created at f77ea8a. FS-1 dispatched on `build` (bg bl5md05e7, cap 14400), FS-4 on `coordinator-gpu` (bg b3o1fjg0f, cap 21600). Both leases HELD, pools at capacity (STOP). Awaiting codex completion notifications.
- 2026-07-24: Tom's 8 operating rules recorded above (binding). FS-4 pre-flight: `nix develop -c rustc --version` = 1.96.1 ≥ 1.91 → Boa 0.21.1 MSRV OK, no PENDING-TOM. WATCH: FS-4 running without boa-0.21.1-tag caveat in-prompt (rule 3).
- 2026-07-25 04:11Z: CP-B (#52) GREEN — THE SEAL. CAMPAIGN COMPLETE. env-sanitized isolated seal on e7ae081: 360 tests, clippy 0, all 21 flake checks (FORCED flow-multi-host VM rebuild 31.65s — SSH child/coordinator-SIGKILL/restart-readopt/replay-attached/local-child/Git-handoff/witness), calendar flow reconstructed across daemon restart (parent + ordinals [0,1,2] + 4 proofs + witness seq 4), witness GREEN(0)/RED(1), legacy FS-1 regression + no-stubs GREEN. No deploy, no prod daemon touched. Isolated root cleaned. 1 comment on #52. Disclosed harmless preflight --help notation deviation. Detached worktree removed. MORNING-REPORT.md written. All pools idle/GO. Stop condition met → done.
- 2026-07-25 03:xxZ: FS-7 (#50) SEALED — LAST CODE MERGE. Codex e7ae081 DONE; at main tip (no rebase); independent ladder green: cargo test core 298 + flow 22 + CLI 24 + catalog golden, clippy 0, witness GREEN(0)/RED(1), no-stubs GREEN. Flake check-set verified via `nix eval .#checks` = 14→21, EXACTLY 7 flow checks added (flow-dialect-accept, 3× flow-dialect-reject-*, flow-pool-closure, flow-catalog-schema, flow-multi-host), ZERO removed. flow-multi-host VM passed: remote SSH child → coordinator SIGKILL → restart/re-adopt → replay-attach → local child → Git-branch artifact handoff → witness verify. 4 ORACLE-DELTAS. Pushed, PR#60, ff-merged, MERGED + #50 CLOSED. main=e7ae081. **ALL 7 FS UNITS + CP-A DONE.** Dispatched CP-B (#52) THE SEAL on `build` (bg bmj13yrrb, env -u sanitized, --detach e7ae081). Campaign ends when CP-B posts its comment → then write MORNING-REPORT.md.
- 2026-07-25 02:xxZ: CP-A (#51) GREEN. All 6 live assertions passed on isolated env-sanitized dev daemon: heterogeneous 6-node (shell+cheap adapter), SIGKILL replay reused×3→created×3, daemon-restart re-await, replay-divergence exit20, 2 concurrent runners created×6/attached×6, scheduler fairness braid A0,B0,A1,B1... Witness chain 40/40. Cleanup verified (daemon/socket/root gone, checkout clean). Sole mutation = 1 comment on #51. Detached worktree removed. No fix session needed. CP-B still gated on FS-7 merge.
- 2026-07-25 02:xxZ: FS-5 (#48) SEALED. Codex 76367a5 DONE; rebased onto d5d99c2 → 54b5abe (CLEAN despite both FS-5+FS-6 touching daemon.rs/main.rs — disjoint regions). Independent ladder on merged tree green: cargo test core 298 + flow 22 + CLI 24 incl. fs5_live_acceptance_matrix, clippy 0, witness GREEN(0)/RED(1) legacy hashes UNCHANGED (rule-6 clear), no-stubs GREEN. Flake: settled 66-vs-69 confusion DEFINITIVELY via `nix eval .#checks` — FS-5 tree and main BOTH 14 checks, IDENTICAL set (running-N is cache-dependent sub-derivation count, non-signal); all passed. FS-5 FIXED latent FS-4 canon bug (see NOTABLE FINDINGS). Pushed, PR#59, ff-merged, MERGED + #48 CLOSED. main=54b5abe. ENDGAME LAUNCHED: FS-7 (#50) on coordinator-gpu (bg bqqdliikl) + CP-A (#51) on build (bg b7fx334k8, env -u TALLY_JOB_ID/SOCKET) IN PARALLEL. worker-gpu also held (FS-7 multi-host VM offload). CP-B after both.
- 2026-07-25 01:xxZ: FS-6 (#49) SEALED. Codex d5d99c2 DONE; at main tip (no rebase); independent ladder green: cargo test core 298 + flow 22, clippy 0, nix flake check 69 all-passed (FS-6 added jsonPathLast enum + preset checks, additive 67→69), witness VALID GREEN(0)/TAMPERED RED(1) with LEGACY FIXTURE HASHES UNCHANGED (rule-6 byte-compat tripwire clear), no-stubs GREEN. Handled owner-amended scope + spec-vs-spec conflict correctly (implemented per FLOW-SPEC §13, recorded 4 ORACLE-DELTAS, edited NO frozen spec). Pushed, PR#58, ff-merged, MERGED + #49 CLOSED. main=d5d99c2. **LANE A COMPLETE.** Only FS-5 active (lane B, worktree still at 5f6ed4e — MUST rebase onto d5d99c2 on completion; FS-6 touched daemon.rs/evidence/nix so real conflict possible — mine to resolve).
- 2026-07-25 00:4xZ: FS-3 (#46) SEALED. Codex 5f6ed4e DONE; already at main tip (no rebase); independent ladder green: cargo test core 291 + flow 22, clippy 0, nix flake check all-passed (66-vs-67 in codex log was eval-CACHE display, not a dropped check — FS-3's flake/nix diff is EMPTY; my run showed "running 0 checks / all passed" confirming set unchanged), witness GREEN(0)/RED(1), no-stubs GREEN. 3 ORACLE-DELTAS in PR#57 body + recorded above. Pushed, PR#57 (no 500), ff-merged, MERGED + #46 CLOSED. main=5f6ed4e. PIVOT REACHED: FS-1+FS-3+FS-4 merged → FS-5 unblocked. Dispatched FS-5 on `coordinator-gpu` (bg bvyziourc, cap 14400) + FS-6 on `build` (bg blqetphp4, cap 14400), both from 5f6ed4e. BOTH LANES ACTIVE again. On completion: STAGGER their gate ladders (rule 8). CP-A (#51) becomes eligible once FS-5 merges.
- 2026-07-25 00:11Z: FS-2 (#45) SEALED. Codex d720a83 DONE; rebased onto a6a95e0 → dbbd351 (NO conflict); independent ladder on merged tree green: cargo test core 283 + flow 22 + all suites, clippy 0, nix flake check 67 all-passed, witness GREEN(0)/RED(1), no-stubs GREEN. Pushed, PR#56 (no 500), ff-merged, MERGED + #45 CLOSED (GitHub merge-detection lagged ~seconds after push), worktree+branch removed. main=dbbd351. FS-3 (#46) dispatched on `build` (bg b7kqjee9w, cap 14400) from fresh main — its merge unblocks FS-5 (lane B) + enables FS-6 (lane A). Lane B still idle.
- 2026-07-24 23:39Z: FS-4 (#47) SEALED. Codex commit 1e87923 DONE; orchestrator rebased onto merged main 7dc10b1 → a6a95e0 (NO conflict — rule-4 collision auto-resolved), independently re-ran full ladder on rebased tree: cargo test core 273 + flow 22 + all suites exit 0, clippy -D warnings 0, nix flake check all-passed (incl. stock-host VM activation on worker-tb), witness VALID GREEN(0)/TAMPERED RED(1), no-stubs GREEN. 8 named acceptance tests green. Pushed fs-4-flow-crate, PR#55 (no 500), ff-merged, PR MERGED + #47 CLOSED, worktree+branch removed. main=a6a95e0. Lane B now IDLE (see scheduling notes) — coordinator-gpu GO. NOTE: FS-2 branched from 7dc10b1 (pre-FS-4); on FS-2 completion I rebase it onto current main (a6a95e0) and resolve any main.rs/Cargo.* collision myself before merge.
- 2026-07-24 22:53Z: FS-1 (#44) SEALED. Codex commit 7dc10b1 DONE; orchestrator independently re-ran full ladder on frozen tree — cargo test 273/273 core + all suites exit 0, clippy -D warnings exit 0, nix flake check all-passed, witness VALID GREEN(0)/TAMPERED RED(1), no-stubs GREEN. Adversarial diff vs #44: 6 acceptance tests map 1:1 to disposition rows + legacy regression + canonical payload; witness byte-compat proven. Pushed fs-1-attach, PR#54 created (no 500), ff-merged to main, PR MERGED + #44 CLOSED, worktree+branch removed. main=7dc10b1. NOTE(rule 4): FS-1 changed crates/tally/src/main.rs +22 — FS-4 collides here on second merge (mine to rebase). FS-2 (#45) dispatched on `build` (bg besjspkn2, cap 14400) from fresh main.
