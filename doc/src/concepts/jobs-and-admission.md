# Jobs and admission

A tally job is an immutable execution request plus a durable row. It is the
common unit produced by the CLI, a producer, or a flow node. Those entry paths
do not gain private execution machinery: they all narrow to the same admission,
lease, executor, evidence, and witness path.

The important parts of a job are:

- an argv array and, optionally, an absolute working directory;
- one or more named pools, acquired as one set;
- a priority and an adapter;
- optional executor, credentials, runtime bound, evidence, gate manifest,
  brief, and provenance;
- a stable task UUID, attempt, and lease epoch once admitted.

The canonical payload deliberately excludes bookkeeping such as `wait`. This
lets submission identity describe the work rather than how a caller chose to
observe it.

## Argv is data

The public CLI sends everything after `--` as an argv array. tally neither
inserts a shell nor joins the elements into a command string. Use an explicit
`sh -c` or `bash -c` element when shell syntax is genuinely part of the job.

The RPC retains an older `invocation` string field and parses it with a bounded,
quote-aware splitter, but it is still direct-exec syntax, not a shell language.
New callers should send `argv`.

An adapter adds its configured prefix and authorized launch options to the
workload argv. The resulting array remains an array through local
`systemd-run` and the SSH executor.

## Admission is a boundary, not a queue append

Admission rejects malformed evidence, unknown pools or adapters, unsafe paths
and credential names, conflicting pool sets, and invalid cross-references
before launch. Pool names are sorted and deduplicated, so `["gpu", "build"]`
and `["build", "gpu"]` cannot become different resource requests.

For job-originated work, the running parent is identified to the daemon. The
daemon requires a deduplication key, enforces the configured depth and fan-out
caps, refuses children from a parent carrying `noEnqueue`, and attaches the
parent relationship itself. A flow uses this same guarded path; it does not
write child rows directly.

The acknowledgement boundary is durable. The enqueue event is written and
acknowledged before the caller is told that a new job exists. A lease grant is
also fsynced before execution is launched, and terminal waiters are released
only after the witness record is fsynced. TaskChampion projection, journal
events, adapter scraping, and other derived observations happen outside those
acknowledgement barriers.

## The execution environment is constructed

tally supplies reserved `TALLY_*` variables from canonical job facts and
rejects an adapter environment that tries to set them. Optional variables are
explicitly removed when their fact is absent, rather than inherited from the
daemon's ambient environment. The same rule removes stale
`CREDENTIALS_DIRECTORY` when a job has no credentials.

Credentials are named references to absolute sources. The local executor
passes them through systemd's credential mechanism instead of placing secret
values in argv or the declarative JSON. Remote execution resolves the same
request on the worker.

`runtimeMaxSec`, when present, becomes a transient-unit runtime bound. Expiry
has the distinct canonical verdict `runtime-exceeded`; it is not disguised as a
normal nonzero exit.

## Exit is not proof

An exit code is only one possible evidence check. If a job declares an
artifact and exits zero without producing it, the outcome is
`clean-exit-no-artifact`, not `pass`. Conversely, the job from
[Getting started](../getting-started/first-job.md) passes because all three
declared checks agree.

The wire shape and job-origin guardrails live in
`crates/tally-core/src/wire.rs`. Durable admission and terminal barriers live
in `crates/tally-core/src/daemon.rs`; direct argv and environment construction
live in `crates/tally-core/src/executor.rs`. The integration test
`crates/tally/tests/cli_rpc.rs` proves the public CLI/RPC boundary, while the
`no_enqueue_depth_fanout_and_dedup_are_enforced` and
`canonical_payload_is_exact_ordered_and_excludes_admission_metadata` tests pin
the two admission rules described above.

Inspect the job from the walkthrough without consulting the journal:

```console
$ tally query job "$task" | jq '{taskUuid, argv, pool, priority, adapter, liveState, terminalVerdict}'
```
