# FLOW-BUILD-SEQUENCE — FS units for the flow era

Companion to `docs/FLOW-SPEC.md` (normative) and `docs/NIX-SPEC-FLOW.md`. Same contract as
`docs/BUILD-SEQUENCE.md`: each unit is feature-complete per spec — no stubs, no TODOs, no
MVP tiers; deliberately-deferred design chapters live only in FLOW-SPEC §20. Every unit is
one issue, one worktree, one codex session, one PR.

## 0. Preconditions (already true at campaign start)

- `main` = `1ff5f3d` (post-wave-5, post-checkpoint-2), no open PRs, clean worktree list.
- Conformance shells #33/#34/#35 are explicitly NOT part of this campaign; behavior
  changes made here (frame cap, concurrent serving, dispositions) accrue documented
  entries for the future `docs/ORACLE-DELTAS.md` per FLOW-SPEC §19.

## 1. Parallelization model — two lanes, one queue

Two exclusive implementer lanes, each a capacity-1 lease on the deployed daemon:

- **Lane A** = pool `build` (the proven wave-1..5 lane).
- **Lane B** = pool `coordinator-gpu` — consciously repurposed for this campaign as a
  second implementer mutex (the GPU is idle overnight; the pool is a lease token, and
  tally has never known what a GPU is). Heavy `nix` builds in lane B SHOULD offload to
  the worker (`nix build --builders`/distributed build via the existing fleet setup)
  to soften cargo contention on the coordinator.

Each lane = its own worktree per unit (`~/mecattaf/tally.nix-fsN`), branch `fs-N-<slug>`,
rebased on `main` at dispatch time. Units within a lane are sequential; lanes proceed
concurrently because their file surfaces are disjoint by construction (see §3 conflict
map). The orchestrator merges (from the main checkout, never a worktree), re-runs the
gate ladder, then dispatches the next unit of that lane on fresh `main`.

## 2. The units

### Lane A — kernel and wire

**FS-1 — submission idempotency, attach, canonical payload** (FLOW-SPEC §3)
`crates/tally-core` admission path + wire envelope. Canonical payload serialization +
`payloadHash` on row and witness (additive optional field); the complete §3.2 disposition
table including memoized failure; versioned enqueue-response `disposition` surface (§3.3);
`--wait` composition for attached waiters. Tests: every row of the case table, plus
byte-compat regression (absent fields ⇒ legacy hashes identical).
Dedup key: `fs-1-attach`.

**FS-2 — provenance, brief, counters, durable bookkeeping** (FLOW-SPEC §4, §5, §6)
`orchestration` object persisted row→witness; structured brief (inline + path, briefHash,
`TALLY_BRIEF` provisioning, query rendering split from argv); guardrail counter redesign
(outstanding-based fanout, per-run maxNodes via provenance, recovery re-registration of
parent entries); barrier/`job_results` retention bounded by waiter lifecycle with
reconstruction-on-demand. Tests: counter semantics under completion/rollback/restart;
brief round-trip; provenance grouping in `query jobs --flow-run`.
Dedup key: `fs-2-provenance-brief`.

**FS-3 — concurrent serving, frame discipline, fairness** (FLOW-SPEC §7, §8)
Per-request task dispatch with request-ID correlation and bounded in-flight window;
`FRAME_CAP_BYTES` → configurable transport limit (default 16 MiB) applied symmetrically;
intra-priority round-robin across `flowRunId` groups with single-step aging. Tests:
multiplexed awaits on one connection; ordering guarantees within watch streams;
starvation regression (400-node group vs 6-node group, same class).
Dedup key: `fs-3-concurrent-wire`.

