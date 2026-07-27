# Recovery and restarts

Restarting tally does not restart every job. The daemon rebuilds its view from
durable enqueue events and the witness ledger, bumps its lease epoch, probes the
execution units that could still be live, and adopts an exact surviving
invocation. It does not infer success from a missing unit and it does not launch
a replacement when the previous launch is ambiguous.

That distinction is the recovery contract:

- durable facts are reconstructed;
- live work is adopted only when its identity is exact; and
- in-memory requests are made again by the client.

## What survives

| State before the restart | What happens after the restart |
|---|---|
| A queued job | Its acknowledged enqueue event rebuilds the row and the lease request is presented again. |
| A running local job | tally probes `tally-job-<uuid>.service`. A matching attempt, lease epoch, and systemd invocation ID are adopted; the argv is not run twice. |
| A running remote job | The coordinator repeatedly sends the same bounded `Probe`/`Adopt` operation to the named SSH executor. Its logical leases remain held while transport state is unknown. |
| A terminal job | The canonical witness reconstructs `queue.await_job` and that job's deterministic `barrier:<task>:<attempt>` result without re-execution. |
| A job-originated parent | Active child rows rebuild the parent's depth and outstanding-child count. Terminal parents disappear when their last child becomes terminal. |
| A reuse acknowledged just before a crash | Startup reconciles the durable reuse event with its governing witness and appends only a missing, unambiguous legacy reuse witness. It refuses conflicting history. |
| A blocked socket call | The connection and its waiter disappear. The caller must reconnect and send the idempotent await again. The shipped flow client does this automatically. |
| A flow runner process | Its JavaScript heap is gone. Running the same flow run again evaluates from the top; completed nodes return their witnessed terminal result, live nodes attach, and only the frontier is created. |

Two boundaries matter:

1. A drain snapshot such as `barrier:drain:<epoch>:<sequence>` is an
   in-memory convenience. It is not reconstructed after a daemon restart.
   Re-run `tally queue drain` and wait on the new barrier. Job-specific barriers
   are different: their task and attempt can be checked against the witness
   ledger.
2. Recovery adopts execution, not the caller's open connection. A generic
   client must reconnect and re-arm `queue.await_job` or
   `queue.await_barrier`. The flow runner's live client retries socket closure,
   I/O failure, daemon timeout, and epoch change, then reissues the await.

The relevant implementation is in
[`recovery.rs`](https://github.com/mecattaf/tally.nix/blob/4c85563/crates/tally-core/src/recovery.rs),
the startup path in
[`daemon.rs`](https://github.com/mecattaf/tally.nix/blob/4c85563/crates/tally-core/src/daemon.rs),
and the reconnect loop in
[`flow_live.rs`](https://github.com/mecattaf/tally.nix/blob/4c85563/crates/tally/src/flow_live.rs).
The `flow-multi-host` VM check kills the coordinator while a worker unit is
running and verifies that the unit name, invocation ID, PID, attempt, and
execution count do not change.

## Restart the coordinator

First record the jobs and pools you expect to recover:

```console
$ tally query jobs --state running
$ tally query jobs --state queued
$ tally query pools
```

For a Home Manager deployment, restart and inspect the user service:

```console
$ systemctl --user restart tally-daemon.service
$ systemctl --user is-active tally-daemon.service
active
$ journalctl --user -u tally-daemon.service --since -5m
```

For the NixOS system module, omit `--user`:

```console
$ sudo systemctl restart tally-daemon.service
$ systemctl is-active tally-daemon.service
active
$ journalctl -u tally-daemon.service --since -5m
```

Then check the socket, recovered rows, and ledger:

```console
$ tally query jobs --state running
$ tally query jobs --state queued
$ tally query pools
$ tally witness verify
```

The default Home Manager ledger is
`$XDG_DATA_HOME/tally/witness.jsonl` (normally
`~/.local/share/tally/witness.jsonl`). The NixOS module uses
`/var/lib/tally/data/witness.jsonl`. Pass `--ledger` when inspecting a
non-default location.

Do not remove `daemon.lock`, enqueue events, unit-exit records, or the witness
ledger to make startup proceed. Those files are the evidence recovery uses to
decide whether replay is safe. An old-format events directory produces an
explicit archive-aside error; archive the named directory exactly as the error
instructs, rather than converting or deleting it.

## Recover a killed flow runner

Use the original flow-run ID and the same script generation:

```console
$ tally flow run /nix/store/…-review.js \
    --flow-run-id 4f8608e1-608f-4e04-bf47-0e49fd9801f1 \
    --args '{"repository":"mecattaf/tally.nix"}'
```

The report identifies each node as `created`, `attached`, `reused`,
`substituted`, or `terminal`. A replayed prefix must not create duplicate rows:

```console
$ tally query jobs \
    --flow-run 4f8608e1-608f-4e04-bf47-0e49fd9801f1
```

Do not change the script, arguments, or node payload while retaining the run
ID. `script-changed-mid-run` and `replay-divergence` are deliberate stop
conditions, not recovery modes. See
[Submission identity and replay](../flows/submission-and-replay.md) and
[Troubleshooting](troubleshooting.md).

## Remote uncertainty fails closed

The worker runs no tally daemon. The coordinator invokes only the fixed
`tally __remote-executor` helper over SSH, and the worker stores a launch marker,
captures, and an exit record under the configured worker `stateDir`.

When SSH becomes unavailable, the coordinator logs:

```text
tally: remote executor "worker" transport is unavailable; retaining leases and retrying: …
```

It keeps retrying the same operation. It does not release capacity, start a
replacement, or ask an operator to guess whether the command ran. After a
coordinator restart, an exact running invocation is re-adopted and an exact
durable exit is collected. Any of these conditions stop recovery instead:

- a durable launch marker exists but neither the unit nor its exit record does;
- the attempt or lease epoch differs;
- the systemd invocation ID differs;
- the remote helper returns the wrong protocol version or response shape.

This is intentionally less available than guessing. If the worker state is
indeterminate, preserve its `stateDir`, restore SSH and the user systemd
manager, and let the coordinator probe it. Do not manually free the pool or
delete the marker while the job could still be running.
