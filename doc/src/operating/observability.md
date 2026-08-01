# Query and observability

The query surface joins four different kinds of fact without pretending they
have equal authority:

- acknowledged enqueue rows are durable admission facts;
- `lifecycle.jsonl` contains tally's execution observations;
- `witness.jsonl` is the canonical terminal record; and
- attestations and provider captures are advisory.

When they disagree, the query keeps the disagreement visible and the witness
wins for canonical verdict and usage. Querying does not mutate a job.

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
$ tally query log --flow-run <flow-run-uuid>
$ tally query proof --flow-run <flow-run-uuid>
```

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
$ tally query trace --task <task-uuid> --attempt 2 --limit 100
```

The log is tally's durable observation history. The trace is exposed only when
the adapter declares a JSON-lines provider stream. Trace records preserve
provider order, parsed JSON when valid, raw text, and base64 for non-UTF-8
bytes. The response also says whether the generation is complete, unavailable,
unsupported, or truncated. A running remote trace can honestly report
`remote-live-trace-unavailable`; it is never presented as an empty successful
trace.

For a campaign node, every journal/lifecycle record carries
`TALLY_TASK_REF=crm/t07`, its `MESSAGE` includes `taskRef=crm/t07`, and the
`query log` projection exposes `taskRef: "crm/t07"`. The same value is exported
to the child as `TALLY_TASK_REF`.

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
| Lifecycle history and watch log | same data directory | same data directory | Lifecycle is unbounded; watch keeps 4,096 records |
| Enqueue events, captures, unit exits, meters | `~/.local/state/tally/` | `/var/lib/tally/state/` | No general automatic pruning |
| Current stdout/stderr | Ordinary: `<stateDir>/capture/<uuid>.out` and `.err`; task-ref node: `<uuid>.<task-id>.out` and `.err` | same layout | Accumulates |
| Older attempt captures | Ordinary: `<stateDir>/capture/archive/<uuid>/`; task-ref node: `archive/<uuid>.<task-id>/` | same layout | Accumulates |
| Worker-side remote state | configured executor `stateDir` | configured executor `stateDir` | Accumulates on the worker |

These files are private implementation storage. Prefer the query API: it
validates authority, attempt identity, bounds, and pagination that a direct file
read would have to reconstruct. Capacity planning and the one managed GC path
are covered in [Retention and growth](retention.md).
