# FLOW-SPEC — the flow era of tally

Status: DRAFT for the 2026-07-24 overnight campaign; becomes FROZEN when the spec PR
merges. Grounding pass against `docs/transfer/` briefs and the code-fact extractions is
mandatory before freeze (sections marked ⟨GROUND⟩ carry exact names/values from the tree).

Reading order for implementers: `docs/SPEC.md` (the kernel you are extending — its rulings
stay binding except where §1 and §19 here amend them) → this file → `docs/NIX-SPEC-FLOW.md`
→ the `docs/transfer/` brief named by your wave issue.

This spec is feature-complete by intent. There is no MVP tier: every behavior described
here, including every exception path, is in scope for the FS build sequence. Deliberately
deferred items appear only in §20 and are deferred as *design chapters*, not as cut
corners of in-scope features.

---

## 1. Doctrine

`SPEC.md`'s law "tally never decides what work should run next" is amended to its precise
form:

> **Tally never originates intent.** Every job — including a job submitted by another job —
> passes the same admission door, is bounded by ancestry guardrails and pools, and is
> witnessed. Resource pools, not spawn prohibitions, are the safety boundary.

The former constraint "tally does not schedule more tally runs" is repealed. Job-originated
enqueue (already mechanically present via `EnqueueSource::Orchestrator` and the socket
guardrails) becomes doctrine-sanctioned.

A **workflow is not a unit of scheduling; only nodes are.** A flow run exists at runtime
only as (a) an ordinary runner job in the `flow` pool and (b) provenance on the rows and
witnesses of its children. Between any two nodes, a flow run holds no scarce resource.
There is no pause/suspend/resume API — absence of held resources *is* the pause. The
scheduler remains singular; flow adds dependency structure hovering over the queue, never a
second arbiter.

One sentence for DECISIONS.md: *tally schedules jobs; a workflow is a deterministic,
content-hashed script that materializes jobs through the same admission door and exists at
runtime only as provenance.*

## 2. Vocabulary

- **flow**: a named, content-hashed workflow script plus its static args schema.
- **flow run**: one execution lifecycle of a flow: `flowRunId` (UUIDv7 assigned at first
  admission of the runner job), pinned `scriptHash`, pinned canonical `args`.
- **runner**: the `tally flow run` process executing the script; itself an ordinary tally
  job, restartable, stateless (its entire durable state is the witness chain + rows).
- **node**: one job materialized by the script (`job()` or sugar). Nodes are ordinary jobs.
- **call site**: one dynamic `job()` submission during script execution; identified by its
  **submission ordinal** (§11.2).
- **submission key**: the deterministic dedup key derived for a node (§3.2).
- **attach**: returning the identity/result of an existing row for a duplicate submission
  instead of creating a new row (§3).

## 3. Kernel seam 1 — submission idempotency and attach

This section is the load-bearing kernel change. Its semantics are frozen the day the second
flow script exists; implement exactly.

### 3.1 Canonical payload bytes

Define `canonicalPayload(job)` as the canonical JSON serialization (stable field order =
struct declaration order, `preserve_order` — matching the witness hash-input discipline;
absent optionals omitted, never null-filled) of exactly these `ResolvedEnqueue` fields
(`wire.rs:421-456`), which together define *the work*:

> `argv` (post `invocation` tokenization), `pools` (canonical order), `executor`,
> `adapter`, `cwd`, `workspace`, `adapter_options`, `gate_manifest`, `evidence`
> (canonicalized specs), `evidence_class`, `manifest_hash`, `runtime_max_sec`,
> `no_enqueue`, `credentials`, `briefHash` (§5).

Deliberately **excluded** (metadata about scheduling, ancestry, or intake — never work
identity): `priority`, `consumption_estimate`, `source`, `origin`, `parent`,
`caller_job_id` and depth, `task_uuid`, `gh_*` trigger fields, `related_trigger`, `wait`,
`resume_from`, the `dedup_key` itself, and the `orchestration` object (§4). Two
submissions differing only in excluded fields are the same work and attach.

`payloadHash = sha256(canonicalPayload)`. It is computed at admission, stored on the row,
and carried into the witness.

### 3.2 Attach semantics (complete case table)

Current behavior (extraction-verified): dedup consults **only terminal `Pass` witness
records** — a failed or in-flight row never matches and the submission runs fresh; dedup
is disabled entirely when a `gate_manifest` is present. That is the **`legacy`** mode and
it stays byte- and behavior-identical for every existing caller.

