# Witness format and verification

The canonical witness is an append-only, hash-chained NDJSON ledger at
`$XDG_DATA_HOME/tally/witness.jsonl` (normally
`~/.local/share/tally/witness.jsonl`). It records terminal verdict facts. The current and only
supported record schema is **2**.

There is no v1/v2 reader pair, epoch-named ledger, boundary record, or on-read conversion. A
daemon that finds the predecessor format fails closed with an archive-aside command. Once the
operator moves those old bytes, the current ledger starts from sequence 1. Archived predecessor
state is inert forensic material; current commands do not interpret it.

## Authority

The coordinator's `witness.jsonl` is the single-writer canon. Two other chains are deliberately
advisory:

- `attestations.jsonl` stores arbitrary adapter observations;
- each execution host may own `exec-attestations.jsonl`, whose typed payload independently
  observes one execution.

All three are tamper-evident hash chains, but none is signed. Hash continuity proves that the
bytes have not changed relative to the chain head you trusted; it does not authenticate who
wrote them. Execution attestations exist to expose agreement or divergence with coordinator
canon, not to create another canonical writer.

## Canonical verdict record

Each physical line is exactly one compact UTF-8 JSON object followed by LF. Blank lines,
pretty-printing, a missing final LF, reordered known fields, and explicit `null` optionals are
non-canonical.

The known top-level fields have this order and shape:

| Field | Required | Type and meaning |
|---|---|---|
| `schemaVersion` | yes | Integer `2`. |
| `recordType` | yes | String `verdict`; no other canonical record type exists. |
| `transitionTimestamp` | yes | Canonical RFC 3339 UTC timestamp with milliseconds, for example `2026-07-26T12:34:56.789Z`. |
| `taskUuid` | no | UUID for the durable task. |
| `verdict` | yes | One of the verdict strings below. |
| `exitCode` | yes | Signed process/result exit code. |
| `artifactContentHash` | no | Lowercase `sha256:` plus 64 hexadecimal digits. |
| `storePaths` | no | Non-empty byte-ascending, unique array of valid Nix store paths. |
| `drv` | no | `{drvPath, outputs:[{name,path}, ...]}`; output names are sorted and unique. |
| `gpuSeconds` | no | Finite, non-negative seconds the unit's main process ran, for a **declared** `vram`-resource pool (see below); a lower bound on lease occupancy, not GPU compute time. |
| `wallClock` | yes | Finite, non-negative seconds. |
| `attempt` | yes | Positive integer. |
| `leaseEpoch` | yes | Positive integer. |
| `dedupKey` | no | Admission deduplication key. |
| `payloadHash` | no | Full-submission canonical payload hash. |
| `briefHash` | no | Structured-brief content hash. |
| `origin` | yes | Versioned admission origin object. |
| `orchestration` | no | Flow provenance capsule, preserved as submitted. |
| `laborClass` | yes | `fresh`, `recovered`, `reused`, or `substituted`. |
| `traceRef` | no | Advisory trace/session reference. |
| `pools` | yes | Non-empty canonical sorted array, even for a single pool. |
| `executor` | no | Safe configured executor name. |
| `hostId` | no | Bounded execution-host identifier. |
| `charge` | no | `{unit, amount, class}` resource charge — cgroup CPU-seconds, including tally's own exit recorder (see below). |
| `model` | no | Canonical model observation, where one is trustworthy enough to witness. |
| `evidenceClass` | no | Opaque evidence classification, hash-covered. |
| `manifestHash` | no | Opaque manifest identity, hash-covered. |
| `completion` | no | Versioned execution/gates/acceptance result. |
| `error` | no | Bounded structured tally-side terminal cause `{code,message,details?}`. It is present only with verdict `failed`; executor request rejection uses code `executor-validation-failed`. |
| `resultRevision` | no | Lowercase 40- or 64-character Git object ID. |
| `authorship` | no | Git AI binding status and hashes. Requires `resultRevision`. |
| `authorshipSessions` | no | Sorted, unique 1–16 observations `{tool,id,model}`. Requires suitable `authorship`. |
| `seq` | yes | Contiguous sequence starting at 1. |
| `prevHash` | yes | Previous record's `hash`, or the genesis value on sequence 1. |
| `hash` | yes | SHA-256 of this record's canonical hash input. |

Optional fields are omitted, never written as `null`. Unknown additive top-level fields are
accepted and retained in an extension map. They remain in their original object position and
are covered by `hash`.

The verdict vocabulary is:

```text
pass
clean-exit-no-artifact
failed
cancelled
reused
pool-vanished
preempted
runtime-exceeded
substituted
```

`reused` must pair with `laborClass: "reused"`; `substituted` must pair with
`laborClass: "substituted"`. Both require `exitCode: 0`. A substituted derivation record is
stricter still: it has task/store/derivation evidence, attempt and lease epoch 1, zero wall
clock, the `build` pool, and no artifact hash, GPU use, or charge.

`error` records a cause diagnosed by tally itself, not stderr synthesized from
the payload. Its code is lowercase kebab-case (at most 64 bytes), its non-empty
message is at most 4 KiB, and optional object details encode to at most 16 KiB.
It is part of the canonical hash input and therefore survives daemon restart
and terminal-result reconstruction even when failure occurred before a capture
generation existed.

