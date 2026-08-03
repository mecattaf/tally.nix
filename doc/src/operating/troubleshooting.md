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
| Flow configuration is rejected | code `windowed-consumption-excluded` | A pool in `meta.pools` uses windowed consumption, from which flows are excluded by design. |
| Child points at the wrong daemon | `unknown parent job ID` | An inherited `TALLY_JOB_ID` was sent to a daemon that does not own that parent. |
| A key names different work | `dedup-key-conflict for key "KEY"` | The same full-mode key was presented with a different payload, or legacy history contains multiple live owners. |
| Flow history no longer matches | code `replay-divergence`; `ordinal N re-derived payload NEW but the ledger recorded OLD` | Replay derived different work at an existing ordinal. |
| Flow script changes during one run | code `script-changed-mid-run`; `flow run ID is pinned to RECORDED, not CURRENT` | One flow run observed two script hashes. |
| Flow arguments change during one run | code `args-changed-mid-run`; `flow run ID is pinned to RECORDED, not CURRENT; ... Retire the run and start a successor: tally flow supersede ...` | One flow run observed two serialized argument hashes — including when only the binary moved. |
| Flow catalog changes during one run | code `catalog-changed-mid-run`; `flow run ID is pinned to RECORDED, not CURRENT` | One flow run observed different exact catalog bytes, or changed between a catalog and none. |
| A retired run ID is replayed | code `flow-run-superseded`; `flow run OLD was superseded by NEW ...` | `tally flow supersede` durably retired that run; start the successor. |
| CLI loses the socket on a large request or response | `wire frame exceeds N bytes` or `daemon closed the socket before replying` | One side exceeded its configured frame bound. |
| Declared culmination is absent | `cannot read gate manifest PATH: ...` in `completion.gates.manifestError` | The job did not write the declared file on its execution host. |
| Remote work appears hung | `tally: remote executor "NAME" transport is unavailable; retaining leases and retrying: ...` | SSH or the fixed worker helper is unavailable after dispatch. |
| The daemon crash-loops at startup after an upgrade | `recovery error: executor fact collection failed: N acknowledged row(s) have unusable local execution facts` with `[pre-label unit-exit record]` | `unit-exit/` records written before campaign task labels entered the unit name. Run the named migration. |

## A job failed

Start with the lifecycle projection; failed events carry the final bounded
2 KiB of captured stderr directly:

```console
$ tally query log --task <task-uuid> --event failed --json \
    | jq '.items[] | {exitCode, stderrTail, stderrTruncated}'
```

`stderrTruncated: true` means earlier bytes were omitted. If the tail is not
enough, inspect `<stateDir>/capture/<task-uuid>.err`, an atomic UTF-8 diagnostic
projection capped at 2 KiB and materialized only for a failed generation. The
byte-authoritative raw adapter stream is
`<task-uuid>.adapter.err`; it exists on successful jobs too and may contain
routine runtime chatter, so non-empty `.adapter.err` is not a failure signal.

`postEvidence` never publishes a failure. A campaign issue receives failure
metadata only with explicit `postFailureEvidence`; it receives a conservatively
redacted tail only when `postFailureStderr` is also enabled. Each failed retry
has its own durable completion ID and therefore its own comment. A flow
runner's local structured terminal error still embeds its failed child's tail.

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

Flow node submissions deliberately have no `consumptionEstimate` surface.
Checking a registered flow against its rendered configuration rejects a
windowed pool before activation:

```text
FlowPoolError [windowed-consumption-excluded]: flows are excluded from
windowed-consumption admission by design; use priorities to control contention
between workloads
```

Use node priorities for flow contention: low-priority wave work remains
eligible to complete, while a more important ask can intercede midway through
the run. The kernel-side mechanism above remains available to direct and
producer enqueues.

## `unknown parent job`

Every admitted local job receives three related values. `TALLY_JOB_ID` is its
identity, `TALLY_SOCKET` selects the daemon, and `TALLY_JOB_TOKEN` is the
capability the daemon minted for that job. The CLI forwards the first as
`callerJobId` and the third as `callerJobToken`.

