# FLOW-GAP — what exists, what doesn't, what we revisit, what we leave alone

Ground truth for the FS campaign, extracted from the tree at `main = 1ff5f3d`
(2026-07-24, post-wave-5, post-checkpoint-2). Citations are `file:line` in this repo.
Normative counterpart: `docs/FLOW-SPEC.md` / `docs/NIX-SPEC-FLOW.md`; unit mapping:
`docs/FLOW-BUILD-SEQUENCE.md`.

## 1. Exists and carries the flow era unchanged (leave as-is)

| Capability | Where | Note |
|---|---|---|
| Durable admission, pools, priorities, windowed budgets | `lease.rs` (2305 LoC): `admit_at` :475, `sort_pending` :949 (rank desc, sequence asc), `promote` :960 | The core product. Rank constants `config.rs:69-87`. |
| Job-originated enqueue, bounded | `wire.rs:674-710`: noEnqueue, requireDedupKey, depthCap 3, fanoutCap 64; rollback `daemon.rs:2936` | Mechanically shipped; doctrine catches up (FLOW-SPEC §1). |
| Blocking joins surviving restart | `BarrierTracker` `daemon.rs:231-348`; `await_job` :1185, `await_barrier` :1216; restore-from-witness :5730-5754, re-arm :5871 | The runner's join primitive. Waiter oneshots die with the daemon — clients re-await (runner does). |
| Memoized re-execution of passed work | `probe_dedup` `evidence.rs:482-547`; reuse admit `daemon.rs:930-1055`; crash-repair `reconcile_reuse_witnesses` :5075 | Witness-ledger-keyed, artifact-rehash-verified. Becomes `legacy` mode, byte-identical. |
| Hash-chained witness + additive-field discipline | `witness.rs`: schema :47-81, canonical hash :172-188, skip-if-absent forward compat :240 | The flow era's new fields ride exactly this discipline. No epoch break (reserved session). |
| Recovery re-registers parents (incl. adopted-running) | `daemon.rs:5937`, actions sorted AdoptRunning-first :5782 | A surviving runner keeps its enqueue capability across daemon restart — already true. |
| Cross-harness adapters + presets | `adapters.rs` (1402 LoC); presets `nix/lib/adapters.nix:80-241` | `job()`'s heterogeneity is free. |
| Producers (calendar/gh/events-dir/build-effect/pool-reachability) | `producers.rs` (7094 LoC); option schemas `common.nix:727-1276` | Flows are one more enqueue template through them, zero producer changes. |
| Gate manifests → canonical verdict | `completion.rs` (schema v1); `canonical_verdict` `daemon.rs:3533` (Fail downgrades, NotRun doesn't) | FS-6 adds env provisioning + defaults, not new semantics. |
| Query/observability contract (protocol 3) | `query_v2.rs` FactAuthority; trace/watch/pagination (`pagination.rs`: 48 KiB pages, 32 snapshots) | Checkpoint-2-certified. Flow adds one grouping key. |
| Eval-time config validation via the real binary | `mkCheckedConfig` `common.nix:1962-1980` → `Mode::CheckConfig` `main.rs:605` | The pattern `flow check` extends. |
| TALLY_* env contract + LoadCredential | `executor.rs:2664-2775`, :2031-2037 | Runner consumes `TALLY_SOCKET`/`TALLY_JOB_ID`; sanitizes before spawning daemon-driving children (cp2 finding). |

## 2. Exists but wrong for the flow era (revisit — in campaign)

| Defect | Ground truth | Fix (unit) |
|---|---|---|
| No in-flight idempotency: same dedup_key queued/running ⇒ **duplicate execution**; failed ⇒ silent fresh run; dedup disabled under gate_manifest | `evidence.rs:495` (Pass-only filter), `daemon.rs:899-903` (manifest exclusion), §4c of facts: no live-queue coalescing | `submission.mode="full"` disposition table, FLOW-SPEC §3 (FS-1) |
| Serial request/response per connection — a runner cannot multiplex awaits | `wire.rs:163-197`; single-thread LocalSet `daemon.rs:3853` | Concurrent per-request dispatch, FLOW-SPEC §7.1 (FS-3) |
| `FRAME_CAP_BYTES = 65536` chokes briefs/prompts/results | `wire.rs:23`, enforced :151/:201/:245 | Configurable 16 MiB default, §7.2 (FS-3) |
| `ParentInfo.children` is lifetime-cumulative — a long-lived runner permanently exhausts fanoutCap; live counter diverges from recovery reconstruction | increment `wire.rs:709`; decrement only on rollback `wire.rs:510-522`; recovery recounts surviving rows `daemon.rs:5768-5779` | `outstanding` counter + per-run `maxNodes`, §6 (FS-2) |
| `parents` map never evicted; `job_results` unbounded | facts §5 (no remove anywhere), `daemon.rs:258` only re-arm clears | Waiter-lifecycle retention + reconstruction-on-demand, §6 (FS-2) |
| Argv is the only identity vessel (prompts-as-argv, secrets queryable, unreadable status) | field report §3; `EnqueuePayload` `wire.rs:341-407` has no brief | Structured brief, §5 (FS-2) |
| Intra-priority strict FIFO by sequence — one 400-node flow starves siblings in-class | `sort_pending` `lease.rs:949-958` | Round-robin per flowRunId + aging, §8 (FS-3) |
| No env var tells an agent job where to write its gate manifest; no adapter defaults one | facts §10 (no TALLY_GATE*); manifest spec daemon-side only | `TALLY_GATE_MANIFEST` + preset defaults, §13 (FS-6) |
| Scraped consumption actuals not fed to windowed admission | meter contract exists (`TALLY_METER_*`, HM units `home-manager.nix:238-304`) but nothing wires adapter scrape → meter | Built-in feeder, §16 (FS-6) |
| Final agent message buried in captures (jq spelunking) | field report §6.2 | First-class projection, §13.2 (FS-6) |
| No flow runner, no dialect, no `services.tally.flows` | — | FS-4, FS-5, FS-7 |

## 3. Exists, imperfect, deliberately NOT revisited now

| Item | Why it stays |
|---|---|
| Witness encoding ballast (scalar-or-array pools, legacy hash preservation, TaskChampion projection) | Epoch-break/witness-v2 is a reserved Tom-led session (Trustix brief is its input). All flow fields land additive under the existing discipline. |
| `Enforce = cooperative` only; dmem/servingSlice/patched-systemd absent | Tested-absent by design (`flake.nix:640-649`); belongs to the #34/#35 conformance thread, not flows. |
| Remote-lease/cross-host re-adoption surface | PR #23's fail-closed executors stand; wave-7 (#34) territory. |
| `wait: bool` on the payload — carried, apparently inert beyond barrier-id return (`daemon.rs:1044-1054`) | Runner uses explicit `await_job`; leave the field's semantics untouched until the conformance thread rules on it. |
| BS-14 scenarios as shell scripts, not `nix flake check` VM tests | The multi-host `runNixOSTest` FS-7/CP-B adds is flow-scoped; migrating the BS-14 trio is #34's business. |
| Producers home-manager-only rendering | Matches the deployed topology (coordinator user daemon). Flows follow the same rule; NixOS-module parity is not a flow-era need. |
| `#33/#34/#35` conformance shells | Explicitly outside this campaign; behavior changes here accrue ORACLE-DELTAS obligations (frame cap, serving concurrency, dispositions, fairness) to be classified when wave 6 runs. |

## 4. Style-transfer map (exhaustive index)

| Brief (`docs/transfer/`) | Donates to |
|---|---|
| `boa.md` | FS-4: `from_async_fn` job() shape, JobExecutor ordering seam, intrinsic deletion mechanics, hardening hooks, error/stack surface, 0.21.1 pin |
| `rquickjs.md` | FS-4 contingency engine (decision inputs table) |
| `inngest-cloudflare.md` | FLOW-SPEC §3/§11.2: step-identity failure modes, memoization/replay evidence |
| `durable-execution.md` | FLOW-SPEC §12: Temporal sdk-core replay matching + nondeterminism detection; Obelisk = Boa-embedding prior art; script-edit rule rationale |
| `workflows-js.md` | Dialect shape, combinator semantics, determinism bans, authoring model, do-not-copy list |
| `spec-kit-vocabulary.md` | Step-vocabulary completeness check; gate deliberately unported; tasks.md-shape contract |
| `dotfiles-prior-art.md` | §11.5 selectors/quorum/dissent (normative), supervisor.sh lineage, roster/catalog fields |
| `nix-module-style.md` | FS-7: microvm.nix submodule anatomy, srvos hardening bundle (the one worked example), eval-validation idioms |
| `attic-trustix.md` | Deferred chapters: artifact data plane + GC-root retention (attic, deployed on fleet); witness-v2 reference (Trustix, reserved session) |

## 5. Numbers an implementer should have in hand

Workspace ~45.6k LoC. `daemon.rs` 11657, `producers.rs` 7094, `executor.rs` 5520,
`lease.rs` 2305, `wire.rs` 1213, `witness.rs` 1075, `evidence.rs` 1121,
`completion.rs` 505. Nix layer 3862 (`common.nix` 2016, `flake.nix` 969). Wire methods:
22 advertised (`wire.rs:25-48`) + 2 internal. Query protocol 3, schema 1; remote
executor protocol 2; gate manifest schema 1.