### What `gpuSeconds` and `charge` measure

Both are filled by the exit recorder from one `systemctl show` of cgroup accounting
properties, issued while the transient unit is still queryable
(`ExecStopPost`, before `--collect` can garbage-collect it). Neither field observes a GPU;
there is no vendor-neutral GPU accounting in systemd to read.

`gpuSeconds` is the unit's **main-process wall-clock runtime**
(`ExecMainExitTimestampMonotonic − ExecMainStartTimestampMonotonic`), for a pool the operator
explicitly configured `resource = "vram"` — a job holding that pool while idle bills the same
as one saturating the device. It is deliberately not CPU-cgroup time, which would understate a
GPU-bound job that spends most of its wall clock waiting on the device. It is a **lower bound**
on how long the job actually held the pool's lease, not the lease span itself: the lease is
held from admission through completion handling, which strictly contains the main process's
lifetime, so `gpuSeconds` understates true occupancy by a fixed per-job overhead — in the
same reassuring-shortfall direction as `charge`'s recorder floor below. It is gated on the
pool's *declared* resource, not the default: a pool whose config omits `resource` entirely
never carries `gpuSeconds`, even though `vram` is `resource`'s default value for every other
admission decision. Saying nothing about a pool's resource is not the same fact as declaring
it a GPU pool.

`charge` is the unit's whole-cgroup `CPUUsageNSec`, converted to seconds, for any pool
regardless of resource kind. Because the exit recorder itself runs inside that cgroup as
`ExecStopPost`, the figure includes the recorder's own CPU and the `systemctl` subprocess it
spawns — a fixed overhead on the order of single-digit milliseconds, which dominates the
charge on very short jobs and is proportionally negligible on longer ones. Nothing in the
tree aggregates `charge` into a bill yet; the lane that does should decide whether to
subtract a recorder baseline or state this as the number's known floor.

### Nested origin

`origin` has `schemaVersion: 1`, a `source`, and optional `producer` and `github` detail:

```json
{"schemaVersion":1,"source":"calendar","producer":{"name":"monthly","kind":"calendar"}}
```

Sources are `manual`, `orchestrator`, `calendar`, `events-dir`, `gh`, `build-effect`, and
`pool-reachability`. Producer kind must agree with source. GitHub detail is allowed only for
`source: "gh"` and must agree with the generic producer identity.

### Semantic completion

When present, `completion` keeps three truths separate:

```text
schemaVersion: 1
execution: {status, exitCode?, reason}
gates: {status, artifact?, gates, missingRequiredGateIds?, manifestError?}
acceptance: {status, policy, reason}
```

Execution status is `success` or `failure`; gate status is `pass`, `fail`, or `not-run`;
acceptance status is `pending`, `accepted`, or `rejected`. An absent gate manifest is visibly
`not-run`, not a fabricated pass and not an execution failure.

## Flow orchestration capsule

A node admitted by the shipped flow runner carries this capsule through the durable row and into
the canonical witness:

```text
flowName, flowRunId, scriptHash, argsHash, catalogHash, nodeOrdinal, nodeLabel?, taskRef?, maxNodes,
promptRevision?, skillRevision?, selection?
```

