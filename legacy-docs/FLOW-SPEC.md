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
- **flow run**: one execution lifecycle of a flow: `flowRunId` — verbatim the runner
  job's `taskUuid`, assigned by the daemon at first admission of the runner job (FS-2
  switches task-uuid minting at `daemon.rs:846` from `Uuid::new_v4` to `Uuid::now_v7`,
  so new run ids are time-sortable; UUID parsing is version-agnostic, additive-safe),
  pinned `scriptHash`, pinned canonical `args`.
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
| terminal `pass`, hash H, artifact rehash dirty | identical | existing re-run semantics for stale artifacts apply unchanged; response discloses `reusedRejected: "artifact-drift"` (drift grounding below) |
| terminal `failed`/`skipped`/any non-pass verdict, hash H | identical | **memoized failure**: response `disposition: "terminal"` with the recorded verdict and witness seq. The kernel never silently re-runs a failed key — replay must observe the same history. A fresh attempt requires an explicit retry verb (new attempt lane, same key) or a new key (script repair branch). |
| any terminal, hash ≠ H | different | fail closed, `dedup-key-conflict` (a key permanently names one payload) |

Attach never mutates the attached row. `--wait` composes: an attaching waiter joins the
same terminal-result broadcast as the original submitter.

### 3.2.1 Durable side-effects per disposition (normative)

Only `created` materializes state:

| disposition | new row | witness append | taskUuid in response | counters |
|---|---|---|---|---|
| `created` | yes (Fresh) | at terminal fact, as today | the new row's | `outstanding` +1 at admission, −1 at terminal; flow-run node count +1 |
| `attached` | no | none | the existing live row's | none — the original creation already charged and is still outstanding, counted once |
| `reused` | no | none | the governing terminal record's | none |
| `terminal` | no | none | the governing terminal record's | none |

Full-mode `reused` is a pure read of the ledger. Unlike legacy reuse — which
materializes a `Completed` row and appends a `Verdict::Reused` record
(`daemon.rs:930-1055`; preserved verbatim for `legacy` mode) — full mode returns the
original row's `taskUuid`, the recorded verdict, and the original terminal record's
`witnessSeq`, appending nothing. `terminal` likewise. This is what makes replay sound:
re-running an N-node completed prefix touches no durable state, `maxNodes` (§6.2) counts
only `created` rows, the §12.2 terminal order stays singular per ordinal, and CP-A's
zero-duplicate-rows assertion is meaningful.

Guardrail charging: the fanout/outstanding charge commits only on the `created` path. An
implementation charging before the disposition probe (today's order, `wire.rs:709`) MUST
roll back on every non-`created` disposition; observable behavior is as-if never charged.
`fanoutCap` denies only submissions that would create a row — a parent at cap still
resolves attach/reused/terminal successfully (a replaying runner is never starved by its
own history). Recovery reconstruction (surviving non-terminal child rows) and live
counting converge by construction.

No-artifact rule (full mode, normative): "artifacts rehash clean" quantifies over the
governing record's declared artifact paths. When the governing terminal-`pass` record's
evidence declares no artifact paths (e.g. `exit:0`-only — the common shape for flow `sh`
and adapter nodes), the rehash step is vacuously satisfied and the disposition is
`reused`. The legacy `NoArtifactEvidence` miss (`evidence.rs:503-506`) applies in
`legacy` mode only. Full-mode terminal matching is keyed on `dedup_key` + recorded
`payloadHash` and does not require `artifact_content_hash` presence on the record
(unlike the legacy probe filter, `evidence.rs:496`).

Governing-candidate rule (normative). For key K under full mode: (1) live rows govern
first — `Paused` classifies with queued/running (it is a non-terminal liveness state).
Exactly one live row with key K ⇒ the table's live rows apply (attach on hash match,
conflict on mismatch). More than one live row with key K (legal residue of legacy
no-coalescing) ⇒ fail closed, `dedup-key-conflict`, disclosing every live `taskUuid` —
full mode never chooses among duplicates. (2) No live row ⇒ the governing record is the
terminal witness record for K with the highest `seq` (the legacy probe's rule,
`evidence.rs:498`); earlier records for K are superseded history and are never
consulted, whatever their hashes. (3) A governing terminal record with no recorded
`payloadHash` (pre-full-mode history) is incomparable: the submission proceeds as
`created` and the response discloses `reusedRejected: "payload-hash-unrecorded"` —
visible, deterministic, and unreachable for flow-namespaced keys
(`flow:<flowRunId>:…` is a fresh namespace).

