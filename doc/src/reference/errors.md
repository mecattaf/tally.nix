# Exit codes and error taxonomy

Tally has three related but distinct failure surfaces:

1. RPC calls return a typed error object.
2. Flow checking/running returns a structured `FlowError`.
3. The CLI converts those failures, and sometimes terminal job verdicts, to a process exit code.

Do not infer one surface from another without the mapping below. In particular, numeric CLI exit
codes 3 and 4 are deliberately overloaded.

## RPC errors

The wire error object is:

```json
{
  "code": "invalid_params",
  "message": "pool set must contain at least one pool",
  "data": {"optional": "method-specific detail"}
}
```

`data` is omitted when absent. These are all declared code strings:

| Code | Meaning and current behavior | CLI exit |
|---|---|---:|
| `unsupported_protocol` | Reserved for an unsupported negotiated version. The current transport has no negotiation and the daemon does not emit this code. | 1 |
| `invalid_params` | The params object cannot decode or violates method/admission validation. This includes most unknown request fields. | 2 |
| `invalid_frame` | A size-valid request line is not a decodable request frame. Its response ID is `null`. | 1 |
| `frame_too_large` | Declared transport code. The current framing path normally reports oversize as a connection-level `WireIoError`, not a correlated RPC error. | 1 |
| `unknown_method` | Method string is not handled. | 1 |
| `not_found` | Requested job, barrier, lease, cursor snapshot, proof attempt, trace, or pool was not found. | 4 |
| `unsupported` | Reserved for a recognized but unsupported capability. The current daemon does not emit it. | 1 |
| `internal` | Failed invariant, corrupt durable data, I/O failure, invalid projection, or another failure that is not the caller's parameter error. | 1 |
| `timeout` | Declared transient code used by the flow client's retry classification; the current daemon does not emit it. | 1 |
| `epoch_changed` | Declared transient lease-generation code used by the flow client's retry classification; current daemon lease errors are mapped differently and do not emit it. | 1 |
| `dedup-key-conflict` | Full-mode dedup key already governs a different canonical payload. | 1 |
| `flow-node-cap` | Admitting the node would exceed the capsule's run-scoped `maxNodes`. | 1 |
| `flow-lineage-conflict` | A `flow.supersede` call contradicts durable lineage: the run already has a different successor, the successor already has a predecessor, the rollover would close a cycle, the predecessor still has unfinished nodes, the successor has already started, or the predecessor's own rows disagree about a pinned hash. | 1 |
| `flow-lineage-unusable` | The durable lineage index holds a complete record that cannot be decoded or validated. Every flow start reads that index, so this blocks flow runs until the file is repaired. Carries `data.transient: false` and `data.resolution: "repair-lineage-ledger"`. | 1 |
| `storage-budget-exceeded` | A daemon-owned store crossed its allocated-byte hard limit or its filesystem fell below `minimumFreeBytes`. New intake is refused; admitted work and queries continue. | 1 |
| `storage-monitor-unavailable` | The cached monitor reports an I/O or state failure and cannot make a safe budget decision. New intake is refused; admitted work and queries continue. | 1 |

The CLI mapping is intentionally narrow: only `invalid_params` and `not_found` get dedicated
codes. All other RPC errors exit 1. A daemon socket that cannot be reached exits 3 before any
RPC response exists.

### Structured conflict data

`dedup-key-conflict` carries enough `data` to diagnose live and terminal collisions:

```text
dedupKey
payloadHash
existing[]: {taskUuid, payloadHash, orchestration, nodeLabel?}
liveTaskUuids[]
existingTaskUuid? / existingPayloadHash? / existingOrchestration? / existingLabel?
```

The singular compatibility fields appear only when exactly one candidate exists.

`flow-node-cap` carries:

```json
{"flowRunId":"...","maxNodes":1000,"existingNodes":1000}
```

These are semantic failures. Blindly retrying the same request will reproduce them.