**FS-6 — semantic truth for agent nodes** (FLOW-SPEC §13, §17)
Adapter-provisioned `TALLY_GATE_MANIFEST` by default on claude-code/codex presets with
`gates: not-run` visibility; final-agent-message first-class projection; node result
schema validation hook; `Assisted-by:` trailer emission from witness data in the gh
mutation sink. Tests: manifest-absent visibility; trailer format; result-schema reject
on passing verdict.
Dedup key: `fs-6-semantic-truth`. (Lane A slot 4 — touches adapters/gh sink, disjoint
from lane B's crate.)

### Lane B — the flow crate and its surfaces

**FS-4 — tally-flow: engine, dialect, host API, replay** (FLOW-SPEC §9–§12)
The one genuinely new component, feature-complete in a single unit: new workspace crate
`tally-flow` + `tally flow run|check` subcommands. Engine: Boa primary — record the
final engine decision with justification in the PR body after implementing against
`docs/transfer/boa.md` (contingency `docs/transfer/rquickjs.md`; Obelisk's
`deterministic_executor.rs` is prior art for driving Boa deterministically). Dialect:
banned-global removal/override with `FlowDeterminismError`; pure-literal `meta`
validation; static lint (`flow check`). Host API complete per §11: `job` + settle mode +
typed rejection codes, sugar (brief-carried prompts), `parallel`/`pipeline` fail-loud +
settle, `members` selectors + quorum/dissent helpers per the pi-appliance contract,
`log` with replay suppression. Ordinal keying + explicit-key override + duplicate
detection. Replay per §12: disposition-driven, payloadHash divergence detection,
script-edit refusal. Full error taxonomy surfaced with line/column.
Tests: determinism (3× identical ordinal streams incl. under `parallel`), every error
class, replay against a mocked daemon covering every §3.2 disposition, 3-member quorum
with dissent preservation, >64 KiB structured result.
Dedup key: `fs-4-flow-crate`. Runs against pre-FS-1 `main` with the daemon interface
behind a client trait; rebases and binds to the real dispositions in FS-5.

**FS-5 — replay/attach integration + runner-as-job** (FLOW-SPEC §9, §12)
After FS-1 and FS-4 merge: bind the runner's replay to the live disposition surface;
`TALLY_*` env sanitization; runner failure taxonomy exit codes; `source=orchestrator`
submission with ancestry + provenance on every node; end-to-end test on an isolated dev
daemon: flow of 6 nodes, kill runner mid-run → replay collapses prefix, kill daemon
mid-run → attach resumes await, divergence injection → `replay-divergence` fail-closed.
Dedup key: `fs-5-replay-integration`.

**FS-7 — Nix surface** (NIX-SPEC-FLOW entire)
`services.tally.flows.<name>` + rendering to calendar producers; auto-declared `flow`
pool + assertions; eval-time validation chain (`flow check`, pool closure, args schema,
catalog schema); catalog JSON Schema shipped + selector resolution goldens; hardening
preset vocabulary rendered to transient-unit properties; the seven new flake checks;
`examples/flows/` shipped as fixtures: `pooled-review.js` (the pi-appliance pattern
realized), `agency-nightly.js` (skeleton), `fleet-deploy.js` (zero-LLM shape).
Dedup key: `fs-7-nix-flows`. (Lane B slot 3 — pure Nix + fixtures, disjoint from FS-6.)

### Checkpoints (scarce and decisive — two, not five)

**CP-A — flow liveness, mid-campaign** (after FS-1, FS-4, FS-5)
Isolated dev daemon on the post-merge build. Live assertions: (1) example flow
end-to-end with heterogeneous nodes (sh + adapter node); (2) runner killed mid-graph →
replay, zero duplicate rows, prefix collapsed via dispositions; (3) daemon killed
mid-graph → runner reconnects/re-awaits, run completes; (4) payload-divergence injection
→ fail-closed with precise ordinal error; (5) attach under concurrent duplicate enqueue
(two runners, same keys) → single row per node; (6) fairness smoke: two flows braid
round-robin in one class. Red ⇒ fix session in the offending lane before anything else
merges. Dedup key: `cp-a-flow-live`.

**CP-B — the seal** (after all units)
Full gate ladder on final `main` (`cargo test --workspace`, clippy `-D warnings`,
`nix flake check` incl. the new flow checks and multi-host `runNixOSTest`, witness
regression valid-GREEN/tampered-RED, no-stubs sweep); calendar-producer-fired flow on a
dev daemon reconstructed end-to-end through the query surface after restart; completion
report in the checkpoint-2 house style: per-unit PRs, live results, accrued
ORACLE-DELTAS obligations, and the first three commands Tom runs in the morning.
Dedup key: `cp-b-seal`.

## 3. Dependency + conflict map

```
Lane A:  FS-1 ──► FS-2 ──► FS-3 ──► FS-6
                     │
Lane B:  FS-4 ───────┴─(needs FS-1+FS-4 merged)─► FS-5 ──► FS-7
                                                    │
                                  CP-A ◄────────────┘   (also needs FS-1)
Final:   CP-B after everything.
```

File-surface disjointness: FS-4 creates a new crate + subcommand registration (one small
`main.rs` touch — coordinate the merge); FS-1/2/3 live in tally-core/daemon/wire; FS-6
in adapters + gh sink; FS-7 in `nix/` + `examples/`. The single expected collision point
is `crates/tally/src/main.rs` subcommand wiring (FS-4) and wire.rs envelope types
(FS-1 vs FS-3) — FS-3 dispatches only after FS-1 merges (same lane, sequential, no
conflict by construction).

## 4. Per-unit ritual (unchanged house law)

The wave issue is the sole authoritative instruction set for the codex session; work
only in the worktree; commit locally; no push, no PR, no GitHub mutation; full gate
ladder run and pasted; ambiguity ⇒ BLOCKED with the precise question and spec citation —
never invented semantics. Orchestrator re-runs the ladder, pushes, PRs with
`Closes #…`, merges from the main checkout, prunes the worktree, rebases the other
lane's next dispatch. Evidence and honesty laws of `docs/CODEX-HANDOFF.md` §8 apply
verbatim to every session.