Artifact-drift grounding (resolves the table's drift row): "existing re-run semantics
apply unchanged" means exactly the current `WitnessHashMismatch` path — plain dedup miss
⇒ a fresh Fresh-row run is admitted (`evidence.rs:535-537`). The full-mode response for
the drift row is therefore `disposition: "created"` plus the disclosure. All three
full-mode rehash-miss reasons surface, each under its own string: `WitnessHashMismatch`
⇒ `reusedRejected: "artifact-drift"`; `DeclaredHashMismatch` ⇒
`reusedRejected: "declared-hash-mismatch"`; `ArtifactUnavailable` ⇒
`reusedRejected: "artifact-unavailable"` (offending path in the error-detail field).
`reusedRejected` is disclosure only — the disposition is `created` in all three cases
and the fresh run proceeds normally.

### 3.3 Wire surface

The enqueue response envelope gains the versioned `disposition` field
(`created | attached | reused | terminal`) plus `payloadHash`, `attempt`, and, for
terminal dispositions, the verdict + witness seq. Protocol-versioned per the existing
query-envelope discipline; older clients that ignore unknown fields keep working
(additive-only, §19).

Envelope mechanics (normative): every `queue.enqueue`/`queue.continue`/`queue.retry`
success response gains `schemaVersion: 1` — a NEW enqueue-response schema counter,
independent of `QUERY_PROTOCOL_VERSION` (which is not bumped; it versions the query
surface only) — advanced in the future only for additive changes, per the standing
discipline. All existing response fields (`task_uuid`, `job_id`, `barrier`, `state`,
`status`, `verdict`, `dedup_key`, `artifact_content_hash`, `witness_lsn`, …) survive
with unchanged names, types, and values in both modes: the versioned surface wraps
additively around the current shape, never replaces it. `disposition` appears on every
enqueue response (legacy fresh ⇒ `created`; legacy reuse hit ⇒ `reused`). Full-mode
responses additionally carry `payloadHash` and `attempt`, and — for `reused`/`terminal`
— the recorded `verdict` and the governing terminal record's `witnessSeq` (`reused`
responses carry the witnessed verdict, i.e. `pass`; the disposition field, not the
verdict, conveys reuse).

The retry verb (normative, FS-1 scope): new wire method `queue.retry`, CLI
`tally queue retry <task-uuid>`. Preconditions (else `invalid`): the row exists and is
terminal with a non-pass verdict — retrying a passed row is invalid (reuse semantics own
that case; genuinely new work needs a new key). Effect: the same row re-enters `Queued`
on a new attempt lane (`attempt+1`, same `taskUuid`, same payload and `payloadHash`,
fresh lease lifecycle); no new row; nothing is appended to the witness at retry time —
the new attempt's terminal fact witnesses normally. The parent's `outstanding`
re-increments at retry admission and decrements at the new terminal fact; no depth is
burned. While the retry attempt is live, a full-mode enqueue of the same key sees a
queued/running row and follows the live rows of the §3.2 table; once it settles, the
governing record is the new latest terminal record for the key. Response: the versioned
envelope with `retried: true` plus `taskUuid` and the new `attempt` (`created`
semantics do not apply). `queue.continue` is untouched and remains the scraped-session
resume verb.

## 4. Kernel seam 2 — orchestration provenance

Optional `orchestration` object on the enqueue payload, persisted on the row and into the
witness (additive optional field; absent = byte-identical legacy hashes):

```json
{ "flowName": "agency-nightly", "flowRunId": "…", "scriptHash": "sha256-…",
  "nodeOrdinal": 17, "nodeLabel": "impl:T042",
  "promptRevision": "sha256:…", "skillRevision": "review-agent-v3",
  "maxNodes": 200, "selection": { "selector": "pooled-fast", "catalogHash": "sha256-…",
  "memberId": "…", "members": ["…"] } }
```

`iterationPath` does not exist — it has no honest derivation in plain JS; node identity
within a run is `nodeOrdinal` (+ optional `nodeLabel` and the §11.2 submission key).
`maxNodes` (§6.2) and `selection` (§11.5) are optional. The kernel interprets exactly
two capsule fields — `flowRunId` and `maxNodes` (the §6.2 admission backstop) — and
carries everything else opaque, verbatim, row→witness.

