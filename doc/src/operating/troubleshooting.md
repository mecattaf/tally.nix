# Troubleshooting

Start with the stable task UUID and preserve the data and state directories.
Restarting tally is safe for ordinary recovery, but deleting rows, witnesses,
worker launch markers, or captures removes the facts needed to decide what
happened.

This table is an index. Text in the **code or message** column is emitted by
the shipped implementation; placeholders vary with the job.

| Symptom | Code or message | Usual cause |
|---|---|---|
| Job remains queued or paused | state `queued` or `paused`; pool signal `STOP` | Capacity is held, a consumption window is spent, or the pool is paused/unreachable. |
| Child is rejected before admission | `parent fanout would exceed fanoutCap N`, `enqueue depth D exceeds depthCap N`, or `flow run ID already has N nodes; maxNodes is M` | A configured guardrail, not pool contention. |
| Budget submission is rejected | `windowed-consumption pool "NAME" requires consumptionEstimate` | The request omitted the authoritative debit estimate. |
| Child points at the wrong daemon | `unknown parent job ID` | An inherited `TALLY_JOB_ID` was sent to a daemon that does not own that parent. |
| A key names different work | `dedup-key-conflict for key "KEY"` | The same full-mode key was presented with a different payload, or legacy history contains multiple live owners. |
| Flow history no longer matches | code `replay-divergence`; `ordinal N re-derived payload NEW but the ledger recorded OLD` | Replay derived different work at an existing ordinal. |
| Flow script changes during one run | code `script-changed-mid-run`; `flow run ID is pinned to RECORDED, not CURRENT` | One flow run observed two script hashes. |
| CLI loses the socket on a large request or response | `wire frame exceeds N bytes` or `daemon closed the socket before replying` | One side exceeded its configured frame bound. |
| Declared culmination is absent | `cannot read gate manifest PATH: ...` in `completion.gates.manifestError` | The job did not write the declared file on its execution host. |
| Remote work appears hung | `tally: remote executor "NAME" transport is unavailable; retaining leases and retrying: ...` | SSH or the fixed worker helper is unavailable after dispatch. |

## A job never admits

Inspect the job and every requested pool:

```console
$ tally query job <task-uuid>
$ tally query pools
$ tally query jobs --state queued --pool <pool>
```

For a slot pool, `remainingCapacity: 0` and `queued` greater than zero means
all logical slots are held. For a windowed-consumption pool, inspect
`remainingBudget`, `resetAt`, and `signal`. `STOP` is a read-only headroom
projection; it does not itself pause or cancel work.

Wait for a holder or consumption window to clear, reduce competing admission,
or change the declared capacity and redeploy after verifying the host can
actually sustain it. Pools are cooperatively enforced: increasing `capacity`
does not create kernel isolation. If the job is `paused`, inspect producer and
pool changes before using:

```console
$ tally queue resume <pool>
```

Resume only an administratively paused, reachable pool. A pool-reachability
producer will re-arm jobs when its configured hysteresis confirms return.

Guardrails look different because the child is never queued. The exact
rejections are:

```text
parent fanout would exceed fanoutCap <N>
enqueue depth <D> exceeds depthCap <N>
flow run <ID> already has <N> nodes; maxNodes is <M>
```

Reduce concurrent outstanding children, remove accidental recursive enqueue,
or make a deliberate configuration change to `fanoutCap`, `depthCap`, or the
flow's `maxNodes`. `maxNodes` counts created rows for the lifetime of one flow
run; waiting for earlier nodes to finish does not reduce it. A cancelled row is
projected as Deleted and releases its slot, so a replay can continue past a
cancelled frontier without raising the cap.

A manual request for a `windowed-consumption` pool must include the debit:

```console
$ tally enqueue --pool programmatic \
    --consumption-estimate 20 -- <program> <arg>
```

Without it the daemon emits:

```text
windowed-consumption pool "programmatic" requires consumptionEstimate
```

Flow node submissions have no `consumptionEstimate` surface in the shipped
dialect, so assigning such a pool to a flow node is not an operable workaround.

## `unknown parent job`

Every admitted child receives `TALLY_JOB_ID`; the CLI turns that value into
`callerJobId`. `TALLY_SOCKET` selects the daemon. A process launched as a tally
job can therefore accidentally present coordinator A's parent ID to
coordinator B:

```text
unknown parent job <ID>
```

Inspect the inherited routing before retrying:

```console
$ env | grep '^TALLY_'
```

For an independent root submission to the same daemon, remove only the parent
capability:

```console
$ env -u TALLY_JOB_ID tally enqueue --pool <pool> -- <program>
```

For a child intentionally driving a different daemon, remove both inherited
values and name the target explicitly:

```console
$ env -u TALLY_JOB_ID -u TALLY_SOCKET \
    tally --socket /run/other/tally.sock enqueue --pool <pool> -- <program>
```

Do not make an unknown UUID valid by editing durable rows. Fix the process
environment or the explicit socket.

## `dedup-key-conflict`

Full-mode dedup keys permanently identify canonical payload bytes. A live or
terminal owner with another hash produces:

```text
dedup-key-conflict for key "<KEY>"
```

The structured error carries the existing task UUID and payload hash. Query
that task, then compare the current argv, brief, adapter options, working
directory, workspace, evidence, and other payload-bearing fields:

```console
$ tally query job <existing-task-uuid>
$ tally query proof --task <existing-task-uuid>
```

Restore the payload that the key already names, or choose a deliberately new
key for genuinely new work. Do not delete the old witness or keep changing
fields until the error disappears. More than one live owner is possible only
as legacy residue; tally refuses to choose one.