**Identity comes from the token, not from `TALLY_JOB_ID`.** When a request
presents a token, the daemon resolves the caller from it and ignores
`callerJobId` except as a consistency check. A process launched as a tally job
can therefore accidentally route to the wrong coordinator:

```text
unknown parent job <ID>
```

Inspect the inherited routing before retrying:

```console
$ env | grep '^TALLY_'
```

Unsetting `TALLY_JOB_ID` alone no longer converts a child into a root
submission — the token still resolves the caller, and the guardrails
(`depthCap`, `fanoutCap`, `noEnqueue`, ancestry) still apply. Naming a
different job's ID while holding your own token is rejected:

```text
callerJobId is not accepted as authorization; identity derives from TALLY_JOB_TOKEN
```

For a genuinely independent root submission from inside a job, drop the
capability itself:

```console
$ env -u TALLY_JOB_TOKEN -u TALLY_JOB_ID tally enqueue --pool <pool> -- <program>
```

That is a supported operator action, not a bypass: under tally's tenancy model
this machine has one trusted Unix user, and a process running as that user is
trusted as an operator. The token exists so guardrails are real rather than
cooperative and so one job cannot claim another job's identity — not to contain
hostile same-user code. See [SECURITY.md](https://github.com/mecattaf/tally.nix/blob/main/SECURITY.md)
for the full boundary.

For a child intentionally driving a different daemon, remove all three
inherited values and name the target explicitly. The other coordinator never
minted your token, so carrying it there fails with
`callerJobToken is not a live job capability`:

```console
$ env -u TALLY_JOB_ID -u TALLY_JOB_TOKEN -u TALLY_SOCKET \
    tally --socket /run/other/tally.sock enqueue --pool <pool> -- <program>
```

Do not make an unknown UUID valid by editing durable rows. Fix the process
environment or the explicit socket.

## `callerJobToken is not a live job capability`

The daemon mints one token per job generation and revokes it when the job
reaches a terminal state. This error means the presented token was never minted
by this daemon or has already been revoked:

```text
callerJobToken is not a live job capability; it was never minted or has been revoked
```

Common causes, in order of likelihood: the value was inherited by a process
that outlived its job; `TALLY_SOCKET` points at a different coordinator than
the one that minted the token; or the job was retried, which starts a new
generation with a new token. A live job always sees its current token in its
own environment — re-read it rather than caching it across a retry.

Jobs are also denied the administrative and producer-internal method classes.
Presenting a token to `queue.pause`, `queue.resume`, `queue.cancel`,
`queue.retry`, `queue.drain`, or a `__producer.*` method fails with:

```text
method <NAME> is not available to a job capability
```

Run those as the operator, from a shell that does not carry `TALLY_JOB_TOKEN`.

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

If the old generation is no longer available — the usual case after an
activation — retire the run instead of retrying it; see
[`flow-run-superseded`](#flow-run-superseded).

## `args-changed-mid-run` and `catalog-changed-mid-run`

The same run also pins `orchestration.argsHash` and
`orchestration.catalogHash`. The arguments hash covers the compact serialization
of parsed JSON; the catalog hash covers exact file bytes and is `null` when no
catalog was supplied. A mismatch emits the corresponding code and exits 20
before another node is admitted.

Query the run as above and restore the original invocation. For arguments, use
values with the same serialized identity. For a catalog, use the exact original
bytes, including insignificant-looking whitespace; adding or removing the
catalog is also an identity change. Use a new flow run ID when either change is
intentional.

### After a tally upgrade, byte-identical arguments can still be refused

The pin is over the bytes the runner serialized, not over a canonical form of
the logical value, so an in-flight run recorded by an older tally can be refused
for arguments nobody edited. Moving the runner's arguments off argv and into the
brief file did exactly this: the same logical arguments reach the hash through a
different serialization, so the recorded hash and the current hash disagree even
though `jq -c` of the unchanged arguments file reproduces the *current* hash
exactly.

There is no migration for this and there deliberately is not one. Recomputing
the recorded hash from the current arguments is the same operation as dropping
the pin, and the pin exists precisely because only the operator can attest that
the arguments are unchanged. The refusal therefore names the remedy, and the
`remedy` detail carries the command verbatim:

```console
$ tally flow supersede \
    --flow-run-id <OLD> \
    --new-flow-run-id <FRESH-UUID> \
    --reason args-changed
```

Persist the successor UUID before calling: idempotency is keyed on the whole
triple, so a fresh UUID per attempt records a different rollover. Then start the
successor. A supervisor can act on this without reading prose —
`resolution: "supersede"` and `transient: false` are unchanged, and `remedy` is
the new string field carrying the command.

## `flow-run-superseded`

The run ID was durably retired by `tally flow supersede`. The runner reports
this before comparing any hash and exits 20:

```text
flow run <OLD> was superseded by <NEW> (<reason>) at <timestamp>; run the successor
```

Start `<NEW>`. The old run is intact and still queryable; it simply will not
advance again.

This is the machine-actionable end of the identity refusals above. A supervised
runner that keeps one `flowRunId` per work item and retries it across
deployments cannot recover from `script-changed-mid-run` or
`args-changed-mid-run` on its own — after an activation, the exact old script or
`args.tools` store path may not exist any more, so "restore the original inputs"
has no operator answer. Three such items adjacent in a worklist will trip a
supervisor's failure fuse on every pass and starve everything behind them.

Record the generation boundary instead:

```console
$ tally flow supersede \
    --flow-run-id <OLD> \
    --new-flow-run-id <NEW> \
    --reason generation-change
$ tally query lineage <OLD>
```

Repeating the identical supersede call is safe — it answers
`disposition: "reused"` — so an unattended supervisor may issue it, crash, and
issue it again. `tally query lineage` reports `currentFlowRunId`, which is the
run to start. `tally query run <OLD>` reads `superseded` and names the successor
above its task board.

A supersede that contradicts durable lineage — a different second successor, a
successor already claimed, a cycle, a predecessor with unfinished nodes, or a
successor that has already started — is refused with `flow-lineage-conflict` and
exits 1. Cancel a live predecessor first; pick a fresh UUID for a successor. On
that conflict, read `tally query lineage <OLD>` and adopt `supersededBy`: a
supervisor that crashed after calling and before persisting its successor finds
the answer already durable there.

A supersede naming a run with no durable node exits 4 (`not_found`). That run
never recorded a script hash, so it can never trip an identity pin and never
needs retiring — check the run ID for a typo. Renderings do not matter: upper
case, unhyphenated, and braced UUIDs are all canonicalized to the hyphenated
lowercase form on both write and read.

## `flow-lineage-unusable`

Every flow start reads `<dataDir>/flow-lineage.jsonl`, so a damaged record there
stops flow runs that have no rollover of their own:

```text
flow lineage ledger <PATH> line <N> is unusable: <reason>
```

The RPC code is `flow-lineage-unusable`, the CLI exits 1, and the error carries
`transient: false` with `resolution: "repair-lineage-ledger"` so automation
escalates instead of retrying it every pass.

An *interrupted* append — a crash, a power loss, or a short write under ENOSPC —
never causes this: an unterminated final line is skipped on read and truncated by
the next write. This message means a **complete** record cannot be decoded or
validated, which in practice means a hand edit or bit rot. Failing closed is
deliberate: skipping the line could resurrect a run an operator durably retired.

Repair it with the daemon stopped. The file is a plain JSONL index, not a hash
chain, so removing the offending line is sufficient and nothing downstream needs
re-verifying:

```console
$ systemctl --user stop tally
$ sed -n '<N>p' ~/.local/share/tally/flow-lineage.jsonl   # inspect it first
$ sed -i '<N>d' ~/.local/share/tally/flow-lineage.jsonl
$ systemctl --user start tally
```

Removing a line forgets that one rollover: the run it retired is no longer
refused on replay, and its successor is no longer reachable through
`tally query lineage`. Re-record it with `tally flow supersede` if it still
applies.

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
<stateDir>/capture/<unit-uuid>[.<task-id>].attempt-<N>.gates.json
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