`promptRevision` and `skillRevision` are reserved optional capsule keys populated
host-side for `claude()`, `codex()`, and `local()` nodes. `promptRevision` is
`"sha256:" + hex(Sha256(prompt_bytes))`, where `prompt_bytes` is the exact UTF-8
sequence of the resolved prompt submitted in the structured brief. Adapter configuration
may carry either resolved `skillBundle` content, producing the same hash construction
over its UTF-8 bytes, or a stable `skillRevision` version/name copied verbatim; the two
configuration inputs are mutually exclusive. Unknown skill revision means the key is
absent, never a placeholder. Files used as bundles are resolved while constructing the
adapter configuration, not read by the runner during replay. A changed prompt changes
both `promptRevision` and the brief-derived `payloadHash`, so the existing
`replay-divergence` path remains the enforcement mechanism. With both keys absent, the
serialized capsule and witness hash input remain byte-identical.

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

`briefHash` input bytes (normative): the daemon parses the brief — the inline JSON value
or the `briefPath` file's contents — and hashes the compact canonical serialization of
the parsed document: `serde_json` compact form, object member order preserved exactly as
received (`preserve_order`, the witness hash-input discipline), no insignificant
whitespace, absent optionals omitted. Stated consequences: an inline brief and a
`briefPath` file containing the same document (same member order, same values) produce
the same `briefHash` and therefore attach; formatting and whitespace differences never
matter; member-order differences do (order is content under `preserve_order` — the flow
runner always serializes its briefs deterministically, so replay re-derivation is
byte-stable by construction). The daemon stores the brief durably at a content-addressed
path derived from `briefHash`; `TALLY_BRIEF` names that path. Raw-file-bytes hashing is
rejected: inline briefs have no raw bytes after JSON-RPC parsing, and
transport-dependent hashes would break inline↔path attach.

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

`maxNodes` transport (normative): the per-run cap travels in the orchestration capsule.
`tally flow run` gains `--max-nodes <n>`; the Nix module renders
`"--max-nodes" (toString maxNodes)` into every flow producer argv (companion amendment
in NIX-SPEC-FLOW §1). The runner computes the effective cap = min(`--max-nodes`,
`meta.maxNodes`) over the values present (the Nix default always renders 1000) and
stamps it as `maxNodes` on every node's orchestration capsule. At admission of a
submission that would resolve `created` and carries orchestration provenance, the daemon
counts durable rows whose `orchestration.flowRunId` equals the capsule's and rejects
with `flow-node-cap` when count ≥ the capsule's `maxNodes` (absent ⇒ daemon default
1000). Attach/reused/terminal never count (§3.2.1). The count is reconstructed from
surviving rows at recovery. The capsule is excluded from `canonicalPayload` (§3.1), so a
changed cap never perturbs work identity or trips §12.3 divergence. The cap is a
runner-stamped runaway backstop, not a security boundary — pools remain the safety
boundary; depthCap/fanoutCap/noEnqueue still bound a misbehaving runner.

Call-site identity for the iteration counter (normative): the counter is keyed by the
source position `(line, column)` of the innermost flow-script stack frame at the moment
`job()` or sugar is invoked — i.e. the position of the `job(` token itself; host frames
are skipped, script frames never are. Stated consequence, documented in the dialect
docs: a script-defined helper wrapping `job()` is ONE call site for every loop that
reaches it — its counts aggregate, and authors of helper-heavy scripts raise
`meta.iterationCap` accordingly. This is the deterministic reading; plain JS has no
reified back-edges, so the token position is the back-edge proxy. Each call site's
counter is per flow-run execution, monotonic, and never resets — not per `parallel()`,
not per enclosing function. Re-execution recounts identically because the script is
deterministic. Exceeding the cap (`meta.iterationCap`, default 64, applied per call
site) throws `FlowLoopError` naming the position and the count (§11.6).

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

Braid definition (normative, stateless): at each scheduling pass, within one
effective-rank stratum, every eligible row belongs to exactly one group — its
`orchestration.flowRunId`; rows without flow provenance group by parent row; parentless
standalone rows form one global group. Within a group, rows order by admission sequence
ascending; a row's braid index k is its 0-based position among its group's
currently-eligible rows. Groups order by their oldest eligible row's admission sequence,
ascending. The stratum's total order is: k ascending, then group order, then own
admission sequence. There is no rotation cursor and no scheduler state: the order is a
pure function of the queue snapshot, so "deterministic given identical queue states"
holds literally and the ordering is restart-invariant by construction. The FS-3 fairness
regression encodes exactly this function.