## `replay-divergence`

A replayed flow re-derives each node and compares its payload hash with the
governing durable row:

```text
ordinal <N> re-derived payload <NEW> but the ledger recorded <OLD>
```

The flow error code is `replay-divergence` and `tally flow run` exits 20.
Inspect the ordinal in the flow-run projection and the governing proof:

```console
$ tally query jobs --flow-run <flow-run-uuid>
$ tally query proof --task <node-task-uuid>
```

Restore the exact prompt, argv, adapter configuration, workspace input, or
other payload source used by that run. Put revised work in a new flow run.
Changing history or retrying the same mismatch cannot make replay
deterministic.

## `script-changed-mid-run`

One `flowRunId` is pinned to the first script hash recorded for it. A concurrent
generation or a manual reuse of that ID with different JavaScript emits:

```text
flow run <ID> is pinned to <RECORDED>, not <CURRENT>
```

The code is `script-changed-mid-run` and the CLI exits 20. Query the run and
compare its `orchestration.scriptHash` with the exact file:

```console
$ tally query jobs --flow-run <flow-run-uuid>
$ sha256sum <flow-script>
```

Resume the old run with the exact recorded script bytes. Start changed code as
a new run. Declarative `services.tally.flows` reduces this risk because each
generation refers to an immutable Nix store path, but the identity is the
SHA-256 of the bytes, not a generation number.

## Oversized wire frame

Requests and responses are newline-framed JSON. Both directions default to
16 MiB and use `services.tally.transport.maxFrameBytes`; there is no protocol
negotiation. A client-side overflow says:

```text
wire frame exceeds <N> bytes
```

If the daemon rejects an oversized incoming frame before replying, the client
can instead see:

```text
daemon closed the socket before replying
```

Check that the client reads the same rendered configuration as the daemon.
For the NixOS service, for example:

```console
$ jq .maxFrameBytes /etc/tally/config.json
$ tally --config /etc/tally/config.json \
    --socket /run/tally/tally.sock query pools
```

Prefer moving a large prompt into the structured brief or a declared file
rather than argv. If the payload genuinely requires a higher bound, change
`transport.maxFrameBytes`, redeploy the daemon, and pass that rendered config
to every client.

## Gate manifest absent

When a gate manifest is declared, tally exports its execution-host path as
`TALLY_GATE_MANIFEST`. The `codex` and `claude-code` presets receive a default
path under:

```text
<stateDir>/capture/<unit-uuid>.attempt-<N>.gates.json
```

If the file is absent after execution, query shows:

```text
cannot read gate manifest <PATH>: No such file or directory (os error 2)
```

```console
$ tally query job <task-uuid> \
    | jq '.job.completion | {gates, acceptance}'
```

Absence is represented honestly as `gates.status: "not-run"`, with the error
in `manifestError`. It is not silently converted to a passing culmination.
Under manual acceptance the acceptance fact remains pending; under
`execution-and-gates`, a not-run gate is also pending.

Fix the job or adapter so it atomically writes valid schema-version-1 JSON to
the exported path on the host where it runs. Do not create an empty file after
the job has ended and call that evidence. If no culmination is intended, use
an adapter and submission with no manifest declaration; that is distinct from
declaring one and failing to produce it.

## Remote executor unreachable

The daemon logs the first transport loss:

```text
tally: remote executor "<NAME>" transport is unavailable; retaining leases and retrying: <DETAIL>
```

It deliberately retains the lease and does not fall back to local execution.
Inspect the coordinator and probe the same pinned transport:

```console
$ journalctl --user -u tally-daemon.service --since -30min
$ sudo -u tally ssh -T -F /dev/null \
    -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none \
    -o StrictHostKeyChecking=yes \
    -o UserKnownHostsFile=/etc/tally/worker-known-hosts \
    -i /run/credentials/tally-worker-key \
    tally-worker@worker.example.net true
```

Then verify on the worker:

```console
$ sudo -u tally-worker \
    XDG_RUNTIME_DIR=/run/user/$(id -u tally-worker) \
    systemctl --user is-active default.target
$ sudo -u tally-worker \
    XDG_RUNTIME_DIR=/run/user/$(id -u tally-worker) \
    systemctl --user list-units 'tally-job-*.service' --all
$ find /var/lib/tally-remote -maxdepth 2 -type f -print
```

Repair DNS/network reachability, the pinned host key, coordinator key
permissions, the worker user manager, the absolute `program`, or `stateDir`
ownership as indicated by the transport detail. Do not restart into local
execution or remove the worker state directory.

After transport returns, tally logs:

```text
tally: remote executor "<NAME>" is reachable again
```

If the worker has a durable launch marker but neither the exact unit nor an
exit record, the helper instead refuses ambiguity:

```text
execution unit <UNIT> has a durable launch marker for attempt=<N>, leaseEpoch=<E> but no unit or exit record; refusing ambiguous replay
```

Preserve the marker and captures, determine whether the external work could
have run, and treat the job as an incident. Replaying argv automatically could
duplicate side effects, which is why tally stops there.

The strings above are anchored in
[`daemon.rs`](https://github.com/mecattaf/tally.nix/blob/4c85563/crates/tally-core/src/daemon.rs),
[`wire.rs`](https://github.com/mecattaf/tally.nix/blob/4c85563/crates/tally-core/src/wire.rs),
[`completion.rs`](https://github.com/mecattaf/tally.nix/blob/4c85563/crates/tally-core/src/completion.rs),
and
[`executor.rs`](https://github.com/mecattaf/tally.nix/blob/4c85563/crates/tally-core/src/executor.rs).
