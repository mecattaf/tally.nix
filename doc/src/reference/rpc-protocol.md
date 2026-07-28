# RPC protocol contract

Tally's public API is the Unix socket spoken by `tally-client`, the CLI, and the flow runner.
It is a local, versioned NDJSON-RPC protocol. There is deliberately no HTTP server, TCP port,
REST facade, connect-time negotiation, or multi-tenant authentication layer. Filesystem
permissions protect the socket; SSH and Unix-socket forwarding are the remote-access story.

This chapter describes the protocol shipped at commit `4c85563`. The advertised table contains
**23 public methods**. Two daemon-internal producer methods also exist in the dispatcher, but
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
| `lease.acquire` | `{pool: string \| string[]}` | `{epoch: integer, outcome: AdmitOutcome}` |
| `lease.release` | `{lease: string}` | `{released: LeaseGrant, promoted: LeaseGrant[]}` |
| `lease.status` | `{lease?: string, jobId?: string}` | `LeaseStatus` |
| `query.jobs` | Jobs filters plus `limit` and `cursor` | Paginated job collection |
| `query.job` | `{id: string}` | Job detail |
| `query.status` | `{pool?: string}` | Status view |
| `query.log` | Lifecycle filters plus `limit` and `cursor` | Paginated lifecycle collection |
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
| `orchestration` | object, optional | Opaque flow capsule. `flowRunId` must be a UUID; `maxNodes`, when present, is positive; `nodeOrdinal`, when present, is a non-negative integer. |
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
witness_lsn, witnessSeq, lease_epoch, completion, recordedLabel,
recordedOrchestration
```

The duplicate snake_case/camelCase fields are shipped compatibility baggage. A new client should
read the camelCase version when both are present, but tolerate either documented spelling.

`queue.retry` keeps the task UUID, increments `attempt`, and returns
`{schemaVersion:1, retried:true, task_uuid, taskUuid, job_id, barrier, state, status, attempt,
payloadHash?}`. Only a terminal non-pass job with a governing witness can be retried.

`queue.cancel` returns `{ok:true, affected, task_uuid, was, lease_epoch, already_terminal?}`.
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
{task_uuid, job_id, verdict, exit_code, artifact_content_hash,
 attempt, lease_epoch, witness_seq, completion?}
```

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

- collection: `{schemaVersion, protocolVersion, items, nextCursor, snapshot}`;
- snapshot: `{createdAt, cursor, history, witnessHead:{seq,hash}}`;
- job detail: `{schemaVersion, protocolVersion, job, attempts, snapshot}`;
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
a run's nodes. Each job summary includes its durable identity and admission fields, live and
terminal state, parent/children, provenance with authority labels, evidence, timestamps,
resource use, artifact/witness facts, authorship when present, and trace availability. Two of the
admission fields are `dedupKey`, the node's submission identity, and `disposition`, which is
`created`, `reused`, or `substituted` for the admission that wrote the row. Admissions that write
no row — `attached`, and full-mode `reused` and `terminal` — are reported by the flow runner's
`node-submitted` and `node-terminal` lifecycle events instead.

`query.log` filters by `task`, `flowRun`, `attempt`, `session`, lifecycle `event`, `source`,
`since`, and `until`. A lifecycle event carries no orchestration capsule, so `flowRun` is resolved
to the run's task UUIDs through the durable rows and the witness chain. `query.trace` requires `task` and optionally selects `attempt`. Both return collection
envelopes; trace also includes a `generations` array describing capture capability, completeness,
retained range, byte count, truncation, and redaction provenance.

`query.proof` selects either a task with an optional attempt, or a `flowRun`; supplying both, or
neither, is an invalid request, and `attempt` applies only to a task. Its `status` is `verified`,
`no-witness-expected-yet`, or `proof-missing`. The selected canonical witness is returned
verbatim as `witnessRecord`; advisory attestations and their authority are separate. A `flowRun`
request returns a collection envelope whose `items` hold one proof per node in node-ordinal
order, and an unknown run is `unknown job`.

`query.status` combines pool headroom and the legacy job projection. `query.pools` returns only
headroom. A pool item includes capacity/held/queued counts, remaining capacity, optional
window-consumption counters and reset, utilization percentages, and the `GO`, `SLOW`, or `STOP`
signal.

`query.producers` filters configured producers by exact `name` and/or `kind`. It returns their
unit identity, schedule, rendered enqueue summaries, and observed runtime state. It is not
cursor-paginated despite carrying `nextCursor: null`.

`query.render` accepts `scope` over `all`, `queue`, or `witness`. With `format: "json"` or no
format it returns the object. With `format: "text"` it returns a JSON **string** containing a
pretty-printed JSON object; the CLI unwraps that string before printing.

`query.standup` returns a window, completed and in-flight entries, reuse and gate-failure
summaries, cancellations, and canonical GPU seconds. The RPC accepts a `source` filter even
though the current CLI exposes only `--since`.

### Pagination cursors

Only `query.jobs`, `query.log`, and `query.trace` use page cursors. The default limit is 100 and
the allowed range is 1–1,000. A page result is capped at 48 KiB; an individual item too large to
fit is an `internal` error.

The first call creates an in-memory snapshot. Subsequent calls must repeat the same method and
filters with the returned cursor; `limit` may change. A cursor:

- is bound to the method and filter fingerprint;
- expires when its snapshot is evicted (only 32 are retained) or the daemon restarts;
- yields `invalid_params` when malformed or used with different filters;
- yields `not_found` when its snapshot expired.

There is no durable recovery for a page cursor. Start the query again.

### Watch cursors

`query.watch` is different. Changes live in a private durable `changes.jsonl` capped at 4,096
records. Limits are again 1–1,000 and each result is capped at 48 KiB.

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
`dedup-key-conflict`, `flow-node-cap`, or `not_found` into a generic retry: each carries a
different recovery decision.