"One effective rank step" (normative): a row waiting longer than `agingThresholdSec`
takes, as its effective rank, the next-higher priority class's rank value — `low` 10→50,
`medium` 50→100, `high` 100→1000; `interrupt` is already top and never ages — applied
once, never compounding. The aged row sorts and braids within the higher stratum as its
own group (the braid definition applies unchanged). Effective rank is computed at sort
time from `now − admission time` and is never persisted; the base priority on the row is
untouched and all projections keep reporting it.

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
- `flowRunId` derivation (normative): the runner reads `TALLY_TASK_UUID` from its
  environment and uses it verbatim; `--flow-run-id <uuid>` overrides (tests, CP-A's
  two-concurrent-runners assertion, manual resume). Neither present ⇒ fail closed at
  startup with a distinguished exit code (part of the failure taxonomy above). Because a
  crash/re-attempt re-executes the same row with the same `taskUuid`, `flowRunId` is
  byte-identical across attempts with zero extra machinery — §11.2 submission keys and
  §12 replay hold by construction. The Nix-rendered producer argv stays static: no id
  flag is ever rendered.

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
  selectors: ['pooled-fast'],                  // optional; every selector class
                                               // members() may be called with
}
```

`meta.selectors` semantics (normative): the exhaustive declaration of selector classes
the script may pass to `members()` (§11.5). Calling `members()` with an undeclared
class — or at all when the field is absent/empty — is a script error
`selector-undeclared` naming the class, raised before any submission where statically
detectable and at the call otherwise. `flow check --catalog` verifies every declared
class resolves to ≥1 catalog member. The Nix module requires `flows.<name>.catalog`
exactly when `meta.selectors` is non-empty; a catalog set for a script with no declared
selectors is permitted and inert.

Violations (missing meta, non-literal meta, undeclared pool used at runtime, args failing
schema) are errors **before any node is submitted**; the Nix module additionally rejects
undeclared pools at eval time.

## 11. Host API — complete surface

### 11.1 `job(spec) → Promise<NodeResult>`

`spec`: `{ argv | adapter+prompt, pools, executor?, priority?, runtimeMaxSec?,
evidence?, workspace?, brief?, key?, label?, env?, resultSchema? }` — the LLM-agnostic
primitive matching the kernel's ontology.

`executor?` (string — an executor name declared in daemon config) is marshalled verbatim
onto the enqueue payload's first-class `executor` field (`wire.rs:356`), defaulting
exactly as any enqueue does; it participates in `canonicalPayload` (§3.1). No
pool→executor mapping exists or is added — pools remain resource leases, executors
remain placement, orthogonal. The multi-host test and `examples/flows/fleet-deploy.js`
place worker-side nodes with `executor: 'worker'`; the §15 data-plane rule applies to
such nodes unchanged.

`resultSchema?` (a JSON Schema object) is host-API surface only — never marshalled onto
the wire, absent from `ResolvedEnqueue`, and outside `canonicalPayload`; it names what
the script will accept, not the work. Validation and the `result-schema-mismatch`
rejection are implemented in `tally-flow` (FS-4; live-bound in FS-5) — the kernel side
(FS-6) supplies only the final-message/structured-result projection that feeds
`NodeResult.result`. When `resultSchema` is declared and `NodeResult.result` is absent
or fails validation, the node rejects with `result-schema-mismatch` even on verdict
pass; `job(spec, { settle: true })` resolves with the settled NodeResult carrying the
mismatch.
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

### 11.2.1 `drv(spec) → Promise<NodeResult>`

`drv()` is the first-class primitive for hermetic, replay-stable work:

> If a node is hermetic and replay-stable, it should be a derivation and Nix memoizes it.
> `job()` exists only for the impure — and everything impure gets witnessed.

`spec` is `{ drvPath, outputs: [{ name, path }, ...] }`. The derivation path must be a
top-level Nix store path ending in `.drv`; outputs are canonicalized by name and must be
non-empty, uniquely named top-level store paths. The fixed mapping is
`nix build --no-link <drvPath>^*`, pool `build`, `store:<path>` evidence for every
output, and dedup key `drv:<drvPath>`. The submitted task UUID is derived from the
flow-local `flowRunId` and ordinal: it is byte-stable on replay, while a later flow run
gets a distinct seed and therefore its own witness.

The `build` pool is reserved in `meta.pools` and auto-declared beside `flow` with resource
`build-slot`, cooperative enforcement, no hard preemption, and default capacity 2. If
any output is not valid, the daemon admits an ordinary build row that leases one slot.
If all outputs are valid, it skips admission entirely: no row and no lease. It still
appends the cheap, hole-free witness with disposition and verdict `substituted`, pools
`["build"]`, the stable UUID, derivation identity, and output `storePaths`.

Store reuse validates paths through Nix rather than rehashing artifacts. Locally built
outputs can enter downstream `build-effect` work through the host's existing Nix
post-build-hook or JSONL feed. The substitution fast path does not synthesize a build
event; its witness records that the already-valid store object satisfied the node.

### 11.3 Sugar

`claude(prompt, opts)`, `codex(prompt, opts)`, `local(prompt, opts)`, `sh(argv, opts)` —
thin, host-side, each a documented mapping onto `job()` with an adapter preset, its
conventional pool set, and the prompt placed in the **brief** (§5), never argv. `local`
resolves its model through the catalog (§11.5). Sugar adds no semantics.

Prompt delivery (normative, all adapters): the sugar's argv is the preset invocation
plus ONE constant positional after the preset's `--`, the literal sentinel:

`Read the file whose path is in the TALLY_BRIEF environment variable and execute the
mission it contains. That brief is your complete instruction set.`

defined once in `tally-flow` as an exported, golden-tested constant, identical for
`claude`, `codex`, `local`, and pi-adapter members (`local` resolves to a catalog member
whose adapter preset gets the same sentinel). The mission rides exclusively in the
brief; the sentinel is constant, so argv — and with it `canonicalPayload` — is identical
across replays and across nodes that differ only in prompt (identity flows through
`briefHash`, §5). No filesystem path and no `$`-expansion appears in argv: the executor
spawns without a shell, and the agent process reads its own environment. `sh(argv)`
takes no prompt and gets no sentinel; a brief attaches to a `sh` node only when the
author passes `opts.brief`.

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

`log()` mechanics (normative): the lifecycle stream is the runner's own structured
stdout/capture — JSONL events — with no wire emission and no new RPC (the kernel stays
closed). Suppression is per-ordinal and uniform across first execution and replay,
requiring no mode detection: each `log(msg)` is attributed to the runner's current
submission frontier f (the count of submissions already made, i.e. the next ordinal).
The event is withheld until ordinal f's disposition is known, then emitted iff that
disposition is `created`; suppressed iff it is `reused`, `terminal`, or `attached` (the
log belongs to a replayed/duplicated prefix). Logs after the script's final submission —
where ordinal f never comes to exist — emit at script exit. On a first execution every
disposition is `created`, so every log emits (slightly deferred); on replay exactly the
frontier-and-beyond logs emit. Suppressed logs MAY be written to the capture tagged
`replayed: true` for debugging; they never enter the lifecycle stream as events.

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

Witness-before-inference, realized (normative — no new kernel seam): (1) before the
first member submission, the runner emits a `selector-resolved` lifecycle event (§11.4
stream, flushed to its capture) carrying `{ selector, opts, catalogHash, members:
[ids] }` — the pre-inference durable record in the runner's own evidence, exactly the
pi-appliance transcript rule. (2) Every member node's orchestration capsule carries
`selection: { selector, catalogHash, memberId, members: [ids] }`, persisting row→witness
with the capsule: no member node can exist in the ledger without the resolution that
produced it. Together these satisfy "stamped into the run's provenance before any member
node is submitted" — (1) temporally, (2) durably. Resolution is a pure function of the
content-hashed catalog (the runner content-hashes the catalog file at startup,
`catalogHash`), so replay re-derives the identical list; a catalog change that alters
membership changes the member nodes' own specs and fires §12.3 payload divergence on the
first affected ordinal — no separate catalog-pinning rule is needed.

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
   Observation-order mechanics (normative): no query is involved. Every `reused` and
   `terminal` disposition response carries the governing terminal record's `witnessSeq`
   (§3.3); live nodes acquire theirs at their terminal fact. The runner's JobExecutor
   resolves ready node promises in ascending `witnessSeq` order — one rule covering both
   regimes: for replayed ordinals this is exactly the witness chain's recorded terminal
   order (a replayed promise is held while any known replayed promise with a smaller
   `witnessSeq` is unresolved); at the frontier, fresh witness seqs are minted in true
   completion order, so live resolution order is completion order, durably recorded for
   the next replay. `attached` ordinals are live work and resolve on true completion,
   necessarily after every already-terminal replayed ordinal. FS-2's
   `query jobs --flow-run` grouping carries no ordering obligation for replay soundness.
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
   Injection point (normative): preset manifest defaulting happens at execution-request
   construction — daemon-side, strictly post-admission. When the daemon builds the
   `ExecutionRequest` for a row whose adapter preset is claude-code/codex and whose
   row-level `gate_manifest` is `None`, it synthesizes `GateManifestSpec { path:
   <deterministic executor-owned path beside the capture file, a pure function of row
   identity + attempt>, requiredGateIds: [], acceptance: manual }` onto the request; the
   executor exports `TALLY_GATE_MANIFEST` from the request's spec in both the declared
   and defaulted cases. The row is never mutated: `canonicalPayload.gate_manifest`
   contains exactly what the enqueuer declared (possibly nothing), so resubmission and
   replay hashes are unaffected, §12.3 divergence never fires on defaulting, and the
   legacy dedup manifest-exclusion (`daemon.rs:900-902`, keyed on the row field) is
   untouched — §3.2's byte- and behavior-identical promise holds. Completion-side gate
   evaluation reads the spec from the execution request (`completion.rs`) exactly as
   today, so the defaulted manifest is evaluated with no new channel.
2. **Final message + structured result.** A job's final agent message becomes a
   first-class projection (`query job` field, `NodeResult.result`). Flow nodes may declare
   a result schema in spec; the runner validates the structured result before the script
   observes it (invalid ⇒ the node rejects with `result-schema-mismatch` even on verdict
   pass — the pi-appliance validate stage, generalized; validation lives in `tally-flow`
   per §11.1).
   Final-message extraction (normative, per adapter): implemented as a preset scrape
   capture named `finalMessage` (same machinery as `sessionRef`/`usage`, `adapters.rs`;
   FS-6 adds a last-match selection mode where required), evaluated at the existing
   scrape point — daemon-side, at job completion — and persisted with the row's durable
   detail, so the `query job` projection survives daemon restart. Anchors:
   **claude-code** (stream-json): the `result` string of the last event with
   `type == "result"`; **codex** (`exec --json`): the `item.text` of the last
   `item.completed` event whose `item.type == "agent_message"`; **pi** (`--mode json`):
   the text content of the last assistant-role message in the session output;
   **shell**: no capture — the field is absent; stdout is never promoted to a message.
   `NodeResult.result`: the parsed value when the extracted message parses as JSON,
   otherwise the raw string. Absence or non-JSON is an error only when the node declared
   `resultSchema` (then `result-schema-mismatch`); with no declared schema, whatever was
   extracted — or absence — flows through untyped.

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

Feeder mechanics (normative): (a) placement — inside the daemon, at the point the
completed job's `usage` scrape resolves; "built-in" is literal: no new home-manager
unit, no external process; the feeder emits through the identical event schema and
ingestion path the external `TALLY_METER_EVENT_PATH` contract defines. (b) Routing — the
feeder targets every pool the job actually LEASED that is a windowed-consumption budget
pool WITHOUT a declared `usageMeter`; a declared external meter remains that pool's sole
authority (never double-fed). Jobs leasing no such pool feed nothing. (c) Units — the
built-in feeder is token-denominated: amount = the scraped usage object's `total_tokens`
when present, else `input_tokens + output_tokens` (absent terms 0; the claude/codex/pi
usage shapes all carry these names). Amount 0, missing usage, or unparsable usage ⇒ no
event, silently (debug log only — malformed input is ignored per the acceptance law). A
pool routed to the built-in feeder therefore denominates `consumptionCap` in tokens; the
option docs state this. Advisory headroom clamp only, downward only; charges stay pure
per the standing ruling.

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

> **Superseded 2026-07-26:** The no-break reservation above was discharged by
> [issue #84](https://github.com/mecattaf/tally.nix/issues/84), Amendments 1–2. The final witness
> schema replaced the predecessor encoding in place; it introduced no epoch model. See
> [`doc/witness.md`](../doc/witness.md).

## 20. Explicitly out of scope (design chapters reserved, not corners cut)

Witness v2 epoch break; script patching/versioning for in-flight runs; attic-backed evidence
retention via GC roots; query.watch-driven web/TUI; cross-machine witness comparison; microvm
executor tier; multi-tenant anything.

> **Superseded entries 2026-07-26:** [Issue #84](https://github.com/mecattaf/tally.nix/issues/84)
> discharged the witness-schema, store-evidence/GC-root retention, and cross-machine comparison
> reservations. Its amendments make the schema plain rather than epoch-named.
>
> **Further superseded 2026-07-26:** [Issue #71](https://github.com/mecattaf/tally.nix/issues/71)
> admitted the `drv()` dialect surface. Section 11.2.1 is the resulting contract.