The new semantics are opt-in per enqueue: `submission.mode = "full"` (optional field;
absent = `legacy`). The flow runner ALWAYS submits with `full`. Under `full`, the
disposition table below governs, and idempotency operates **regardless of gate-manifest
presence** (the terminal disposition memoizes the gates outcome along with the verdict —
the legacy dedup-vs-manifest mutual exclusion does not apply in `full` mode).

For a `full`-mode enqueue carrying `dedup_key = K` with `payloadHash = H`:

| Existing row with key K | Payload | Behavior |
|---|---|---|
| none | — | create; response `disposition: "created"` |
| queued or running, hash H | identical | **attach**: no new row; response `disposition: "attached"` with the existing `taskUuid`, current status, attempt ordinal |
| queued or running, hash ≠ H | different | **fail closed**: error `dedup-key-conflict` carrying both hashes and the existing taskUuid; nothing enqueued |
| terminal `pass`, hash H, artifacts rehash clean | identical | **reused** (existing behavior): response `disposition: "reused"` with witnessed result |
| terminal `pass`, hash H, artifact rehash dirty | identical | existing re-run semantics for stale artifacts apply unchanged; response discloses `reusedRejected: "artifact-drift"` ⟨GROUND: exact current behavior⟩ |
| terminal `failed`/`skipped`/any non-pass verdict, hash H | identical | **memoized failure**: response `disposition: "terminal"` with the recorded verdict and witness seq. The kernel never silently re-runs a failed key — replay must observe the same history. A fresh attempt requires an explicit retry verb (new attempt lane, same key) or a new key (script repair branch). |
| any terminal, hash ≠ H | different | fail closed, `dedup-key-conflict` (a key permanently names one payload) |

Attach never mutates the attached row. `--wait` composes: an attaching waiter joins the
same terminal-result broadcast as the original submitter.

### 3.3 Wire surface

The enqueue response envelope gains the versioned `disposition` field
(`created | attached | reused | terminal`) plus `payloadHash`, `attempt`, and, for
terminal dispositions, the verdict + witness seq. Protocol-versioned per the existing
query-envelope discipline; older clients that ignore unknown fields keep working
(additive-only, §19).

## 4. Kernel seam 2 — orchestration provenance

Optional `orchestration` object on the enqueue payload, persisted on the row and into the
witness (additive optional field; absent = byte-identical legacy hashes):

```json
{ "flowName": "agency-nightly", "flowRunId": "…", "scriptHash": "sha256-…",
  "nodeOrdinal": 17, "nodeLabel": "impl:T042", "iterationPath": [2,0] }
```

The kernel never interprets it. Query projections group by it (`query jobs
--flow-run <id>`); the witness carries it verbatim as an optional skip-if-absent field
(hash-compatible additive). **Doctrine amendment, made consciously:** `validate_record`
today forbids graph keys (`parent`/`parent_uuid`/`kind`) in witness lines
(`witness.rs:238-242`) — that prohibition on ancestry *edges* stands, but the
`orchestration` capsule is admitted because for flow nodes provenance is part of the
proof: `scriptHash` in the witness is what ties a proved node to the exact
generation-pinned script that materialized it. The capsule is self-contained (names a
run, never points at another witness record) and `payloadHash` (§3.1) rides in the same
additive envelope. Children of a flow also carry standard
ancestry (`parent`, `caller_job_id`) and, when the flow was GitHub-triggered, the existing
`relatedTrigger` receipt reference — comment → receipt → runner → node → witness stays
queryable end to end with every hop honest about its true source.

## 5. Kernel seam 3 — structured brief

Generalizing `TALLY_GH_CONTEXT` per the operator field report: every job MAY carry a
**brief** — a structured JSON document distinct from argv (mission, boundaries, acceptance,
references). Mechanics:

- Enqueue accepts `brief` inline (small) or `briefPath` (content-addressed file the
  enqueuer wrote); either way the daemon stores the brief durably, hashes it
  (`briefHash` into row + witness), and provisions it to the job as
  `TALLY_BRIEF=<path>` alongside the adapter's own context env.
- `query status`/`query job` render brief summary fields separately from argv; argv stops
  being the identity vessel. The brief participates in `canonicalPayload` via `briefHash`.
