# CLI reference

`tally` is both an operator command and the first client of the
[Unix-socket RPC contract](../reference/rpc-protocol.md). Most online commands print one compact
JSON value to stdout. Flow runs and watches are JSONL streams; selected witness commands and
`query render --format text` also have human-readable output.

On failure, the CLI normally prints `tally: ...` to stderr. Scripts should branch on the
[documented exit classes](../reference/errors.md), then parse the structured stdout or RPC
error rather than matching prose.

## Global selection

Global options may appear before or after a subcommand:

```text
--config PATH
--socket PATH
```

The default config is `$XDG_CONFIG_HOME/tally/config.json`, falling back to
`~/.config/tally/config.json`. Online clients use it to resolve the symmetric
`maxFrameBytes`; an explicitly selected file must exist and parse. The socket order is
`--socket`, `TALLY_SOCKET`, `$XDG_RUNTIME_DIR/tally/tally.sock`, then the temporary directory's
`tally/tally.sock`.

Check a rendered configuration without starting the daemon:

```console
$ tally --mode check-config --config /etc/tally/config.json
{"configuration":"valid","priorityRanks":{"interrupt":1000,"high":100,"medium":50,"low":10}}
```

The check validates the Rust runtime configuration, not the Nix module's cross-option
assertions.

## Command map

| Command | Purpose | Output on success |
|---|---|---|
| `enqueue` | Admit a job; alias of `queue enqueue`. | Admission JSON, or terminal JSON with `--wait`. |
| `gc` | Reconcile/prune witness GC roots and optionally collect the Nix store. | GC report JSON. |
| `queue` | Enqueue, continue, retry, cancel, pause/resume, drain, and await. | RPC result JSON. |
| `producer` | Preview and exercise configured GitHub producers. | Diagnostic/admission JSON. |
| `witness` | Verify/compare chains, verify authorship, or append an advisory observation. | Text or JSON. |
| `view` | Rebuild the derived TaskChampion view. | Rebuild report JSON. |
| `attest` | Run a child through the advisory execution-attestation wrapper. | Child output; child's exit. |
| `lease` | Acquire, release, or inspect an explicit reservation. | Lease JSON. |
| `daemon` | Run the daemon or drain event ingress. | Long-running process or drain JSON. |
| `query` | Jobs, proof, traces, producers, watch, status, and pool headroom. | JSON, JSONL, or text render. |
| `flow` | Check or execute a deterministic JavaScript flow. | Meta JSON or lifecycle JSONL. |

The installed `tallyd` symlink with no arguments is equivalent to `tally daemon run`.
`tally --mode daemon` with no subcommand is another compatibility spelling.

Several `__...` helper commands exist for systemd units and producers. They are hidden,
implementation-private, and not part of this CLI contract.

## Enqueue

Supply one or more pools and exactly one invocation form:

```console
$ tally enqueue \
    --pool local-ai \
    --priority high \
    --dedup-key review:42 \
    --evidence exit:0 \
    --wait \
    -- codex exec "review issue 42"
```

```console
$ tally queue enqueue \
    --pool build \
    --invocation "nix build '.#checks.x86_64-linux.doc'"
```

`-- <ARGV>...` preserves arguments directly. `--invocation STRING` uses tally's small
shell-like tokenizer; it handles whitespace, single/double quotes, and backslash escaping but
does not invoke a shell.

### Admission flags