`storage-budget-exceeded` carries the active warning episode and the complete cached
`query.storage` snapshot in `data.storage`. Free space by applying the declared retention policy
or raise the budget deliberately; do not blindly retry while `data.storage.intake.accepting` is
false. `storage-monitor-unavailable` carries the same snapshot, with `monitorError` identifying
why measurement failed. Repair that error rather than treating it as a crossed byte threshold.

Public `tally enqueue --dedup-key KEY` requests use full submission mode by default, so a live
same-key/different-payload collision reports `dedup-key-conflict` on stderr and exits 1.
`--submission legacy` retains the compatibility behavior without live conflict detection.

## CLI process exits

The general CLI contract is:

| Exit | Meaning |
|---:|---|
| 0 | Command succeeded; for waited jobs, verdict is `pass`, `reused`, or `substituted`. |
| 1 | Generic command/RPC failure, a failed waited job, invalid witness, comparison divergence, or an unclassified flow failure. |
| 2 | CLI/request usage failure, RPC `invalid_params`, selected flow startup identity failures, or invalid witness-comparison inputs. |
| 3 | Daemon socket unreachable **or** a waited job ended `clean-exit-no-artifact`. |
| 4 | RPC `not_found` **or** a waited job was `cancelled`. |
| 10 | Flow script, evaluation, determinism, or runtime-bound failure. |
| 20 | Flow replay-integrity failure. |

Clap parse errors also exit 2; `--help` and `--version` exit 0. Generic configuration and local
I/O failures normally exit 1.

### Admission is not completion

`tally enqueue` without `--wait` exits 0 once admission succeeds, even if the job later fails.
With `--wait`, the CLI issues `queue.await_job`, prints the terminal object, and maps its verdict:

| Terminal verdict | Exit |
|---|---:|
| `pass`, `reused`, `substituted` | 0 |
| `failed`, `pool-vanished`, `preempted`, `runtime-exceeded` | 1 |
| `clean-exit-no-artifact` | 3 |
| `cancelled` | 4 |
| Unknown verdict | 1 |

If a waited result has no `verdict` but has `exit_code`, that integer is clamped into 0–255 and
used. If neither exists, the command exits 1.

There is an awkward CLI distinction: `tally queue await-job ID` is a raw RPC printer and exits 0
when the await call succeeds, even when the returned job verdict is failure. Only
`enqueue --wait` and `queue continue --wait` translate the terminal verdict into the process
status. Scripts using `queue await-job` must inspect its JSON.

`tally attest exec` is another exception. It runs the child and propagates the child's numeric
exit code; on Unix, a signal becomes `128 + signal`. A failure to append the advisory
attestation is logged but does not replace the child's status.

## Structured flow errors

Both the checker and runner use:

```json
{
  "name": "FlowReplayError",
  "code": "script-changed-mid-run",
  "message": "flow run ... is pinned to ...",
  "location": {"line": 1, "column": 1},
  "ordinal": 7,
  "details": {"recordedHash": "...", "currentHash": "..."},
  "stack": "optional JavaScript stack"
}
```

Optional fields are omitted, and `details` is abbreviated above — an exit-20 code carries the
whole family contract described below. `location` is one-based. The run command writes lifecycle JSONL to
stdout; on failure it emits a `{"type":"flow-failed","error":...}` event, then returns the same
structured report through the CLI error path on stderr.

Exit classification uses the stable `code` field, not `name`:

| Exit | Flow codes |
|---:|---|
| 2 | `flow-run-id-missing`, `flow-run-id-invalid`, `runner-identity-invalid`, `runner-identity-incomplete`, `workload-mutex-parent-required` |
| 4 | `flow-cancelled` |
| 10 | `script-syntax`, `script-encoding`, `script-evaluation`, `script-exception`, `unhandled-rejection`, `determinism-violation`, `iteration-cap`, `runtime-limit`, `microtask-budget`, `wall-clock-budget` |
| 20 | `replay-divergence`, `script-changed-mid-run`, `args-changed-mid-run`, `catalog-changed-mid-run`, `flow-run-superseded` |
| 1 | Every other flow error code, including admission, catalog, schema, node, capture, and client failures |