- Flow sugar populates the brief automatically: `codex(prompt, opts)` puts the prompt in
  the brief, not in argv; argv becomes `[preset invocation] + TALLY_BRIEF reference`. This
  retires prompts-as-argv (and the secrets-in-queryable-state problem) for all flow nodes.

## 6. Kernel seam 4 — guardrail counters redesigned

The lifetime-children counter is replaced by three counters with distinct questions:

1. **`outstanding`** (per parent): children in non-terminal states. Incremented at
   admission, decremented at terminal fact. `fanoutCap` binds *outstanding*, not lifetime —
   a long-lived runner materializing hundreds of sequential nodes never exhausts it.
2. **`maxNodes`** (per flow run): lifetime nodes materialized under one `flowRunId` — the
   hard runaway backstop, checked at admission against provenance. Default 1000,
   per-flow-configurable in Nix.
3. **`iteration`** (per call site, runner-side): per-back-edge loop counter with per-script
   cap (default 64); exceeding it is a script error (§11.6), not a kernel concern.

`depthCap` semantics unchanged; a runner's re-execution after crash is the same logical
job (same row, new attempt) and burns no depth. Recovery already re-registers parent
entries for all recovered rows including adopted-running ones (`daemon.rs:5937`),
reconstructing child counts from surviving rows — under the new `outstanding` counter,
live decrement-on-terminal and recovery reconstruction converge on the same value,
eliminating the current live-vs-recovered divergence (live counter is
lifetime-cumulative today, incremented at `wire.rs:709`, decremented only on admission
rollback).

Guardrail and barrier bookkeeping must survive restarts by reconstruction from durable
facts (rows + witnesses), and barrier/`job_results` retention is bounded by waiter
lifecycle — eviction on last-waiter departure plus reconstruction-on-demand. No unbounded
in-memory accretion.

## 7. Wire — concurrent serving and frame discipline

1. **Concurrent per-connection serving.** Today the daemon runs a single-threaded
   `LocalSet` with one task per connection and strictly serial request→response inside a
   connection. The loop changes to dispatch each request into its own local task and
   write responses as they resolve; the existing request-ID correlation disambiguates. A
   single runner connection multiplexes six concurrent `await_job`s. Order of responses
   is unspecified; order within one request's response stream (watch) is preserved.
   Backpressure: per-connection bounded in-flight window of 64 requests (aligned with the
   wave-5 slow-reader discipline); excess requests queue in arrival order.
2. **Frame cap lifted.** `FRAME_CAP_BYTES = 65536` (enforced at `wire.rs:151`, `:201`,
   `:245`) becomes a configurable transport limit, default 16 MiB, applied symmetrically.
   Query pagination (`PageCache`, 48 KiB pages) remains — operator ergonomics — but
   enqueue payloads, briefs, and structured results are no longer forced through a 64 KiB
   needle. The witness discipline is unaffected (artifacts stay on disk; results SHOULD
   remain compact summaries — the cap is a safety limit, not an invitation).
3. **Attach response** per §3.3; **watch/cursor semantics** from wave 5 unchanged.

## 8. Scheduling — intra-priority fairness

Within a priority class, eligible rows are ordered by **round-robin across
`flowRunId`** (rows with no flow provenance form one virtual group per parent, standalone
rows one global group), with **aging**: a row waiting longer than `agingThresholdSec`
(default 3600) gains one effective rank step, once. Two flows plus the ordinary drain braid
through one queue; a 400-node OCR flow cannot starve a 6-node flow in the same class.
Deterministic given identical queue states (tie-break inside a group: admission order).

## 9. The flow runner

- `tally flow run <script.js> --args <json> [--flow-run-id <id>]` — an ordinary
  subcommand, executed as an ordinary tally job. The Nix module (NIX-SPEC-FLOW) renders
  flows as calendar/gh/events producers whose argv is exactly this.
- Runs in the auto-declared **`flow` pool** (cheap, default capacity 8). A blocked runner
  holds a socket and a near-free slot, never a GPU or a subscription window.
- The runner is **stateless**: no journal, no state files. Crash/restart → tally's normal
  retry re-presents the row → the script re-executes from the top → §3 collapses completed
  work. Its only local artifacts are logs and the provenance it stamps.