| Flag | Contract |
|---|---|
| `--pool NAME` | Required and repeatable; all named pools are co-leased atomically. |
| `--executor NAME` | Optional configured execution target. |
| `--priority CLASS` | `interrupt`, `high`, `medium`, or `low`; default `medium`. |
| `--adapter NAME` | Configured adapter; default `shell`. |
| `--cwd PATH` | Working directory. Defaults to the supplied workspace worktree. |
| `--env NAME=VALUE` | Repeatable adapter environment. `TALLY_*` and `CREDENTIALS_DIRECTORY` are reserved. |
| `--pre-prompt-arg VALUE` | Repeatable adapter pre-prompt argument. |
| `--approval-policy`, `--sandbox-policy`, `--model`, `--effort` | Per-job adapter overrides validated by the adapter. |
| `--workspace-repo`, `--workspace-base-rev`, `--workspace-branch`, `--workspace-worktree` | All four are required together. |
| `--brief JSON` / `--brief-path PATH` | Mutually exclusive structured brief sources. |
| `--source SOURCE` | `manual` by default; also `orchestrator`, `calendar`, `events-dir`, `gh`, `build-effect`, `pool-reachability`. |
| `--dedup-key KEY` | Deduplication key. |
| `--orchestration JSON` | Opaque flow provenance capsule. |
| `--parent UUID` | Durable parent task identity. |
| `--evidence SPEC` | Repeatable canonical evidence specification. |
| `--evidence-class JSON`, `--manifest-hash VALUE` | Witnessed evidence/manifest metadata. |
| `--consumption-estimate N` | Direct admission estimate required by a windowed-consumption pool. |
| `--runtime-max-sec N` | Positive execution watchdog. |
| `--no-enqueue` | Forbid child enqueue from this job. |
| `--related-trigger JSON` | Fallback producer-trigger provenance. |
| `--wait` | Await the admitted/reused job and map its terminal verdict to the CLI exit. |

Gate-manifest flags are deliberately all-or-nothing at the CLI:

```text
--gate-manifest PATH
--required-gate ID       (at least one, repeatable)
--acceptance-policy manual|execution-and-gates
```

The CLI is narrower than the RPC here: it will not send a manifest spec with an empty required
gate list.

