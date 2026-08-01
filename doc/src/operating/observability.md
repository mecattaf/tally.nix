# Query and observability

The query surface joins four different kinds of fact without pretending they
have equal authority:

- acknowledged enqueue rows are durable admission facts;
- `lifecycle.jsonl` contains tally's execution observations;
- `witness.jsonl` is the canonical terminal record; and
- attestations and provider captures are advisory.

When they disagree, the query keeps the disagreement visible and the witness
wins for canonical verdict and usage. Querying does not mutate a job.

## Monitor tally's own disk

Use the daemon's measured view before workload-side free-space guards:

```console
$ tally query storage | jq '{intake, dataDir, stateDir, taskchampion, growthPerCompletion}'
```

The two store sizes use allocated filesystem blocks for budget decisions and also expose
apparent bytes and file counts. Each store reports `filesystemAvailableBytes`,
`warningFreeBytes`, and `minimumFreeBytes`; falling below the first emits an early warning and
falling below the second is hard pressure even when the store's own allocated bytes are small.
`taskchampion` separates `databaseBytes`, `walBytes`, and `shmBytes`, then reports `taskCount` and
the append-only SQLite `operationHighWater`. A projection read failure is visible as `readError`;
the total-store and free-space decisions remain authoritative.

Directory measurement is an off-thread, cached sample. `sampledAt` is the tree/SQLite age
boundary; `freeSpaceCheckedAt` is the latest cheap filesystem probe. `query storage` and
`query status` return the cache without filesystem work. Every enqueue performs only `statvfs`,
updates the free-space fields and pressure state, and never walks either tree. The periodic timer
starts one sample on every configured interval when the previous single-flight sample is done;
there is no second elapsed-time guard. If a walk overruns an interval, the next idle tick starts
the next walk. The blocking walk does not occupy the daemon's current-thread runtime, accept loop,
intake path, completion path, lease tick, or watchdog, and a blocking-worker panic cannot take
ownership of or permanently lose the monitor.

`growthPerCompletion` compares samples across canonical witness-count boundaries. Signed byte
rates make both growth and successful compaction visible. `query status` embeds the same object
under `storage`, so a future human-oriented status surface can consume it without another disk
contract.

Warning and hard transitions are fsynced to `<dataDir>/storage-warnings.jsonl` and emitted on the
daemon's journal stream. A level recovers only after allocated bytes fall below 90% of the crossed
threshold. Free space must rise above the crossed threshold by the larger of 10% or 1 GiB; this
absolute band prevents shared-filesystem noise from repeatedly closing and reopening an episode.
Warning-to-hard-to-warning changes stay in one episode until full recovery, so GitHub campaign
intake with evidence receipts enabled gets at most one idempotent issue comment for that pressure
episode.

At a hard size or free-space threshold, tally rejects only new enqueue and continuation requests
with `storage-budget-exceeded`; admitted work, retry, cancel, pause, resume, and every query remain
available. If measurement itself fails, the same intake scope is refused with the distinct
`storage-monitor-unavailable` code and `monitorError` explains the failure. Concurrent removal of
a directory below either store is treated as a normal vanished entry, not a monitor outage.

`<dataDir>/storage-metrics.json` is derived advisory state. Unsupported schema versions, foreign
fields, malformed JSON, and inconsistent episode fields are ignored at startup, journaled, and
replaced by a fresh sample. The durable warning log supplies the next episode sequence so a reset
does not collide with an earlier campaign receipt.

## Find the task anchor

`tally enqueue` returns a task UUID. Keep it: query pages call it the `anchor`,
and it remains stable across attempts even when a live systemd job ID changes.
If all you have is the live job ID, `query job` accepts that too and resolves
the task anchor.

Campaign nodes additionally expose an optional human `taskRef`, such as
`crm/t07`. It is diagnostic provenance, not a replacement for the UUID.

```console
$ tally query jobs --state running --limit 100
$ tally query jobs --state queued --pool worker-gpu
$ tally query job <task-or-live-job-uuid>
```

`query jobs` can filter by verdict, pool, executor, adapter, source, origin,
parent, flow run, session, and time. Its JSON response contains `items`,
`nextCursor`, and immutable snapshot metadata. When `nextCursor` is non-null,
pass it back with the same filters:

```console
$ tally query jobs --source orchestrator --limit 100 --cursor '<opaque-cursor>'
```

A cursor is bound to its original method, filters, and snapshot. Do not edit it
or reuse it with a different query.

## Follow a flow

Every child admitted by a flow carries an orchestration capsule. Group those
nodes without matching descriptions or argv:

```console
$ tally query jobs --flow-run <flow-run-uuid>
```

