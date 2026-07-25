# MORNING REPORT — tally-flow campaign, night of 2026-07-24 → 25

**Verdict: COMPLETE and GREEN.** All seven FS implementation units and both checkpoints
landed. `main` advanced `f77ea8a → e7ae081` as seven clean feature commits. Both checkpoints
sealed GREEN, including a forced (non-cached) coordinator+worker multi-host VM proof. No
PENDING-TOM blockers arose. Nothing was deployed — deployment is your call (see below).

## Campaign table

| Unit | Issue | PR | Commit | Result |
|---|---|---|---|---|
| FS-1 submission idempotency | #44 | #54 | `7dc10b1` | GREEN — full disposition table, canonical payload, attach |
| FS-4 tally-flow crate (Boa) | #47 | #55 | `a6a95e0` | GREEN — engine/dialect/host-API/replay; rebased clean onto FS-1 |
| FS-2 provenance + counters | #45 | #56 | `dbbd351` | GREEN — briefs, fanout counters, durable bookkeeping |
| FS-3 concurrent RPC + fairness | #46 | #57 | `5f6ed4e` | GREEN — 64 in-flight, 16 MiB frames, aging braid; +3 ORACLE-DELTAS |
| FS-6 semantic truth | #49 | #58 | `d5d99c2` | GREEN — manifests, final-message, trailers, meter; byte-compat held; +4 ORACLE-DELTAS |
| FS-5 runner ⇄ kernel replay | #48 | #59 | `54b5abe` | GREEN — live binding, runner-as-job; **fixed a latent FS-4 bug**; +2 ORACLE-DELTAS |
| FS-7 Nix flow surface | #50 | #60 | `e7ae081` | GREEN — flows option tree, 7 new checks (14→21), multi-host VM; +4 ORACLE-DELTAS |