`scriptHash` is SHA-256 over the exact flow source bytes. `argsHash` covers the runner's compact
JSON serialization of parsed arguments. `catalogHash` covers the exact catalog bytes and is
`null` when no catalog was supplied. Before evaluating a run, the runner queries existing nodes
for that `flowRunId`. If any recorded identity differs from the current script, arguments, or
catalog, it stops with the corresponding `*-changed-mid-run` code (CLI exit 20) before admitting
more work. That refusal reports the disagreement as `details.recordedHash` and
`details.currentHash` — the same members, in the same shape, whether the runner's startup scan
or an admission mid-run raised it; see
[one `details` shape for every exit-20 refusal](../flows/submission-and-replay.md#one-details-shape-for-every-exit-20-refusal).
The witness capsule therefore ties a proved node to all three generation-pinned
inputs that produced its ordinal. Prompt and skill revisions perform the same role for resolved
agent inputs when those revisions are known. `taskRef`, when present, is the
validated campaign-scoped human reference `<campaign>/<task-id>`; it remains
outside payload identity but is covered by the witness record hash as part of
the preserved capsule.

This guarantee belongs to the flow-runner path, not to a strong kernel schema for the opaque
capsule. The kernel requires a UUID `flowRunId`, validates a positive optional `maxNodes` and
the optional `taskRef` form, and
validates a non-negative optional `nodeOrdinal` and the optional prompt/skill revision shapes;
it otherwise preserves object members verbatim. A generic RPC client can submit a thinner
capsule, so consumers should not assume these runner identity hashes exist on non-flow records.

`query.proof` returns the selected canonical record verbatim as `witnessRecord`. No separate
“proof copy” drops the capsule; its hash, including the runner identity hashes, is the record
hash input.

## Hash construction

The genesis predecessor is:

```text
sha256:0000000000000000000000000000000000000000000000000000000000000000
```

For each parsed record:

1. Preserve the JSON object's member order.
2. Replace the value of `hash` with the empty string; do not remove the member.
3. Serialize the whole object as compact JSON with no trailing LF.
4. Compute SHA-256 over those UTF-8 bytes.
5. Encode the digest as lowercase `sha256:<64 hex digits>`.

The producer first normalizes the complete record through the same JSON-value parse used by the
verifier, then hashes and appends that same retained object. This slightly awkward round trip is
important: it prevents a duration-derived floating-point decimal from hashing one byte spelling
and being read back with another.

`prevHash` is itself inside the next record's hash input. Unknown additive fields are also inside
the input. Changing any covered byte therefore changes the record hash and breaks the successor
link unless the remainder of the chain is rewritten.

## Additive evolution

The forward rule is “skip if absent”:

- a new derivable or optional field is omitted on records that predate it;
- readers supply absence, not a rewritten historical value;
- unknown additive fields are tolerated and hash-covered;
- canonical records are never rewritten in place.

This is why optional known fields use omission and why a consumer must not require every field
that appears on the newest record. A change that alters existing canonical bytes or the meaning
of a required field is not additive. The current verifier accepts only `schemaVersion: 2`; any
future non-additive evolution must arrive with an explicit versioned migration rather than
silently normalizing old history.

## Offline verification

Verify the default canon and adjacent advisory chain:

```console
$ tally witness verify
verdict chain: ok (42 records, seq Some(1)..Some(42))
attestation chain: ok (7 records; unauthenticated-by-construction)
```

Select paths and obtain a machine-readable report:

```console
$ tally witness verify /srv/tally/witness.jsonl \
    --attestations /srv/tally/attestations.jsonl \
    --exec-attestations /srv/worker-a/exec-attestations.jsonl \
    --format json
```

The JSON result has `schemaVersion: 2`, `protocolVersion: 5`, a combined `ok`, the verdict and
adapter-attestation reports plus chain heads, and an `execAttestations` report for every supplied
host ledger. A missing selected ledger is treated as a valid empty chain with the genesis head.

The canonical verifier checks, in order:

1. LF termination, non-blank compact JSON, and canonical known-field order;
2. schema, record type, required fields, omission instead of top-level null, and field
   invariants;
3. each stored hash against the recomputed hash;
4. every `prevHash` against the predecessor, beginning at genesis;
5. strict, unique, gap-free sequences beginning at 1.

Problems are typed as:

```text
parse-error
invalid-record
schema-version-invalid
record-type-invalid
hash-mismatch
prev-hash-mismatch
seq-order
seq-gap
seq-duplicate
```

An invalid chain is still reported to stdout, but the command exits 1 and also prints
`tally: ledger verification failed` to stderr. For example, changing an artifact hash without
updating the stored record hash reports a `hash-mismatch` with the line, sequence, stored hash,
recomputed hash, and the phrase `line tampered`.

The verifier returns no decoded verdict records when the chain report is invalid. Daemon query,
await reconstruction, retry admission, and deduplication similarly fail closed when canonical
witness verification fails.

## Comparing execution hosts with canon

An execution attestation payload has schema 2 and contains:

```text
kind: "exec", executionId, taskUuid, attempt, leaseEpoch, hostId,
adapter?, executor?, argvHash, payloadHash?, briefHash?,
startedAt, finishedAt, exitCode, outputHash?, storePaths?
```

`executionId` is derived independently from task UUID, attempt, and lease epoch. Compare one or
more host chains with canon:

```console
$ tally witness compare \
    --canon /srv/tally/witness.jsonl \
    --attestations /srv/worker-a/exec-attestations.jsonl \
    --attestations /srv/worker-b/exec-attestations.jsonl \
    --format json --strict
```

The command first verifies every input chain. It then compares execution identity, exit code,
artifact/output hash, store paths, and payload identity. Each canonical execution is
`unanimous`, `diverged`, or `unattested`; attestations with no canonical execution are counted
as orphans. Divergence always makes the report fail. Without `--strict`, unattested executions
and orphans remain visible but do not fail it; strict mode fails on either.

`tally witness append` does **not** append a canonical verdict. It appends an arbitrary payload
to the advisory `attestations.jsonl` chain. Canonical verdicts are emitted only by daemon
terminal processing.

## Authorship verification

When a witness carries `resultRevision` and `authorship`, verify its Git AI note without a
daemon:

```console
$ tally witness verify-authorship \
    --ledger /srv/tally/witness.jsonl \
    --repository /work/repository \
    --task 018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321 \
    --attempt 1 \
    --format json
```

The verifier checks the canonical verdict chain first, then compares the witnessed
`refs/notes/ai` target and note-content hash with Git plumbing in the selected repository. Its
result status is one of `match`, `ledger-invalid`, `witness-not-found`, `not-bound`,
`revision-missing`, `missing-note`, `note-content-mismatch`, `notes-ref-target-mismatch`, or
`error`; anything other than `match` exits 1. The nested witnessed authorship observation keeps
its separate `bound`, `unavailable`, `missing-note`, `mismatch`, or `error` vocabulary.