- The runner connects to `TALLY_SOCKET`, sanitizes inherited `TALLY_*` job-identity env
  before spawning any child tooling that might itself talk to a daemon (the cp2 finding),
  and submits every node with `source=orchestrator`, ancestry, and the §4 provenance
  object.
- Runner failure taxonomy: script syntax/eval error → runner exits with a distinguished
  exit code and a structured error report in its capture (verdict `failed`, gate
  `script-error`); determinism violation detected on replay (§12) → distinguished error
  `replay-divergence`, fail closed, no node submitted past the divergence point.

## 10. The dialect

Plain JavaScript executed by embedded **Boa** (`boa_engine`, pinned `0.21.1`, MSRV 1.91 —
decision grounded in `docs/transfer/boa.md`; rquickjs remains the documented contingency
in `docs/transfer/rquickjs.md`; Obelisk's `deterministic_executor.rs` is prior art). NOT
TypeScript. Scripts evaluate as `Script` (no module system; Boa's default
`IdleModuleLoader` stays — `import` goes nowhere by construction).

Engine hardening, all normative (mechanisms cited in the Boa brief):
- `Date` global **deleted** (`delete_property_or_throw`), not merely pinned via `Clock`;
- `Math.random` **overridden** to throw `FlowDeterminismError` (it is unhookable upstream);
- `WeakRef` and `FinalizationRegistry` globals **deleted** (the one place GC is observable);
- no timers are ever registered; no filesystem, network, or environment access exists;
- runtime string compilation (`eval`, `Function`) **forbidden** via
  `HostHooks::ensure_can_compile_strings` returning an error;
- `HostHooks::promise_rejection_tracker` **overridden**: an unhandled rejection is a run
  failure (`FlowUnhandledRejection`), never silently swallowed (Boa's default is a no-op);
- `runtime_limits` recursion + loop-iteration caps set as the uncatchable backstop (these
  errors bypass JS `try/catch` by design); a wedged synchronous script is ultimately
  killed by the job's `runtimeMaxSec` watchdog — Boa has no mid-evaluation interrupt.

Attempting to use a banned global throws `FlowDeterminismError` with the call site.

Script prelude (pure literal, validated before execution and at Nix eval time):

```js
export const meta = {
  name: 'agency-nightly',
  description: 'advance agency build overnight',
  pools: ['codex-window', 'claude-window'],   // every pool any node may request
  argsSchema: { /* JSON Schema for --args */ },
  maxNodes: 200,                               // optional, ≤ module cap
  iterationCap: 64,                            // optional
}
```

Violations (missing meta, non-literal meta, undeclared pool used at runtime, args failing
schema) are errors **before any node is submitted**; the Nix module additionally rejects
undeclared pools at eval time.

## 11. Host API — complete surface

### 11.1 `job(spec) → Promise<NodeResult>`

`spec`: `{ argv | adapter+prompt, pools, priority?, runtimeMaxSec?, evidence?, workspace?,
brief?, key?, label?, env? }` — the LLM-agnostic primitive matching the kernel's ontology.
Submission is **eager** (admission decides ordering, never the runner). Returns on the
node's terminal fact with
`NodeResult = { taskUuid, verdict, exitCode, witnessSeq, disposition, result?, gates? }`
where `result` is the structured summary (§13.2) when present.

Rejections (JS exception with typed `.code`): `dedup-key-conflict`, `admission-denied`
(guardrails/pools), `flow-node-cap` (maxNodes), `terminal-failure` — a node whose verdict
is non-pass rejects by default; `job(spec, { settle: true })` resolves with the failed
NodeResult instead, for scripts implementing quorum/repair logic.

### 11.2 Submission identity

Every `job()` call receives an **ordinal**: the 0-based count of submissions in program
order (single-threaded interpreter; `parallel()` submits in array order — deterministic).
The submission key is:

```
flow:<flowRunId>:<ordinal>            — default
flow:<flowRunId>:k:<spec.key>         — when the author supplies spec.key (must be unique
                                        within the run; duplicate → FlowKeyError at the
                                        second call site, nothing submitted)
```

Keys are scoped to the flow run — cross-run memoization is opt-in by the author supplying
a raw global `dedupKey` in spec (advanced; documented as escaping run scoping).

### 11.3 Sugar

`claude(prompt, opts)`, `codex(prompt, opts)`, `local(prompt, opts)`, `sh(argv, opts)` —
thin, host-side, each a documented mapping onto `job()` with an adapter preset, its
conventional pool set, and the prompt placed in the **brief** (§5), never argv. `local`
resolves its model through the catalog (§11.5). Sugar adds no semantics.

### 11.4 Combinators

- `parallel(thunks) → Promise<NodeResult[]>` — barrier. All submissions eager, then
  await all. **Fail-loud default**: if any element rejects, the barrier (after all settle)
  rejects with `FlowAggregateError` carrying every element's settled outcome.
  `parallel(thunks, { settle: true })` resolves with status-tagged results for
  quorum-style scripts.
- `pipeline(items, ...stages)` — no barrier between stages; stage signature
  `(prev, originalItem, index)`; a rejecting stage marks that item's chain failed
  (subsequent stages skipped); the pipeline resolves with per-item settled results and
  rejects at the end (fail-loud) unless `{ settle: true }` (final options argument).
- `log(msg)` — appended to the runner's capture and lifecycle stream; replay-safe (logs
  from replayed prefixes are suppressed by disposition, not re-emitted as new events).

### 11.5 Selectors and quorum helpers (pi-appliance contract)

`members(selector, opts)` resolves a pooled-capability selector (`'pooled-fast'`,
`'pooled-strongest'`, count, diversity key) **deterministically** against the catalog JSON
provided at flow registration (Nix-rendered; path in runner env; content-hashed into
provenance). The resolved member list is stamped into the run's provenance **before any
member node is submitted** — the pi-appliance witness-before-inference rule. Quorum
scaffolding (`quorum({ results, minimumValid, requiredMembers })`,
dissent-preserving reduce helpers that force per-member attribution into the aggregate) are
host helpers implementing `docs/transfer/dotfiles-prior-art.md` §2 exactly — identity /
deterministic / aggregate-node reducer classes, one-repair-attempt idiom (a repair branch
is an ordinary `job()` with `key: '<member>@1'`), fail closed below quorum.

### 11.6 Script errors

Uncaught script exception → runner failure (§9) with the exception, stack (line/column),
and the ordinal frontier recorded in the capture. `iterationCap` breach → `FlowLoopError`
naming the call site. All error classes are part of the dialect's documented surface;
authors branch on `.code`.

## 12. Replay — the hard invariant (full enforcement)

1. **Replay-stable inputs only.** A node's spec may be built from: `args`, literals,
   `meta`, and **witnessed results of prior nodes**. The dialect makes everything else
   unreachable (no fs/env/net/clock). Host sugar MUST NOT reintroduce ambient input (no
   read-file-into-prompt helper — pass paths; workers read).
2. **Replay protocol.** On (re)execution the runner derives each submission key and
   enqueues; §3 dispositions replay history: `reused`/`terminal` return recorded results,
   `attached` awaits live work, `created` is the frontier. No runner-side journal exists
   or is needed.
   **Observation-order law.** Node results become observable to the script (promises
   resolve) in a reproducible order or replay is unsound. The order is **the witness
   chain's terminal order for the run's nodes**: live, the runner's `JobExecutor` resolves
   promises in true completion order — which the witness chain durably records; on
   replay, the runner queries the recorded terminal order for its `flowRunId` and
   resolves the replayed prefix in exactly that order before proceeding live at the
   frontier. The witness ledger IS the event history — Temporal's history-replay model on
   tally's native ledger. Never the Boa examples' `poll_once(FutureGroup)`
   first-future-wins drain (Boa guarantees only FIFO of what the host enqueues; ordering
   ready futures is entirely the host's decision — see the Boa brief §3). Full pipeline
   concurrency is preserved: only observation is ordered, execution is not.
3. **Divergence detection (Temporal-grade).** For every replayed ordinal whose disposition
   is not `created`, the runner compares its re-derived `payloadHash` with the recorded
   one. Mismatch → `replay-divergence` failure naming ordinal, both hashes, and both
   labels. This catches the residual nondeterminism class the language bans can't (author
   branching on non-witnessed values smuggled through args mutation).
4. **Script-edit rule.** `flowRunId` pins `scriptHash`. The runner refuses to resume a
   flow run when the script's current hash differs from the recorded one
   (`script-changed-mid-run`, fail closed). An edited script starts a new run; cross-run
   reuse only via explicit author keys (§11.2). There is no patching/versioning mechanism
   in this era (§20).

## 13. Semantic truth for agent nodes

1. **Gate manifests by default.** Today the `GateManifestSpec` (path +
   `requiredGateIds` + acceptance policy) travels on the execution request and is
   evaluated daemon-side (`completion.rs`, schema v1, 1 MiB bound, O_NOFOLLOW); **no env
   var tells the job where to write** — the enqueuer must communicate the path out of
   band. Change: the executor exports `TALLY_GATE_MANIFEST=<spec.path>` whenever a gate
   manifest is declared, and the claude-code/codex adapter presets **default** a manifest
   (executor-owned path beside the capture file, empty `requiredGateIds`, `manual`
   acceptance) when the enqueue declares none. Declared-but-absent manifest file keeps
   the existing `GateSummaryStatus::NotRun` semantics — visible, never conflated with
   exit-0 success, and per current `canonical_verdict` (`daemon.rs:3533`) NotRun does not
   downgrade a Pass; only `Fail` does. Flow `NodeResult.gates` surfaces the summary to
   scripts.
2. **Final message + structured result.** A job's final agent message becomes a
   first-class projection (`query job` field, `NodeResult.result`). Flow nodes may declare
   a result schema in spec; the runner validates the structured result before the script
   observes it (invalid ⇒ the node rejects with `result-schema-mismatch` even on verdict
   pass — the pi-appliance validate stage, generalized).

## 14. Failure propagation in scripts

Kernel stays dumb and closed: verdicts are facts. Scripts express recovery **visibly**:
observe a settled failure, materialize a repair branch (`key: 'member@1'`), enforce
quorum, or rethrow. No automatic fallback, no kernel-level dependency cascade (the await
model makes `needs`-style cascades a script-level `if`). No mid-run human gates ever —
the culmination artifact is the gate (closed ruling).

## 15. Data plane — explicit rule

**Tally moves no bytes between hosts.** Cross-host handoff is via the workspace repo
(commits/branches) or the deployed artifact store (attic push / substitute; R2 for public
artifacts). Flow scripts MUST NOT assume a shared filesystem across pools on different
hosts. The dialect docs state this rule verbatim; nixosTest fleet checks include one
cross-host handoff exercised through the sanctioned channels. Evidence records reference
artifacts; they never carry them.

## 16. Budgets and the usage meter

Adapter-scraped consumption actuals are wired into the **existing external-usage-meter
input** as an advisory headroom clamp (never a charge mutation — proof purity holds): the
scrape envelope feeds the same `TALLY_METER_EVENT_PATH` contract the original rulings
defined, as a built-in feeder. Minimal by ruling: one feeder, windowed pools only. A flow
run MAY declare a run-scoped budget pool (`meta.budgetPool`) its nodes co-charge —
workflow.js `budget` semantics realized with existing pool machinery.

## 17. Provenance projections

Tally-produced commits/PRs carry a standardized trailer generated from witness data:
`Assisted-by: <adapter>:<model> (tally:<taskUuid> witness:<seq>)` — the kernel-community
attribution convention as the cheap greppable projection of the witness. Emitted by the
gh mutation sink where it already posts evidence; skill/prompt revision hashes ride the
provenance object for future distillation loops.

## 18. Blast radius

Adapter-level systemd hardening presets (named bundles rendered into transient units:
`ProtectHome`, `PrivateTmp`, `ReadWritePaths`, per srvos transfer brief) — configuration,
not tool-side output policing (which stays rejected). Preset vocabulary and defaults in
NIX-SPEC-FLOW; `none` remains expressible for trusted local work.

## 19. Compatibility discipline for this era

**Additive-only.** Every new field is optional; absent optionals preserve legacy hash
bytes (existing discipline). No witness-chain epoch break in this campaign — witness v2 /
encoding cleanup is a reserved Tom-led session (Trustix brief is its input). The lifted
frame cap and concurrent serving are behavior changes, not encoding changes; both get
ORACLE-DELTAS entries when the BS-13 harness runs.

## 20. Explicitly out of scope (design chapters reserved, not corners cut)

Witness v2 epoch break; script patching/versioning for in-flight runs; `drv()` derivation
nodes and store-native memoization; attic-backed evidence retention via GC roots;
query.watch-driven web/TUI; cross-machine witness comparison; microvm executor tier;
multi-tenant anything.