Exit 10 groups bugs or bounded failures in the script/evaluator. Exit 20 means continuing the
same run would contradict already recorded identity: the same ordinal resolved to different
canonical work, or the script, arguments, or catalog identity changed after the run began.
Automation should stop and investigate rather than retry either class in place.

### Branching on a failure without reading prose

An unattended supervisor must be able to tell a permanent refusal from a transient daemon or
transport failure without parsing a message. Every classified flow error carries two `details`
fields for exactly that:

| Field | Meaning |
|---|---|
| `transient` | `true` when repeating the identical command can produce a different answer; `false` when it cannot. |
| `resolution` | The bounded operation that clears it: `retry`, `supersede`, `run-successor`, `investigate`, or `repair-lineage-ledger`. |
| `remedy` | The `tally flow supersede` invocation that clears this run, with the successor UUID left as a placeholder because it must be persisted before the call. It is <!-- remedy-nullity:start -->`null` when no single command does — including when no `flowRunId` is known, since the command needs one<!-- remedy-nullity:end -->. |

The classification is fixed per code and is the same wherever the error was raised — the
runner's own startup scan, a daemon refusal handed back mid-run, or the client's translation of
an RPC code. One wire code never has two `details` contracts.

The five exit-20 codes go further: they share **one** `details` shape as a family, so a monitor
reads the same fourteen members whichever one fired and wherever it fired. The members are
listed in
[one `details` shape for every exit-20 refusal](../flows/submission-and-replay.md#one-details-shape-for-every-exit-20-refusal);
all fourteen are always present, and `null` means this code has nothing to say through that
member. The column below names what each code populates.

<!-- supersession-code-rows:start -->
| Code | `transient` | `resolution` | Populates |
|---|---|---|---|
| `script-changed-mid-run` | `false` | `supersede` | `flowRunId`, `divergentInput: "script"`, `recordedHash`, `currentHash`, `remedy` |
| `args-changed-mid-run` | `false` | `supersede` | `flowRunId`, `divergentInput: "args"`, `recordedHash`, `currentHash`, `remedy` |
| `catalog-changed-mid-run` | `false` | `supersede` | `flowRunId`, `divergentInput: "catalog"`, `recordedHash`, `currentHash`, `remedy` |
| `flow-run-superseded` | `false` | `run-successor` | `flowRunId`, `successorFlowRunId`, `reason`, `recordedAt` |
| `replay-divergence` | `false` | `investigate` | `flowRunId`, `divergentInput: "payload"`, `recordedHash`, `currentHash`, `recordedLabel`, `currentLabel`, `taskUuid`, and `kernelError` when a kernel dedup-key conflict revealed it. A rollover does **not** clear it: the same ordinal re-derived different work, which is a question about the script or the configuration. |
<!-- supersession-code-rows:end -->

Every other classified code carries the two branching fields and nothing more:

| Code | `transient` | `resolution` | Populates |
|---|---|---|---|
| `script-history-conflict`, `args-history-conflict`, `catalog-history-conflict` | `false` | `investigate` | The run's own history already holds more than one hash. |
| `flow-lineage-conflict` | `false` | `investigate` | The supersede contradicts durable lineage. |
| `flow-lineage-unusable` | `false` | `repair-lineage-ledger` | The durable lineage index holds an undecodable complete record. |
| `daemon-unreachable`, `daemon-timeout`, `daemon-epoch-changed` | `true` | `retry` | The codes the flow client's own re-arm classification already retries. |

**A code carrying neither field is unclassified — not transient.** Absence means tally has no
recommendation for that code, so treat it as an escalation rather than assuming a retry is safe.

#### The recipe that actually works

A supervisor that persists one run ID per work item can recover unattended, but two details of
the real behaviour matter and are easy to get wrong:

1. **Persist the successor UUID before calling `flow.supersede`.** Idempotency is keyed on the
   identical `(flowRunId, successorFlowRunId, reason)` triple. Minting a *fresh* UUID on every
   attempt is not idempotent: the second call is a `flow-lineage-conflict`, because the run
   already has a durable successor and a rollover is never rewritten. Mint the successor once,
   write it down, then call.
2. **On `flow-lineage-conflict`, read `query.lineage` and adopt `supersededBy`.** That is the
   recovery for a supervisor that crashed after calling but before persisting: the answer is
   already durable, and `currentFlowRunId` names the run to start.
3. **Cancel a live predecessor first.** The incident shape is a runner stopped *mid-flow*, so its
   run usually still has unfinished nodes. `flow.supersede` refuses those with
   `flow-lineage-conflict` rather than stranding them; issue `queue.cancel` with the `flowRunId`
   and then supersede.
4. **A run with no durable node is refused as `not_found`.** Such a run never recorded a script
   hash, so it can never trip an identity pin and never needs retiring; the refusal is how a
   typo'd or mis-rendered run ID is caught instead of being recorded against nothing.

Run IDs are canonicalized to hyphenated lowercase on both write and read, so an upper-case,
unhyphenated, or braced rendering names the same run everywhere.

Nothing else about exit 20 changed: replay is still refused, and neither the old run nor its
history is ever rewritten.

Two edges of the exit mapping are worth knowing before you build automation on it.

`script-history-conflict`, `args-history-conflict`, and `catalog-history-conflict` are raised when
a run's own recorded history already contains more than one hash. They carry the `FlowReplayError`
name, so they read like the exit-20 family, but they are not in the exit-20 list and **exit 1**.
Branch on the `code`, not on the `name` and not on the exit code alone. Their `transient` /
`resolution` pair says the same thing the exit code does not: permanent, and not a supersede.

`runtime-limit` is assigned to every `RangeError`, including one the script threw itself. A
deliberate `throw new RangeError("page out of range")` is therefore reported as a runtime-limit
breach and exits 10, indistinguishable from an engine backstop. Express a domain outcome as a
validated result envelope rather than a thrown error; see
[Two more cookbook recipes](../flows/cookbook.md#expressing-domain-failure).

The flow client translates notable RPC codes to flow codes before this exit mapping:

| RPC | Flow code |
|---|---|
| `dedup-key-conflict` | `dedup-key-conflict` (or `replay-divergence` when same-run evidence proves that case) |
| `flow-node-cap` | `flow-node-cap` |
| `storage-budget-exceeded` | `storage-budget-exceeded` |
| `storage-monitor-unavailable` | `storage-monitor-unavailable` |
| `invalid_params`, `not_found` | `admission-denied` |
| `frame_too_large` | `frame-too-large` |
| `unsupported_protocol` | `unsupported-protocol` |
| `unknown_method`, `unsupported` | `flow-protocol-unavailable` |
| `timeout` | `daemon-timeout` |
| `epoch_changed` | `daemon-epoch-changed` |
| `invalid_frame`, `internal` | `daemon-protocol-error` |

These translated codes are exit 1 unless they become an exit-20 replay code.

## Witness verification problems

The canonical witness verifier reports a list rather than one wire error. Problem kinds are:

| Kind | Meaning |
|---|---|
| `parse-error` | Line I/O, JSON, blank-line, or LF-termination failure. |
| `invalid-record` | Non-canonical bytes/order or a record invariant failed. |
| `schema-version-invalid` | `schemaVersion` is not 2. |
| `record-type-invalid` | `recordType` is not `verdict`. |
| `hash-mismatch` | Stored hash differs from recomputation; the line was changed. |
| `prev-hash-mismatch` | The predecessor link does not equal the preceding stored hash. |
| `seq-order` | Sequence did not strictly increase in file order. |
| `seq-gap` | The set is not contiguous from 1. |
| `seq-duplicate` | A sequence appears more than once. |

`tally witness verify` prints the full report before exiting 1, so callers should preserve
stdout as evidence. `tally witness compare` exits 1 for observed divergence and, under
`--strict`, for unattested canon or orphan attestations; malformed input chains are usage/input
failure and exit 2.