Merge order was FS-1 → FS-4 → FS-2 → FS-3 → FS-6 → FS-5 → FS-7 (units merged as they
finished + verified). Every unit was independently re-gated by the orchestrator on the merged
tree (not just trusted from codex's report): `cargo test --workspace`, `clippy -D warnings`,
`nix flake check`, witness valid-GREEN/tampered-RED, no-stubs. Every inter-lane rebase
resolved cleanly (verified by both crates' test counts coexisting on the merged tree).

## Checkpoint outcomes

- **CP-A (#51) — GREEN.** Flow liveness on an isolated env-sanitized dev daemon: 6-node
  heterogeneous flow (shell + cheap adapter), SIGKILL replay `reused×3→created×3`,
  daemon-restart re-await, replay-divergence exit-20, two concurrent runners
  `created×6`/`attached×6`, scheduler fairness braid. Witness chain 40/40. Report comment on #51.
- **CP-B (#52) — GREEN, THE SEAL.** 360 tests + clippy clean; all 21 flake checks passed with
  a **forced** `flow-multi-host` VM rebuild (31.65s — remote SSH child, coordinator SIGKILL,
  restart/re-adopt, replay-`attached`, local child, Git-branch artifact handoff, witness
  verify); calendar-triggered flow reconstructed across a real daemon restart (one parent,
  ordinals [0,1,2], 4 verified proofs); witness GREEN/RED; legacy FS-1 regression + no-stubs.
  Report comment on #52. (Note: CP-B disclosed a harmless preflight `--help` env-notation
  deviation in how it framed one command — no effect on the sealed gates.)

Issues #51/#52 are left OPEN carrying their GREEN report comments — read them, then close.

## PENDING-TOM

None. No codex session hit a genuine spec gap; the 28-resolution ambiguity splice answered
everything. The only judgment calls were spec-vs-spec reconciliations, all resolved in favor
of the frozen flow-era authority and recorded as ORACLE-DELTAS (below) — none required a stop.

## ORACLE-DELTAS (obligations for the golden-oracle harness + spec reconciliation)

**From FS-3 (#57)** — assertions the oracle must carry:
1. One-connection multiplexing, response-ID correlation, unspecified cross-request ordering,
   six blocked awaits + interleaved queries, 64-request FIFO overflow window.
2. Default/configured frame limits both directions: exact accepted, limit+1 rejected by
   whichever peer observes first, no negotiation.
3. Watch gap-free/duplicate-free cursor-resume + 48 KiB pagination oracles under concurrency.

**From FS-6 (#58) + FS-7 (#60)** — LEGACY `docs/NIX-SPEC.md` diverges from the frozen flow-era
spec; **these need your reconciliation** (implementation followed FLOW-SPEC / amended NIX-SPEC-FLOW):
4. `NIX-SPEC.md §4` requires nonempty `requiredGateIds` + missing-manifest=failure; FLOW-SPEC
   §13 requires empty preset defaults + absent-as-`not-run`.
5. `NIX-SPEC.md §5` lists only `regex | jsonPath`; FLOW-SPEC §13 requires new `jsonPathLast`.
6. `NIX-SPEC.md §2` silent on built-in-meter `consumptionCap` being token-denominated; live
   module docs now state it.
7. Catalog schema ownership: issue bodies/§4-opening say FS-7; amended §4 assigns to FS-4
   (implemented as FS-4-owned, FS-7 consumes + adds goldens).
8. `FLOW-SPEC §11.5` mentions catalog path in runner env; amended NIX-SPEC-FLOW §4 requires
   `--catalog` and forbids `TALLY_FLOW_CATALOG` (followed amended CLI contract).
9. `NIX-SPEC-FLOW §1` `budgetPool`: normative producer rendering fixes runner pool to
   `[ "flow" ]`; `budgetPool` validated for existence only, no extra render channel invented.

## NOTABLE FINDING — latent FS-4 bug caught by live integration

FS-4's mock-based tests passed, but FS-5's live runner surfaced a real cross-crate bug: FS-4
hashed a singleton pool as `["alpha"]` while the kernel's canonical serializer encodes it as
`"alpha"`. On any real deployment this would have made every attach/replay **falsely diverge**
after the first create. FS-5 fixed the canonicalization in `tally-flow/engine.rs`, added a
byte-for-byte kernel-parity test, and also corrected credential-bearing-pool hashing. This is
the value of the live checkpoints — the pure-unit green was not the whole truth.

## Deployment recommendation (YOUR act — I did not deploy)

Per the campaign rule, the deployed daemon still predates all merges and was untouched
overnight. To take `main = e7ae081` to the fleet:

1. In the dotfiles flake, bump the `tally.nix` input to `e7ae081` (or `main`), `nix flake lock`.
2. Deploy **worker first, then coordinator** (CP-B's recommended order — the worker is the SSH
   executor target for cross-host flows; bring it up before the coordinator that dispatches to it).
3. Verify with the three commands below against the live fleet.

Do this in daylight with a rollback path; the multi-host flow path is proven in a VM but has
never run against the real fleet daemon.

## First three commands to see tally-flow alive (from the repo root)

```bash
# 1. Static flow surface — validate a real example flow through the dialect/pool/args checker
nix develop -c cargo run -q -- flow check examples/flows/pooled-review.js

# 2. Live single-host runner — six-node flow, SIGKILL replay, concurrent attach, in-process daemon
nix develop -c cargo test -p tally --test flow_live -- --nocapture

# 3. Full multi-host proof — coordinator+worker VM: SSH child, daemon-kill replay, Git handoff, witness
nix build .#checks.x86_64-linux.flow-multi-host -L
```

## State

`main = e7ae081`, working tree clean, all worktrees removed, all pools idle (GO). Campaign
issues: #44–#50 CLOSED via merge; #51/#52 OPEN with GREEN checkpoint reports (close after reading).
Full step-by-step ledger is in `FLOW-CAMPAIGN-STATE.md`.
