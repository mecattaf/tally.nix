# RPC protocol contract

Tally's public API is the Unix socket spoken by `tally-client`, the CLI, and the flow runner.
It is a local, versioned NDJSON-RPC protocol. There is deliberately no HTTP server, TCP port,
REST facade, connect-time negotiation, or multi-tenant authentication layer. Filesystem
permissions protect the socket; SSH and Unix-socket forwarding are the remote-access story.

This chapter describes the protocol shipped at commit `4c85563`. The advertised table contains
**25 public methods**. Two daemon-internal producer methods also exist in the dispatcher, but
they are not advertised and are not part of this contract.

## Versions at a glance

| Surface | Current version | Where it appears |
|---|---:|---|
| Query schema | `1` | `schemaVersion` on query results |
| Query protocol | `4` | `protocolVersion` on query results and watch records |
| Enqueue response schema | `1` | `schemaVersion` on enqueue and retry results |
| Witness schema | `2` | `schemaVersion` on canonical verdict records |

The transport itself has no version greeting. In particular, the declared
`unsupported_protocol` error code is not a negotiation mechanism: the current daemon never
emits it. A client discovers compatibility from the method it calls and the version fields in
the returned envelope.

## Socket and framing

The CLI resolves its socket in this order:

1. global `--socket PATH`;
2. `TALLY_SOCKET`;
3. `$XDG_RUNTIME_DIR/tally/tally.sock`;
4. `${TMPDIR:-/tmp}/tally/tally.sock` when `XDG_RUNTIME_DIR` is absent.

Each frame is one compact JSON object followed by LF. Requests and responses share one
full-duplex Unix-stream connection:

```json
{"id":"client-7","method":"query.pools","params":{}}
{"id":"client-7","result":{"schemaVersion":1,"protocolVersion":4,"pools":[]}}
```

A request has these fields:

| Field | Type | Contract |
|---|---|---|
| `id` | string or signed 64-bit integer | Required and echoed unchanged in the response. IDs must be unique among that client's outstanding calls. |
| `method` | string | Required; one of the advertised methods below. |
| `params` | JSON value | Optional. Omission is decoded as `{}`. Public methods expect an object. |

A successful response is `{"id": ID, "result": VALUE}`. A failed call is
`{"id": ID, "error": {"code": CODE, "message": STRING, "data": VALUE?}}`. The `data` field is
omitted unless the error carries structured detail.

Malformed JSON that still fits in a frame receives an `invalid_frame` error with `"id": null`;
it cannot be correlated to a request. Empty or all-whitespace input lines are ignored. Clients
should always terminate frames with LF even though EOF after a final unterminated JSON object
is accepted by the current reader.

### The symmetric frame limit

The default `maxFrameBytes` is 16 MiB (`16 * 1024 * 1024`). The count includes the terminating
LF; therefore an encoded JSON value plus LF may be exactly the configured limit, but not one
byte larger. A CR in CRLF also consumes a byte.

The daemon and the reference client both obtain the limit from the rendered `config.json`.
There is no handshake. Both peers must be configured alike:

- the sender rejects an oversized frame before writing it;
- the receiver rejects a frame that grows beyond its own limit;
- an oversized response or request closes the affected connection rather than guaranteeing a
  correlated `frame_too_large` RPC response.

The last point is intentionally less polished than a negotiated protocol. Treat a lost
connection while sending a boundary-sized request as an indeterminate transport failure, then
reconnect and use a deduplication key before retrying a mutation.

## Multiplexing and ordering

One connection serves requests concurrently. Responses carry the request ID because completion
order, and therefore cross-request response order, is unspecified.

The daemon admits at most 64 in-flight request tasks per connection. Once 64 are running it
stops reading another frame until one completes; excess bytes remain in stream order and are
read FIFO. This is backpressure, not an RPC rejection. Long-lived `queue.await_job` calls can
therefore share a connection with queries, but 64 blocked calls consume the entire read window.

The reference `tally-client` keeps one pending-response map per connection and dispatches
responses by ID. A foreign client must do the same; reading “the next response” as the response
to “the last request” is incorrect.

## Compatibility promise

The following rules are the public compatibility policy:

- Existing method names, error-code strings, and existing field meanings are stable.
- Response objects evolve additively. Clients must ignore unknown response fields.
- A removal, type change, or semantic break in a versioned query envelope requires a new
  `protocolVersion`; a breaking record-shape change requires a new `schemaVersion`.
- Request objects are closed to undocumented fields. Most handlers enforce this with
  `deny_unknown_fields`. A few small control handlers currently ignore unknown fields; that is
  an implementation inconsistency, not an extension point.
- Clients send only documented request fields and treat every cursor as opaque.
- There is no promise that a newer request field works against an older daemon. Additive
  evolution is primarily server-to-client; a client that needs a new request must require the
  corresponding protocol version out of band.

The top-level request decoder itself currently ignores extra top-level members. Do not depend
on that either.

## Advertised method table

The table between the markers is checked against the daemon's `RPC_METHODS` constant during
`cargo test`. Keep its first column in advertised order.

