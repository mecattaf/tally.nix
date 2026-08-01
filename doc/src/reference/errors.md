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
| `storage-budget-exceeded` | A daemon-owned store crossed its hard budget, or the monitor cannot safely determine current usage. New intake is refused; admitted work and queries continue. | 1 |

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

`storage-budget-exceeded` carries the active warning episodes and the complete `query.storage`
snapshot in `data.storage`. Free space by applying the declared retention policy or raise the
budget deliberately; do not blindly retry while `data.storage.intake.accepting` is false.

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

Optional fields are omitted. `location` is one-based. The run command writes lifecycle JSONL to
stdout; on failure it emits a `{"type":"flow-failed","error":...}` event, then returns the same
structured report through the CLI error path on stderr.

Exit classification uses the stable `code` field, not `name`:

| Exit | Flow codes |
|---:|---|
| 2 | `flow-run-id-missing`, `flow-run-id-invalid`, `runner-identity-invalid`, `runner-identity-incomplete`, `workload-mutex-parent-required` |
| 4 | `flow-cancelled` |
| 10 | `script-syntax`, `script-encoding`, `script-evaluation`, `script-exception`, `unhandled-rejection`, `determinism-violation`, `iteration-cap`, `runtime-limit`, `microtask-budget`, `wall-clock-budget` |
| 20 | `replay-divergence`, `script-changed-mid-run`, `args-changed-mid-run`, `catalog-changed-mid-run` |
| 1 | Every other flow error code, including admission, catalog, schema, node, capture, and client failures |

Exit 10 groups bugs or bounded failures in the script/evaluator. Exit 20 means continuing the
same run would contradict already recorded identity: the same ordinal resolved to different
canonical work, or the script, arguments, or catalog identity changed after the run began.
Automation should stop and investigate rather than retry either class in place.

Two edges of that mapping are worth knowing before you build automation on it.

`script-history-conflict`, `args-history-conflict`, and `catalog-history-conflict` are raised when
a run's own recorded history already contains more than one hash. They carry the `FlowReplayError`
name, so they read like the exit-20 family, but they are not in the exit-20 list and **exit 1**.
Branch on the `code`, not on the `name` and not on the exit code alone.

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