Each item exposes top-level `taskRef` when present, `orchestration.flowRunId`, the node ordinal, the orchestration
`scriptHash`, `argsHash`, and `catalogHash`, pool, executor, parent task, live
state, and terminal facts. The runner itself is the parent row and is not one of
the orchestrated child items unless it also carries that capsule.

Two fields make a replay auditable rather than merely asserted:

| Field | Meaning |
|---|---|
| `dedupKey` | The node's submission identity. A flow-local `key` is rendered `flow:<run-id>:k:<key>`; a node without one is `flow:<run-id>:<ordinal>`; a `dedupKey` set by the script is used verbatim. |
| `disposition` | How this row came to exist: `created`, `reused`, or `substituted`. |

`disposition` covers only admissions that write a row. An `attached` answer joins
a job already in flight, and a full-mode `reused` or `terminal` answer returns a
governing witness — none of the three writes a second row, which is exactly what
a correct replay looks like from here. Those answers appear in the runner's own
lifecycle stream instead; see [Follow a flow run's
nodes](#follow-a-flow-runs-nodes) below.

To verify that a re-run replayed rather than re-executed: the node count and the
`dedupKey` set must be unchanged, and every `disposition` must still read
`created` from the first run.

## Follow a flow run's nodes

The flow runner writes one JSON object per line. Alongside `log`,
`selector-resolved`, and `flow-completed`, it emits a pair of events per node:

| Event | Emitted | Carries |
|---|---|---|
| `node-submitted` | When the daemon answers the node's enqueue | `ordinal`, `dedupKey`, `label`, `taskRef`, `disposition`, `taskUuid`, `payloadHash`, `attempt` |
| `node-terminal` | When the node's terminal result is observed | `ordinal`, `dedupKey`, `taskRef`, `disposition`, `taskUuid`, `verdict`, `witnessSeq`, `exitCode`, `errorCode` |

Unlike `log`, these are never suppressed on replay: a replayed prefix reporting
`reused` is the fact an operator needs to see. `node-submitted` follows admission
order and `node-terminal` follows the replay-stable observation order.

The same run ID filters the two ledgers:

```console
$ tally query run <flow-run-uuid>
$ tally query log --flow-run <flow-run-uuid>
$ tally query proof --flow-run <flow-run-uuid>
```

Start with `query run` when the question is “what is happening now?” It shows the
spec-build reconciler's task table, any current nodes with elapsed time and remaining runtime
budget, and failure capture pointers plus stderr tails. Use `--json` when a steering agent needs
the same compact view as structured data.

`query log` restricts the lifecycle stream to the run's nodes, resolved from the
orchestration capsule on the durable rows and the witness chain, because a
lifecycle event carries no capsule of its own. `query proof` returns one proof
per node in node-ordinal order under an `items` array, rather than requiring the
task UUIDs the operator is trying to discover; it is mutually exclusive with
`--task`, and `--attempt` applies only to `--task`.

Both spellings of the run ID work everywhere: `--flow-run` and `--flow-run-id`
are aliases on `tally query jobs`, `tally query log`, `tally query proof`, and
`tally flow run`.

For one node, inspect all attempt lanes:

```console
$ tally query job <task-uuid>
```

The `attempts` array retains each observed `(taskUuid, attempt, leaseEpoch)`
lane. This is the useful view after a retry or daemon restart because it does
not overwrite the earlier attempt.

## Read the final agent message

The built-in `pi`, `claude-code`, and `codex` adapters declare a
`finalMessage` scrape. After the terminal acknowledgement, tally projects it
onto the job:

```console
$ tally query job <task-uuid> | jq -r '.job.finalMessage.value'
```

That field is the first-class result; there is no need to search raw JSONL
captures for the last provider event. Its authority is
`advisory-provider-capture`, not canonical evidence. A flow agent node receives
the same projected value as `NodeResult.result`. If a configured projection
does not appear within ten seconds of terminal acknowledgement, the flow node
reports `result-projection-timeout` rather than silently returning an empty
result.

Shell output is not automatically a trace or a final message. A custom adapter
must declare the scrape or trace explicitly.

## Inspect proof

```console
$ tally query proof --task <task-uuid>
$ tally query proof --task <task-uuid> --attempt 2
$ tally witness verify
```

`query proof` returns the selected full witness record, evidence observations,
separate advisory-attestation references, the verified chain head, and one of
three statuses:

- `verified`: a canonical witness exists and the chain verifies;
- `no-witness-expected-yet`: the selected attempt is not terminal; or
- `proof-missing`: tally observed a terminal condition but cannot find the
  witness it should have.

`proof-missing` is an incident. Preserve the data directory and inspect daemon
logs; do not manufacture a replacement record. `tally witness verify` checks
the ledger offline and should be part of restart and deployment verification.

## Read lifecycle and provider traces

Lifecycle events and provider output are separate:

```console
$ tally query log --task <task-uuid> --attempt 2 --limit 100
$ tally query log --task <task-uuid> --attempt 2 --json
$ tally query log --task <task-uuid> --attempt 2 --json --provenance
$ tally query trace --task <task-uuid> --attempt 2 --limit 100
```

The default log is a terse human transition view. It suppresses evidence observations and
collapses a terminal journal record with its canonical witness, so “started” and “passed” are
not repeated just because tally retained both authorities. `--json` retains the structured
fields with the same collapse. `--provenance` restores every journal, evidence, and witness echo
for an audit. The underlying RPC remains tally's uncollapsed durable observation history.

The trace is exposed only when
the adapter declares a JSON-lines provider stream. Trace records preserve
provider order, parsed JSON when valid, raw text, and base64 for non-UTF-8
bytes. Both each trace record and each generation summary expose `taskRef` when
the attempt belongs to a campaign task. The response also says whether the
generation is complete, unavailable, unsupported, or truncated. A running remote trace can honestly report
`remote-live-trace-unavailable`; it is never presented as an empty successful
trace.

For a campaign node, every journal/lifecycle record carries
`TALLY_TASK_REF=crm/t07`, its `MESSAGE` includes `taskRef=crm/t07`, and the
`query log` projection exposes `taskRef: "crm/t07"`. The same value is exported
to the child as `TALLY_TASK_REF`.

A `failed` log item carries `stderrTail` and `stderrTruncated`. The tail is a
lossy UTF-8 rendering bounded to 2 KiB including the omission marker; it is a
diagnostic projection, not evidence. Read it first. Inspect the retained raw
capture only when the bounded tail is insufficient.

The `completed`, `inFlight`, `gateFails`, and `cancelled` entries returned by
`query standup` likewise expose `taskRef`, so the campaign digest does not
require a UUID-to-worklist lookup.

Query reads at most 16 MiB from one capture generation. Larger local capture
files remain on disk, but the trace reports
`query-read-truncated-at-16777216-bytes`. Remote capture transfer is also
bounded to 16 MiB per stream.

## Resume a watch

`query watch` emits one JSON record per line for job, lifecycle, trace, proof,
pool, and producer changes:

```console
$ tally query watch
```

With no cursor it starts at the current tail. Save the `cursor` from the last
record you processed and resume after a disconnect:

```console
$ tally query watch --after 'change:00000000000000001234'
```

The durable change log retains the latest 4,096 records. If a reader falls
behind, tally returns `status: "cursor-expired"` with
`earliestAvailableCursor`, `resumeAfterCursor`, and an explicit `gap`
termination. Treat that as a missed interval: take a fresh `query jobs`
snapshot, then start a new watch. Do not pretend the stream was continuous.

## Where the files live

| Data | Home Manager default | NixOS default | Retention |
|---|---|---|---|
| Witness and attestation ledgers | `~/.local/share/tally/` | `/var/lib/tally/data/` | Append-only |
| Lifecycle history and watch log | same data directory | same data directory | Lifecycle compacts an old prefix after `lifecycleMaxBytes`, preserving `lifecycleHorizon`; watch keeps 4,096 records |
| Enqueue events, captures, unit exits, meters | `~/.local/state/tally/` | `/var/lib/tally/state/` | Selected sets only; see retention policy |
| Current stdout/raw adapter stderr | Ordinary: `<stateDir>/capture/<uuid>.out` and `.adapter.err`; task-ref node: `<uuid>.<task-id>.out` and `.adapter.err` | same layout | Accumulates |
| Failure-only stderr | Ordinary: `<stateDir>/capture/<uuid>.err`; task-ref node: `<uuid>.<task-id>.err`; atomic UTF-8 projection capped at 2 KiB, only present after `failed` | same layout | Current generation remains; archived copy follows the archive horizon |
| Older attempt captures | Ordinary: `<stateDir>/capture/archive/<uuid>/`; task-ref node: `archive/<uuid>.<task-id>/`; each retains the same stream distinction | same layout | Pruned by `captureArchiveHorizon` (30 days by default) on the coordinator |
| Worker-side remote state | configured executor `stateDir` | configured executor `stateDir` | Accumulates on the worker |

`.adapter.err` may contain benign adapter-runtime chatter on a healthy job;
the presence of current-generation `.err` is the failure signal. `.err` is not
a second raw stream. These files are private implementation storage.
Prefer the query API: it validates authority, attempt identity, bounds, and
pagination that a direct file read would have to reconstruct. Capacity planning
and the one managed GC path are covered in
[Retention and growth](retention.md).