The exact RPC request fields, canonical payload hash, and result shapes are in
[the enqueue protocol](../reference/rpc-protocol.md#enqueueparams).

### Legacy submission warning

The public enqueue command has no submission-mode flag. It always omits the RPC `submission`
object and therefore selects **legacy mode**, even when `--dedup-key` is present. Legacy mode
does pass-witness reuse only; it does not attach identical live submissions or reject a live
same-key/different-payload collision.

The flow runner uses full mode. A foreign client that needs the full disposition table sends
`"submission":{"mode":"full"}` directly over RPC. The CLI's absence of that switch is a real,
current limitation, not an implied default.

`--wait` does not make enqueue one blocking RPC. The CLI first admits, then calls
`queue.await_job` on the returned identity. This makes reconnect/re-arm behavior explicit.

## Queue control

### Continue and retry

Continue a terminal adapter session with new arguments:

```console
$ tally queue continue TASK-UUID --wait -- "address the review"
```

The old job must be terminal and have a scraped session reference. Pools, executor, adapter,
priority, source, workspace, and adapter options are inherited. The new admission has a new task
identity and points back through `resumeFrom`.

Retry a terminal failed task in place:

```console
$ tally queue retry TASK-UUID
```

Retry keeps the UUID, increments `attempt`, and requires a governing terminal non-pass witness.
A pass cannot be retried.

### Cancel, pause, and resume

```console
$ tally queue cancel TASK-UUID
$ tally queue cancel TASK-UUID --force
$ tally queue pause local-ai
$ tally queue pause --all
$ tally queue resume local-ai
$ tally queue resume --all
```

Without `--force`, cancel affects paused/queued work but leaves running work untouched. Forced
cancel reclaims running execution and emits a cancelled witness. Pausing a pool withdraws its
queued lease requests and marks those jobs paused; it does not preempt running holders. Resume
re-admits jobs only when none of their required pools remains paused or unreachable.

### Drain and await

```console
$ tally queue drain
$ tally daemon drain
$ tally queue await-job TASK-UUID
$ tally queue await-barrier 'barrier:TASK-UUID:1'
```

The two drain spellings call the same `queue.drain` method. They claim pending event-directory
files and return a snapshot barrier for all active jobs. The CLI does not expose the RPC
`producer` filter.

`await-job` and `await-barrier` print raw terminal JSON. Unlike `enqueue --wait`, they do not map
a failed returned verdict to a non-zero process exit; inspect the JSON in scripts. Job awaits
can be reissued after daemon restart. Drain barriers cannot.

## Query

### Jobs and flow grouping

```console
$ tally query jobs --state running --pool local-ai --limit 100
$ tally query jobs --flow-run RUN-UUID
$ tally query jobs --cursor 'OPAQUE-CURSOR'
$ tally query job TASK-UUID
```

`query jobs` filters:

```text
--state / --live-state
--verdict / --terminal-verdict
--pool
--executor
--adapter
--source
--origin
--parent
--flow-run
--session
--since
--until
--limit
--cursor
```

`--flow-run` matches the witnessed/durable `orchestration.flowRunId` and is the supported way to
group a runner's nodes. Pagination cursors are daemon-memory snapshots: repeat the same filters,
and restart from the beginning if the daemon or cursor snapshot is gone.

### Status, proof, log, and trace

```console
$ tally query status
$ tally query status --pool build
$ tally query proof --task TASK-UUID --attempt 2
$ tally query log --task TASK-UUID --attempt 2 --event completed
$ tally query trace --task TASK-UUID --attempt 2 --limit 100
```

`query log` additionally filters by `--session`, `--source`, `--since`, and `--until`, and
supports `--cursor`. `query trace` also supports page cursors. Proof is not just a witness
lookup: it reports whether a witness is expected, returns the canonical record when present,
separates advisory attestations, and includes ledger verification state.

### Producers, pools, render, and stand-up

```console
$ tally query producers
$ tally query producers --name flow-monthly --kind calendar
$ tally query pools
$ tally query render --format json
$ tally query render --format text
$ tally query standup --since 2026-07-27T00:00:00Z
```

The RPC `query.render` supports an additional `scope` field, but the current CLI does not expose
it and always requests the default `all` scope. Likewise, the RPC stand-up method supports a
`source` filter that the CLI does not expose.

### Watch

```console
$ tally query watch
$ tally query watch --after 'change:00000000000000000123'
```

Watch prints each change record as its own JSON line. With no cursor, the first poll seeds at the
current head and prints no historical items; the command then polls every 500 ms. The durable
change log retains 4,096 entries. A gap prints the `cursor-expired` envelope and exits 2; decide
whether the missing interval is acceptable before resuming from `resumeAfterCursor`.

## Flow

Check syntax, metadata, deterministic-global restrictions, arguments, catalog requirements, and
configured pool closure without contacting the daemon:

```console
$ tally flow check ./review.js \
    --args '{"subject":"https://example.invalid/change/42"}' \
    --catalog /nix/store/...-tally-catalog.json
```

On success, `flow check` prints the script's normalized meta object as compact JSON.

Run a flow:

```console
$ tally flow run ./review.js \
    --flow-run-id 018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321 \
    --args '{"subject":"change-42"}' \
    --max-nodes 1000 \
    --catalog /nix/store/...-tally-catalog.json
```

`--args` defaults to `{}`. `--max-nodes` defaults to 1,000 and is intersected with a smaller
literal `meta.maxNodes`. A run ID is required from `--flow-run-id` or inherited
`TALLY_TASK_UUID`; it must be a UUID. When the runner itself is a tally job,
`TALLY_TASK_UUID` and `TALLY_JOB_ID` must be present together and valid.

The runner emits lifecycle objects as JSONL and ends with either `flow-report` or
`flow-failed`. Script/determinism errors exit 10; replay divergence or a changed script in the
same run exits 20. See the [full mapping](../reference/errors.md#structured-flow-errors).

### Catalog is flag-only

The selector catalog travels exclusively through `--catalog PATH`. There is no
`TALLY_FLOW_CATALOG` channel, and one is not reserved for future use. A Nix-rendered producer
adds the flag only when `services.tally.flows.<name>.catalog` is non-null. The option itself is
optional; a script whose literal meta declares selector use fails checking unless a catalog is
provided.

### Declarative runner pool and `budgetPool`

The generated calendar producer renders the runner with:

```json
{"pool":["flow"]}
```

It does this unconditionally. `services.tally.flows.<name>.budgetPool` is validated only to be
null or the name of an existing configured pool. Its value is not added to the runner pool,
passed as an argument, exported in the environment, or used as a child-admission estimate.

The live option description at `4c85563` calls `budgetPool` a pool “leased with the flow
runner.” That sentence is stale and contradicts both the renderer and the final ruling. The
existence-only behavior above is the contract. As shipped, the CLI has no flag that adds another
run-lifetime pool to a declaratively rendered flow runner.

## Witness

Verify the canonical and adjacent advisory ledgers:

```console
$ tally witness verify
$ tally witness verify /srv/tally/witness.jsonl --format json
$ tally witness verify \
    --ledger /srv/tally/witness.jsonl \
    --attestations /srv/tally/attestations.jsonl \
    --exec-attestations /srv/worker/exec-attestations.jsonl
```

Compare per-host execution observations with coordinator canon:

```console
$ tally witness compare \
    --canon /srv/tally/witness.jsonl \
    --attestations /srv/worker-a/exec-attestations.jsonl \
    --format json --strict
```

`--data-dir DIR` selects `DIR/witness.jsonl` instead of `--canon`. `--attestations` is required
and repeatable.

Verify a Git AI authorship binding:

```console
$ tally witness verify-authorship \
    --ledger /srv/tally/witness.jsonl \
    --repository /work/repo \
    --task TASK-UUID \
    --attempt 1 \
    --lease-epoch 7 \
    --format json
```

Append an **advisory** observation:

```console
$ tally witness append --payload '{"kind":"operator-note","value":"checked"}'
```

Despite its name, `witness append` defaults to `attestations.jsonl`; it cannot append canonical
verdict records. Record bytes, verification reports, tamper diagnostics, and comparison
semantics are in [Witness format and verification](../reference/witness-format.md).

## Explicit leases

```console
$ tally lease acquire gpu mutex
$ tally lease status LEASE-ID
$ TALLY_JOB_ID=TASK-UUID tally lease status
$ tally lease release LEASE-ID
```

`acquire` accepts one or more pools and returns a grant or queued ticket. With no lease argument,
`status` resolves the current job's lease through `TALLY_JOB_ID`. These are explicit reservation
tokens; they do not turn a flow runner into a full-run multi-pool lease.

## Producer diagnostics

The current public producer commands exercise configured GitHub producers:

```console
$ tally producer preview github-worklist
$ tally producer poll github-worklist --once --no-enqueue
$ tally producer explain github-worklist --item 'https://github.com/owner/repo/issues/42'
$ tally producer test github-worklist \
    --item 'https://github.com/owner/repo/issues/42' \
    --event label \
    --actor operator
```

All accept `--state-dir PATH`. `poll` currently requires `--once`; omitting it is exit 2.
`preview`, `poll --no-enqueue`, and `test` without `--promote` are non-admitting. `test` events
are `command-comment`, `mention`, `assignment`, or `label`. `--promote` performs the real
admission path and conflicts with `--no-enqueue`.

These commands use the local config and producer state directly, not the public RPC method
table. `query producers` is the read-only daemon inventory.

## Daemon

Run in the foreground:

```console
$ tally daemon run \
    --config /etc/tally/config.json \
    --socket /run/user/1000/tally/tally.sock \
    --state-dir /var/lib/tally/state \
    --data-dir /var/lib/tally/data \
    --cpu-weight 100 \
    --memory-max-bytes 8589934592 \
    --yield-grace-sec 20
```

`--cpu-weight` and `--memory-max-bytes` are required either as flags or through
`TALLY_CPU_WEIGHT` and `TALLY_MEMORY_MAX_BYTES`. The systemd module supplies them. `--mock`
prints `tally mock daemon ready` and does not open a real daemon.

`tally daemon drain` is an online client command, not a daemon lifecycle action.

## Retention, derived view, and execution wrapper

Inspect and then apply retention:

```console
$ tally gc --horizon 30d --dry-run
$ tally gc --horizon '30d' --collect
```

The horizon accepts systemd-like components such as `30d`, `1h 30min`, or `1.5h`.
`--dry-run` reports without changing roots. Without `--collect`, an applying run reconciles and
prunes tally's witness GC roots but does not invoke Nix store collection. `--data-dir` must be
absolute.

Rebuild the disposable TaskChampion projection from durable facts:

```console
$ tally view rebuild --data-dir /var/lib/tally/data --yes
```

Without `--yes`, an existing view prompts on stderr and reads confirmation from stdin. The
canonical witness and durable ingress facts are not rewritten.

Run a command through the advisory execution wrapper:

```console
$ tally attest exec \
    --task-uuid TASK-UUID \
    --attempt 1 \
    --lease-epoch 7 \
    --payload-hash sha256:... \
    --adapter codex \
    --evidence exit:0 \
    --ledger /var/lib/tally/exec-attestations.jsonl \
    -- command arg
```

`--task-uuid`, `--attempt`, `--lease-epoch`, and a non-empty trailing argv are required.
`--brief-hash`, `--executor`, and repeatable `--evidence` are optional. The wrapper inherits
stdio, environment, and cwd, appends one advisory execution observation when it can, and
propagates the child's status.