<!-- rpc-method-list:start -->
| Method | Params | Result |
|---|---|---|
| `queue.enqueue` | [`EnqueueParams`](#enqueueparams) | [`EnqueueResult`](#enqueue-results) |
| `queue.continue` | `EnqueueParams`, with `resumeFrom` required | `EnqueueResult` |
| `queue.retry` | `{task_uuid: string}` (`taskUuid` is accepted as an alias) | Retry admission object |
| `queue.cancel` | Exactly one of `{task_uuid: string, force?: boolean=false}` or `{flowRunId: UUID}` | Single-job or flow-run cancel result |
| `queue.pause` | `{pool?: string, all?: boolean=false}` | `{paused: string[], affected: integer}` |
| `queue.resume` | `{pool?: string, all?: boolean=false}` | `{resumed: string[]}` |
| `queue.drain` | `{producer?: string}` | Drain result and barrier |
| `queue.await_job` | `{task_uuid?: string, job_id?: string, attempt?: integer}` | Terminal job result |
| `queue.await_barrier` | `{barrier: string}` | Completed barrier result |
| `flow.supersede` | `{flowRunId: UUID, successorFlowRunId: UUID, reason: SupersedeReason}` | `{ok, disposition, record}` |
| `lease.acquire` | `{pool: string \| string[]}` | `{epoch: integer, outcome: AdmitOutcome}` |
| `lease.release` | `{lease: string}` | `{released: LeaseGrant, promoted: LeaseGrant[]}` |
| `lease.status` | `{lease?: string, jobId?: string}` | `LeaseStatus` |
| `query.jobs` | Jobs filters plus `limit` and `cursor` | Paginated job collection |
| `query.job` | `{id: string}` | Job detail |
| `query.run` | `{id: string}` | Compact flow-run status |
| `query.lineage` | `{flowRun: string}` (`id` is accepted as an alias) | Generation lineage of one flow run |
| `query.status` | `{pool?: string}` | Status view |
| `query.storage` | `{}` | Daemon-owned storage metrics and intake state |
| `query.log` | Lifecycle filters plus `limit`, `cursor`, and `after` | Paginated lifecycle collection |
| `query.proof` | `{task: string, attempt?: integer}` or `{flowRun: string}` | Proof view, or a proof collection for a flow run |
| `query.trace` | `{task: string, attempt?: integer, limit?: integer, cursor?: string}` | Paginated trace view |
| `query.producers` | `{name?: string, kind?: string}` | Producer inventory |
| `query.watch` | `{after?: string, limit?: integer}` | Watch envelope |
| `query.render` | `{format?: "text" \| "json", scope?: "all" \| "queue" \| "witness"}` | Render object or a JSON string |
| `query.standup` | `{since?: RFC3339 string, source?: string}` | Stand-up digest |
| `query.pools` | `{}` | Pool-headroom view |
<!-- rpc-method-list:end -->

All integer counts and sequence values are non-negative JSON integers unless a field explicitly
describes an exit code. Timestamps are RFC 3339 strings.

## Queue methods

### `EnqueueParams`

`queue.enqueue` accepts exactly one of `invocation` and non-empty `argv`. `pool` is required for
a new admission and may be a scalar for one pool or an array for several. Pool arrays are
canonicalized into ascending order and duplicates are rejected.

| Field | Type and default | Meaning |
|---|---|---|
| `invocation` | string, optional | Shell-like tokenization performed by tally. Mutually exclusive with `argv`. |
| `argv` | non-empty string array, optional | Direct argument vector. Mutually exclusive with `invocation`. |
| `pool` | string or non-empty string array | Pools leased atomically. May be omitted only by `queue.continue`, which inherits the old job's pools. |
| `executor` | string, optional | Named execution target. |
| `priority` | `interrupt`, `high`, `medium`, or `low`; default `medium` | Admission priority. |
| `adapter` | string; default `shell` | Named adapter. |
| `cwd` | absolute path, optional | Execution working directory. |
| `workspace` | object, optional | `{repo, baseRev, branch, worktreePath}`; all four strings/paths are required together. |
| `adapterOptions` | object, optional | `{prePromptArgv?: string[], environment?: object, approvalPolicy?, sandboxPolicy?, model?, effort?}`. |
| `gateManifest` | object, optional | `{path, requiredGateIds, acceptancePolicy}`. The path is absolute; policy is `manual` or `execution-and-gates`. |
| `brief` / `briefPath` | JSON value / path, optional | Mutually exclusive inline or file-backed structured brief. |
| `resumeFrom` | UUID string, optional | Terminal task being continued; required by `queue.continue`. |
| `source` | source enum; default `manual` | `manual`, `orchestrator`, `calendar`, `events-dir`, `gh`, `build-effect`, or `pool-reachability`. |
| `dedupKey` | string, optional | Submission identity key. |
| `submission` | `{"mode":"full"}`, optional | Selects full disposition semantics. Absence is legacy mode; there are no other mode values. |
| `orchestration` | object, optional | Opaque flow capsule. `flowRunId` must be a UUID; `maxNodes`, when present, is positive; `nodeOrdinal`, when present, is a non-negative integer. Optional `taskRef` is a validated `<campaign>/<task-id>` scalar. |
| `parent` | UUID string, optional | Durable parent task. |
| `evidence` | string array; default `[]` | Canonical evidence specifications. |
| `drv` | object, optional | `{drvPath, outputs:[{name,path}, ...]}` for derivation-aware admission. |
| `evidenceClass` | JSON value, optional | Opaque, witnessed evidence classification. |
| `manifestHash` | string, optional | Manifest identity carried into the witness. |
| `consumptionEstimate` | integer, optional | Admission debit required by a windowed-consumption pool. |
| `runtimeMaxSec` | positive integer, optional | Execution watchdog. |
| `noEnqueue` | boolean; default `false` | Prevents the admitted job from spawning children. |
| `credentials` | object of name to absolute path; default `{}` | Explicit credential sources. Pool credentials are merged and conflicts fail admission. |
| `origin` | object, optional | Versioned admission provenance: `{schemaVersion:1, source, producer?, github?}`. |
| `callerJobId` | string, optional | Caller identity. When `callerJobToken` is also present it must name the identity that token resolves to; otherwise the request is rejected. |
| `callerJobToken` | 64-character lowercase hex string, optional | Daemon-minted capability token a local job receives as `TALLY_JOB_TOKEN`. Presenting it makes the request Job class: the daemon resolves the caller from the token, and the administrative and `__producer.*` method classes are refused. |
| `ghTriggerActor`, `ghSelfActor`, `ghOrigin` | optional | Compatibility fields for the GitHub producer. New clients should prefer `origin`. |
| `taskUuid` | UUID string, optional | Preassigned identity, used by derivation flow nodes. Mutually exclusive with `resumeFrom`. |
| `relatedTrigger` | object, optional | Fallback trigger provenance `{producer,eventId,outcome,receiptId?}`. |
| `wait` | boolean; default `false` | Compatibility hint. The daemon still returns the admission immediately; waiting is a second `queue.await_job` call. |

The canonical payload hash used by full mode covers execution identity: `argv`, canonical pools,
executor, adapter, cwd/workspace, adapter options, gate manifest, evidence, derivation,
evidence/manifest identity, runtime limit, `noEnqueue`, credentials, and `briefHash`. It excludes
scheduling and orchestration metadata such as priority, source, dedup key, parent, flow capsule,
consumption estimate, caller identity, and `wait`.

That exclusion is observable and intentional. A client must not infer that `payloadHash`
authenticates every field in the request.

### Submission modes

Full mode (`submission.mode = "full"`) supplies the disposition table used by flows:

| Disposition | Meaning |
|---|---|
| `created` | A new durable row was admitted. |
| `attached` | An identical live submission already exists; await the returned task/barrier. |
| `reused` | A matching successful witness and its declared artifacts remain usable. |
| `terminal` | The matching prior submission is terminal, including memoized failure. |
| `substituted` | A derivation's outputs were already available; no row or lease was created. |

The same dedup key with a different canonical payload fails with `dedup-key-conflict`. A matching
pass whose artifact or store-path evidence drifted creates fresh work and reports
`reusedRejected` (`artifact-drift`, `declared-hash-mismatch`, `artifact-unavailable`,
`store-path-invalid`, or `store-path-drift`) on the `created` result.

Legacy mode is selected by omitting `submission`. It performs only pass-witness reuse, disables
live attachment and conflict detection, and skips dedup reuse entirely when a gate manifest is
present. The public `tally enqueue` CLI selects full mode by default when `--dedup-key` is
present; `--submission legacy` preserves the omission. Keyless CLI enqueues omit `submission`
regardless of the flag, while the flow runner always selects full mode.

### Enqueue results

Every enqueue result has `schemaVersion: 1`, `disposition`, `task_uuid`, `job_id`, and a state.
Created results also carry a `barrier`; full created results add `payloadHash` and `attempt`.
Attached results add both `task_uuid` and `taskUuid`, `status`, `dedup_key`, `payloadHash`,
`attempt`, and any recorded label/capsule. Terminal/reused/substituted results may add:

```text
verdict, exit_code, artifact_content_hash, store_paths, storePaths, drv,
witness_lsn, witnessSeq, lease_epoch, completion, taskRef, recordedLabel,
recordedOrchestration
```

The duplicate snake_case/camelCase fields are shipped compatibility baggage. A new client should
read the camelCase version when both are present, but tolerate either documented spelling.

`queue.retry` keeps the task UUID, increments `attempt`, and returns
`{schemaVersion:1, retried:true, task_uuid, taskUuid, job_id, barrier, state, status, attempt,
payloadHash?, taskRef?}`. Only a terminal non-pass job with a governing witness can be retried.

`queue.cancel` returns `{ok:true, affected, task_uuid, taskRef?, was, lease_epoch, already_terminal?}`.
A running job is unaffected unless `force` is true. Paused and queued jobs can be cancelled
without `force`. The flow-run form returns `{ok:true, affected, flow_run_id, flowRunId, results}`
and force-cancels every nonterminal child carrying that `flowRunId`; it does not inherit the
single-job form's running-job no-op.

`queue.pause` and `queue.resume` require exactly one of a named `pool` or `all: true`. Pausing
withdraws queued lease requests and changes those jobs to paused; it does not stop running jobs.

`queue.drain` atomically claims pending event-directory ingress, archives each claim as accepted
or rejected, and returns:

```text
{barrier, enqueued, rejected, repaired, represented: 0, outcomes: [...]}
```

If `producer` is supplied, it must name an `events-dir` producer. The returned drain barrier
snapshots all then-active jobs, not only jobs admitted by that one call.

### Blocking awaits and restart behavior

`queue.await_job` requires exactly one of `task_uuid` or `job_id`; optional `attempt` must be
positive. Its terminal result is:

```text
{task_uuid, taskRef?, job_id, verdict, exit_code, artifact_content_hash,
 attempt, lease_epoch, witness_seq, completion?, stderr_excerpt?, stderr_truncated?}
```

The two stderr fields are present for every failed job whose capture is
available. `stderr_excerpt` is the lossy UTF-8 rendering of at most the final
2 KiB of its retained stderr, including any omission marker;
`stderr_truncated` says whether earlier bytes were omitted. The retained `.err`
file is the same bounded UTF-8 diagnostic projection; the byte-authoritative
raw stream remains `.adapter.err`. Successful jobs do not materialize `.err`.

For an active job the waiter is memory-resident. For a completed job the daemon reconstructs the
answer from the verified witness ledger, including after restart. A client connection and its
pending call do **not** survive daemon restart: reconnect and issue `queue.await_job` again for
the same task and attempt. If that requested attempt is older than the row's current attempt,
the daemon follows the current attempt. This keeps a waiter issued for attempt 1 attached when an
automatic bounded requeue has already advanced the same task UUID to attempt 2. A requested
future attempt is not rewritten.

Job barriers have the deterministic form `barrier:<task-uuid>:<attempt>`. They can likewise be
re-armed after restart because the daemon can reconstruct their one result from the witness.
`queue.await_barrier` returns `{barrier, complete:true, results:[...]}`.

Drain barriers have the form `barrier:drain:<daemon-namespace>:<sequence>`. They are
memory-resident snapshots and do not survive restart. After reconnect, issue a new drain or
await the known jobs individually. Completed unclaimed drain barriers are also bounded in
memory, so they are not durable bookmarks.

## Flow methods

`flow.supersede` records that one flow run is terminal and is replaced by a fresh successor.
It writes nothing into either run: the predecessor's rows, witnesses, and history are untouched,
and the successor is not created — only the relationship between them becomes durable, in
`<dataDir>/flow-lineage.jsonl`.

| Field | Type | Meaning |
|---|---|---|
| `flowRunId` | UUID | The terminal run being retired. Must have at least one durable node carrying an `orchestration.scriptHash`; otherwise the call is `not_found`. |
| `successorFlowRunId` | UUID | The fresh run that replaces it. Must differ, and must have no nodes yet. |
| `reason` | `generation-change`, `script-changed`, `args-changed`, `catalog-changed`, or `operator` | Recorded durably for later audit. |

Both IDs are canonicalized to hyphenated lowercase before they are stored or looked up, so the
upper-case, unhyphenated, and braced renderings `Uuid::parse_str` accepts all name one run. Records
written by an earlier tally in another rendering are absorbed by the same canonicalization on read.

The result is `{ok: true, disposition, record}`. `disposition` is `recorded` when a new line was
appended and `reused` when the identical `(flowRunId, successorFlowRunId, reason)` triple was
already durable — the call is idempotent by construction, so a supervisor that crashes between
recording a rollover and acting on it may simply call again. `record` additionally carries
`recordedAt` and the predecessor's own `predecessorScriptHash`, `predecessorArgsHash`, and
`predecessorCatalogHash`, read from the abandoned run's durable rows rather than from the caller.

These are refused with `flow-lineage-conflict`:

- a second, different successor for a run that already has one, or the same successor under a
  different reason — a durable rollover is never rewritten;
- a successor already claimed by another predecessor;
- a rollover that would close a cycle in the chain;
- a predecessor with unfinished nodes (cancel the run first);
- a successor that already has nodes;
- a predecessor whose own rows disagree about a pinned hash.

`query.lineage` requires a UUID and answers `invalid_params` otherwise, so a mis-rendered lookup
cannot read as a well-formed "not superseded". Both it and `query.run` fail with
`flow-lineage-unusable` when the durable index holds a complete record that cannot be decoded; an
interrupted final append is skipped instead, and truncated by the next write.

`query.lineage` answers the read side for any run, including one with no recorded rollover:

```text
schemaVersion, flowRunId, superseded, supersededBy?, supersedes?, chain[], currentFlowRunId
```

`chain` is the whole generation chain oldest-first and always contains `flowRunId`;
`currentFlowRunId` is its tip, which is the run an operator or supervisor should actually start.
A run with no lineage answers `superseded: false`, `chain: [flowRunId]`, and
`currentFlowRunId: flowRunId` rather than `not_found`.

`query.run` carries the same two records as optional `supersededBy` and `supersedes` fields, and
reports `state: "superseded"` for a retired run regardless of its own node verdicts.

## Lease methods

`lease.acquire` creates an explicit reservation token rather than a daemon-owned execution. It
uses interrupt priority but is outside managed hard preemption. Its `AdmitOutcome` is either a
granted `LeaseGrant` or a queued ticket and position. A grant contains:

```text
leaseId, jobId, unit, pools, priority, epoch, grantedAt,
admissionKey?, consumptionEstimate?
```

`lease.release` releases a held token and returns any grants promoted by that release.
`lease.status` accepts exactly one non-empty `lease` or `jobId` and returns
`{leaseId, epoch, held, yieldRequested, yieldDeadline?}`.

These methods do not provide a sanctioned way for a flow runner to hold an additional lease for
its whole run. The declarative flow runner is admitted separately and, as shipped, always uses
the single `flow` pool.

## Query methods

All query objects currently report `schemaVersion: 1` and `protocolVersion: 4`. Important common
shapes are:

- collection: `{schemaVersion, protocolVersion, items, nextCursor, truncated, elidedItems,
  snapshot}`, plus `position`, optional `positionGap`, and — when a `flowRun` filter was
  supplied — `flowRunTasks` on `query.log` and `query.jobs`;
- snapshot: `{createdAt, cursor, history, witnessHead:{seq,hash}}`;
- job detail: `{schemaVersion, protocolVersion, job, attempts, snapshot}`;
- run status: `{schemaVersion, protocolVersion, flowRunId, flowName?, campaign?, repository?,
  state, counts, tasks, currentNodes, failures, snapshot}`;
- pool view: `{schemaVersion, protocolVersion, pools}`;
- proof: `{schemaVersion, protocolVersion, taskUuid, attempt, leaseEpoch, status,
  witnessExpected, witnessRecord, authorship?, evidence, advisoryAttestations, ledger, history}`.

`query.jobs` accepts these optional filters:

```text
liveState (alias state), terminalVerdict (alias verdict), pool, executor,
adapter, source, origin, parent, flowRun, session, since, until, limit, cursor
```

`since` and `until` are RFC 3339 timestamps. `terminalVerdict` is one of the witness verdicts.
`flowRun` matches `orchestration.flowRunId`, which is how `tally query jobs --flow-run ID` groups
a run's nodes. Each job summary includes its durable identity, optional top-level `taskRef`, and admission fields, live and
terminal state, parent/children, provenance with authority labels, evidence, timestamps,
resource use, artifact/witness facts, authorship when present, and trace availability. Two of the
admission fields are `dedupKey`, the node's submission identity, and `disposition`, which is
`created`, `reused`, or `substituted` for the admission that wrote the row. Admissions that write
no row — `attached`, and full-mode `reused` and `terminal` — are reported by the flow runner's
`node-submitted` and `node-terminal` lifecycle events instead.

`query.run` accepts a flow-run UUID as `id` and returns the compact state needed during an
operator check. `currentNodes` carries node label, task reference, live state, start time,
elapsed seconds, configured `runtimeMaxSec`, and `budgetRemainingSeconds`. That remainder is
signed: a node past its budget reports the overrun as a negative value rather than saturating at
zero. `failures` carries the failed stage, canonical verdict, attempt/epoch, retained
failure-capture path when present, and the bounded stderr tail. For the built-in `spec-build`
flow, the latest schema-validated reconciliation result also supplies the full campaign task
table. Its tasks are classified as `done`, `running`, `blocked`, or `pending`, with unresolved
dependencies, current/failing node, and merged pull request where available. Other flows still
receive current nodes and failures but have an empty task table; for those runs `state` reaches
`complete` when every admitted node holds a passing terminal verdict on its current attempt.

`query.run` also returns `usage`: what the run cost, summed per attempt from the advisory
attestation ledger — keyed by `taskUuid`/`attempt`/`leaseEpoch` — over the run's durable
membership, so every attempt of a retried task **that the ledger holds** is charged — not only the
task's latest, which is all its durable row keeps — and a node the run was handed but whose row
names its creating run is inside the sum. The ledger is the whole of what the rollup can see; it
covers every attempt the ledger could speak for and no more. See [Usage
rollups](#usage-rollups).

### Usage rollups

A rollup is `advisory-provider-capture`: harnesses reporting on themselves, never a bill tally
verified. `provenance` and `composition` state where the numbers came from and exactly what the
total is a sum over, on the wire rather than only in this document.

`tokens` carries `inputTokens`, `cacheReadTokens`, `cacheWriteTokens`, `outputTokens`, and
`reasoningTokens`, each as a `{value, attempts}` pair so a component no harness in the run reports
is visibly summed over fewer attempts than the rest. Three compositions are fixed and must not be
re-derived by a consumer:

- `freshInputTokens` is `inputTokens + cacheWriteTokens`, and it is the cross-harness uncached
  prompt volume. `inputTokens` alone is not: claude-code's `cache_creation_input_tokens` are
  fresh, uncached prompt tokens its `input_tokens` excludes, so summing `inputTokens` alone
  understates a cache-writing harness by its entire cache-write volume while producing a figure
  that looks directly comparable to codex's. Its `attemptsComplete`/`attemptsPartial` split marks
  attempts that reported only one of the two halves, whose share of the value is a floor.
- `reasoningTokens` is nested inside `outputTokens` and is never added to any total.
- `inputTokensAsReported` is not summed at all: each harness's own input convention means a
  different thing, and the two are not commensurable.

`totalTokens` sums each attempt's own total and names its `source`. A total is
`harness-reported` only when the adapter declared a `totalTokens` field mapping and the harness
filled it; otherwise tally derives it from the components and grades it
`derived-from-components`. **No shipped preset declares `totalTokens`** — codex's real
`turn.completed` carries no `total_tokens` at all, and claude-code's `result` event carries a
cumulative usage object whose members are components, not a total — so every run over the shipped
presets reads `derived-from-components` today, including a run spanning both. `harness-reported`,
and the `mixed` grade a run combining the two kinds produces, are reachable through an
operator-defined adapter that declares the mapping. A consumer should branch on the field rather
than on which harness it believes ran.

`coverage` is the statement that keeps a partial sum from reading as a total. It counts member
`tasks`, `tasksWithReportedUsage`, `tasksWithoutAttestation`, `attemptsObserved`, and then the
attempts apart: `attemptsReported`, `attemptsReportedWithoutFigures`,
`attemptsReportedWithComponents`, `attemptsNotReported` (a usage scrape was declared and the
stream carried none), `attemptsNotDeclared` (the adapter declared no usage scrape), and
`attemptsWithoutUsageRecord` (an attestation predating the usage record). `attemptsReportedWithoutFigures` is the subset of `attemptsReported` that contributed
nothing: the harness reported usage and **no** declared field path resolved — absence is not
unreadability, so nothing lands in `unreadableFields` and the observation is still `reported`.
Those attempts raise `reported-without-figures` rather than being counted as covered. That bucket
is **total** drift only; a harness that renames one key is at least as likely, and it lands
elsewhere, because the attempt still contributes.

Drift in one key is what the per-component `attempts` counts are for, and the rollup now reads
them: when any of the four components the total is a sum of — `inputTokens`, `cacheReadTokens`,
`cacheWriteTokens`, `outputTokens` — was reported by fewer attempts than
`attemptsReportedWithComponents`, that component's sum is over a subset of those attempts and the
rollup raises `partial-components`. On a real claude-code capture, one renamed
`cache_read_input_tokens` takes 97% of the run's tokens out of the total while every other figure
still resolves, so this is the difference between a partial sum that says so and one that does
not. A consumer diagnosing the caveat compares the per-component `attempts` against
`attemptsReportedWithComponents`; the drifted component is the one below it.

Two exclusions from that check, both deliberate. `reasoningTokens` is not in it: claude-code
reports no reasoning figure and it enters no total, so checking it would fire on every claude run.
And the denominator is `attemptsReportedWithComponents` rather than `attemptsReported`, which
excludes attempts whose harness stated a total of its own and reported no component beside it —
the shape an adapter declaring only a `totalTokens` mapping produces. Such an attempt declared no
components to be missing, so judging it against a component threshold would mark a run that
reported everything it intended to as permanently incomplete.

That exemption is one **reported** shape wide, which is not the same promise as "an adapter that
declared components is always judged", and the difference matters: the rollup reads attestations,
never the adapter's declared field map, so an adapter that declared components *and* a total,
whose harness renamed every component key at once, reports exactly the exempted shape and leaves
the denominator. `total-only-attempts` is what stops that passing silently. It is raised whenever
an exempted attempt sits beside attempts that did report components — `attemptsReported -
attemptsReportedWithComponents > 0` and `attemptsReportedWithComponents > 0` — because then the
component sums demonstrably cover fewer attempts than the total does, whichever kind of adapter
produced them. An attempt that reported any component is judged by the component threshold as
usual, even when its harness also stated a total. The one case reported evidence cannot separate
is a run in which *every* attempt is total-only: a legal total-only adapter and a wholly drifted
component adapter are indistinguishable there without the declared field set, which the
attestation does not carry.

`ledgerVerified` is false when the advisory chain did not verify or could not be read, and then
nothing is summed at all rather than answered as a zero. Every reason the sums are partial also
appears as a named entry in `caveats`; an empty `caveats` array claims only that the rollup covers
every attempt the ledger could speak for.

`cost` is the harness's own `costUsd`, summed over the attempts that reported one. Its `basis`
field states what it is not: tally's cgroup `charge` is a distinct quantity, is not summed here,
and is a floor that includes tally's own exit-recorder overhead.

`query.log` filters by `task`, `flowRun`, `attempt`, `session`, lifecycle `event`, `source`,
`since`, and `until`, and accepts optional `provenance` and `after`. A `flowRun`-scoped response
also reports `flowRunTasks`; see [Flow-run membership](#flow-run-membership). `after` is a durable
lifecycle-stream position, described under [Lifecycle stream
positions](#lifecycle-stream-positions) below; it is not `since`, which remains a wall-clock time
filter, and it is not `cursor`, which is an ephemeral page offset. Lifecycle items expose `taskRef` when their durable row/witness did. A lifecycle event carries no orchestration capsule, so `flowRun` is resolved
to the run's task UUIDs through the durable membership ledger, the durable rows, and the witness
chain. A `failed` lifecycle item
includes `stderrTail` and `stderrTruncated`, bounded as described for terminal waits above.
Omitting `provenance` preserves the original RPC behavior and returns the source provenance
stream. The CLI sends `provenance:false` for its default human renderer and `--json`; the daemon
then collapses terminal journal/witness pairs and suppresses evidence echoes before pagination.
CLI `--provenance` sends `true` and preserves the source stream unchanged.
`query.trace` requires `task` and optionally selects `attempt`. Both return collection
envelopes; trace also includes a `generations` array describing capture capability, completeness,
retained range, byte count, truncation, and redaction provenance. Trace items and generation
summaries both include optional `taskRef`.

`query.proof` selects either a task with an optional attempt, or a `flowRun`; supplying both, or
neither, is an invalid request, and `attempt` applies only to a task. Its `status` is `verified`,
`no-witness-expected-yet`, or `proof-missing`. The selected canonical witness is returned
verbatim as `witnessRecord`; advisory attestations and their authority are separate. A `flowRun`
request returns a collection envelope whose `items` hold one proof per node in node-ordinal
order, and an unknown run is `unknown job`.

`query.status` combines pool headroom, the legacy job projection, and the same `storage` object
returned by `query.storage`. `query.pools` returns only headroom. A pool item includes capacity/held/queued counts, remaining capacity, optional
window-consumption counters and reset, utilization percentages, and the `GO`, `SLOW`, or `STOP`
signal.

`query.storage` takes no parameters. It returns allocated and apparent bytes, file counts,
allocated-size warning/hard thresholds, free-space warning/hard thresholds, and `ok`, `warning`,
or `hard` level for `dataDir` and `stateDir`. `sampledAt` dates the cached tree walk;
`freeSpaceCheckedAt` dates the most recent periodic or per-intake filesystem-free probe.
`schemaVersion` is 3; the section version 2 carried for the live task-database projection was
removed outright with that projection rather than being emitted as nulls, and the CHANGELOG entry
for the removal names the exact fields. `growthPerCompletion` is the signed
byte delta divided by the canonical witness-count delta since the prior completion sample;
it is absent until two completion boundaries have been observed. `intake.accepting=false` means
only new `queue.enqueue`/`queue.continue` requests are refused. Already-admitted work, retries,
cancellation, and all queries remain available.

`query.producers` filters configured producers by exact `name` and/or `kind`. It returns their
unit identity, schedule, rendered enqueue summaries, and observed runtime state. It is not
cursor-paginated despite carrying `nextCursor: null`.

`query.render` accepts `scope` over `all`, `queue`, or `witness`. With `format: "json"` or no
format it returns the object. With `format: "text"` it returns a JSON **string** containing a
pretty-printed JSON object; the CLI unwraps that string before printing.

`query.standup` returns a window, completed and in-flight entries, reuse and gate-failure
summaries, cancellations, and canonical GPU seconds. The RPC accepts a `source` filter even
though the current CLI exposes only `--since`. Each completed, in-flight, gate-failed, and
cancelled entry includes optional `taskRef`. `runs` carries one `{flowRunId, usage}` entry per
flow run the window touched, whose `usage` is the same rollup `query.run` returns — see [Usage
rollups](#usage-rollups) — with one wire difference: the three fixed statements every entry would
otherwise repeat verbatim (`provenance`, `composition`, and `cost.basis`) are stated once in the
digest-level `usageBasis` object as `{provenance, composition, costBasis}` and omitted from each
entry. They are safe to state once because each has a single writer assigning a compile-time
constant with no dependence on the run; the omission is nevertheless conditional, so an entry whose
statement ever differs from the digest's carries its own inline and a reader must prefer the
entry's. Where an entry omits one, its value is the digest's `usageBasis` — that object is the
**producer's** statement of what its own rollups summed, so a reader must take an omitted field
from it and must not substitute a constant of its own, which would differ silently whenever the
reader and the daemon are different generations. `usageBasis` is present exactly when `runs` is
non-empty, and both are omitted from the wire when empty — so a digest that carries no `usageBasis`
carries no `runs` either, and there is no entry for a fallback to be applied to. That happens three
ways: the window touched no flow run, reader-state hid every run it did have (`archivedRunsHidden`
is then non-zero, and is what distinguishes the two), or the producer predates the field. Only in
that last case does a reader fall back to what it knows, and it cannot tell which case it is
holding — it does not need to, because there are no entries to fill. `query.run` returns one
rollup and keeps all three inline. A run is touched when the window holds an entry for a task it created
*or* a task its durable membership names, so a run that only attached a node is still listed. The
window selects which runs appear; it does not narrow what is summed, because a run's cost is a
property of the run and a window-narrowed sum would shrink with the window while still being
labelled the run's total.

### Pagination cursors

Only `query.jobs`, `query.log`, and `query.trace` use page cursors. The default limit is 100 and
the allowed range is 1–1,000. A page result is capped at 48 KiB.

Every page carries two completeness fields alongside `nextCursor`:

| Field | Meaning |
|---|---|
| `truncated` | `true` exactly when this response is not the whole filtered window. A reader that checks nothing else still cannot mistake a capped page for a quiet run. |
| `elidedItems` | How many items on this page were served with fields cut down to fit. |

The CLI's default (non-`--json`) `query jobs` and `query log` print a *merged* envelope
assembled by walking every page, in the same shape as a single page. On that output
`elidedItems` is the sum across every page walked, so it can exceed the per-page maximum of
one described below, and `truncated` is always `false` because the walk finished.

An item that alone exceeds the 48 KiB cap is not an error and does not end the query. Its largest
string fields are truncated — largest first, 256 bytes retained, the remainder replaced by
`…<N bytes elided>` — and the item gains an `elided` object naming what was cut:

```json
{"fields": ["/argv/3"], "originalBytes": 208913, "reason": "item exceeded the bounded response size"}
```

`fields` are JSON Pointers into the item. Only a page's leading item can be elided, so
`elidedItems` is never more than one per page. An item that is oversized because of its
*structure* rather than its text cannot be shrunk this way and is still an `internal`
`one collection item exceeds the bounded response size` error.

The first call creates an in-memory snapshot. Subsequent calls must repeat the same method and
filters with the returned cursor; `limit` may change. A cursor:

- is bound to the method and filter fingerprint;
- expires when its snapshot is evicted (only 32 are retained) or the daemon restarts;
- yields `invalid_params` when malformed or used with different filters;
- yields `not_found` when its snapshot expired.

There is no durable recovery for a page cursor. Start the query again.

### Lifecycle stream positions

`query.log` additionally reports `position`, a **durable** coordinate in the lifecycle stream:

```text
log-v1:<lifecycle-sequence>:<witness-sequence>
```

Both components are zero-padded to 20 digits, and both name append-only durable sequences — the
lifecycle history and the witness ledger. Unlike a page cursor, a position survives a daemon
restart and page-cache eviction, so an external poller can hold one between polls. Unlike a watch
cursor it names the lifecycle stream, not the change feed; feeding a page or watch cursor to
`after` is rejected as `invalid lifecycle stream position` rather than being misread.

`position` is the **head** of the stream at projection time, not the newest matched item, so
`after: <position>` with an empty `items` array is a proof of quiet for that filter rather than
merely an absence of matches. Advance the held position only from a response whose `truncated` is
`false`; a truncated response has not shown you everything before the head it reports.

If the requested position predates what durable history still retains, the response carries

```text
positionGap: {requested, earliestAvailable}
```

and the window is a partial continuation: events between the retained floor and the request are
gone.

`position` is the head of the **whole** lifecycle stream, not of the caller's filter. A
filtered poll therefore sees it advance whenever anything else on the daemon does; empty
`items` is the signal that nothing matched after the held position, and `position` is what the
caller carries forward.

### Flow-run membership

Run membership is a durable admission fact. Every admission carrying an orchestration capsule
appends `{schemaVersion, flowRunId, taskUuid, disposition, nodeOrdinal?, nodeLabel?, recordedAt}`
to `<data-dir>/flow-membership.jsonl` and fsyncs it before the admission is acknowledged, for all
five dispositions: `created`, `attached`, `reused`, `terminal`, and `conflict` — a conflict admits
nothing and therefore records nothing. `nodeOrdinal`/`nodeLabel` are the submitting run's, which
for a row-less admission is the only place they are written down at all. An admission carrying no
capsule writes nothing and does not create the file.

The ledger is checked for readability and appendability **before** the kernel commits, so a
damaged or unwritable ledger refuses a flow admission outright, leaving no row, no lifecycle
event, and no dispatch. Which task UUID a run is handed is not known until the admission has been
decided, so the write itself necessarily follows the commit; if the ledger becomes unusable in
that window, the admission is **acknowledged with the degradation named** rather than denied:

```json
{"disposition": "created", "task_uuid": "...",
 "membershipDegraded": {"flowRunId": "...", "taskUuid": "...", "admitted": true,
                        "reason": "...", "resolution": "repair-flow-membership-ledger"}}
```

`membershipDegraded` is absent on every ordinary response. Denying an admission whose work is
already dispatching would orphan it; what is degraded is one node's visibility in one run's
window until the ledger is repaired. The daemon also journals
`flow-membership-degraded flowRunId=… taskUuid=…` so the affected set survives the response.

A `flowRun` filter resolves to the **union** of that ledger and the original scan of durable row
details and witness records for a capsule naming the run. The union is what keeps an upgrade
safe: a row written before the ledger existed still resolves from its capsule exactly as it did.

This replaces the pre-#380 behaviour, in which membership was recomputed per call from the scan
alone. An admission that writes no row — `attached`, and full-mode `reused` and `terminal` —
handed the caller a task UUID whose row, and whose membership, belonged to whichever run created
it, and events for that task were filtered out of the submitting run's window entirely, with no
page cap involved: same items, `nextCursor: null`, while the work ran.

Every run-scoped `query.log` and `query.jobs` response reports the resolved membership:

```text
flowRunTasks: <count of task UUIDs this flowRun resolved to>
```

The field is absent when no `flowRun` filter was supplied. `flowRunTasks: 0` means the daemon
holds no membership for that run ID — usually a mistyped or stale ID, but also a repaired or
deleted ledger, a compacted-out idle run, or an admission that reported `membershipDegraded`. The
CLI says so on stderr.

An unterminated final line in the ledger is an interrupted append and is skipped on read and
truncated on the next append. A *complete* record that cannot be decoded fails the query with
`resolution: repair-flow-membership-ledger` rather than silently answering with a smaller run. A
record written by a **newer** daemon does not: unknown fields, an unknown `disposition`, and a
higher `schemaVersion` are all read on the fields this daemon understands, so a pin rollback
cannot take run-scoped queries out. Only a `schemaVersion` *below* the reader's is refused.

The ledger is compacted when it passes 20,000 records — one per admitted flow node — by dropping
whole runs down to 18,000, **least-recently-touched first**, and never a run holding a task that
has not completed — running, queued, or paused alike — or the run whose record is being written. Never part of a run either, because a
partially-present run reports a membership count lower than the truth. Down to a low-water mark
rather than to the bound, so a compaction is followed by thousands of ordinary appends rather than
by another compaction. Compaction is a write-and-rename, so a reader sees the whole old ledger or
the whole new one, and it re-emits fields and disposition values written by a newer daemon
verbatim rather than stripping them. If nothing is evictable — every run over the bound holds work that has not
completed — the ledger exceeds its target rather than deleting membership that is in use, growing
by one record per flow admission, announcing it on the daemon journal each time, and compacting
on the next admission after that work finishes. A run dropped by
compaction falls back to the row scan, which for a row-less node is nothing.

### Watch cursors

`query.watch` is different. Changes live in a private durable `changes.jsonl` capped at 4,096
records. Limits are again 1–1,000 and each result is capped at 48 KiB.
The file is a non-evidence feed rather than a recovery input: startup replaces
the whole feed with an empty one if its records cannot be decoded or validated.

A call without `after` is a tail subscription seed: it returns no items and a `nextCursor` at
the current head. Call again with that cursor to receive later changes. A normal envelope has
`status: "ok"` and includes `earliestAvailableCursor`, `latestCursor`, `retentionLimit`, and
the next cursor. If retained history has overtaken the caller, the method returns a successful
envelope with:

```text
status: "cursor-expired"
termination: {condition: "gap", reason: "cursor-expired"}
resumeAfterCursor: <cursor immediately before the earliest retained record>
```

The client must decide whether losing that gap is acceptable before resuming. A cursor ahead of
the log is `invalid_params`. Watch records are typed as `job`, `lifecycle`, `trace`, `proof`,
`pool`, or `producer`.

## Error envelope

Every declared wire code, its current emission status, and CLI mapping is listed in
[Exit codes and error taxonomy](errors.md). In particular, do not collapse
`dedup-key-conflict`, `flow-node-cap`, `flow-lineage-conflict`, `flow-lineage-unusable`,
`storage-budget-exceeded`, `storage-monitor-unavailable`, or `not_found` into a generic retry:
each carries a different recovery decision.
