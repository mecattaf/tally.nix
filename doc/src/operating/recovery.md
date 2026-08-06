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
| A running local job | tally probes `tally-job-<uuid>.service`, or `tally-job-<campaign>-<task-id>-<uuid>.service` when the row carries `taskRef`. A matching attempt, lease epoch, and systemd invocation ID are adopted; the argv is not run twice. |
| A running remote job | The coordinator repeatedly sends the same bounded `Probe`/`Adopt` operation to the named SSH executor. Its logical leases remain held while transport state is unknown. |
| A terminal job | The canonical witness reconstructs `queue.await_job` and that job's deterministic `barrier:<task>:<attempt>` result without re-execution. |
| A runtime-exceeded job eligible for automatic bounded requeue | The first witness is retained, a durable retry advances the same task UUID, and existing stale-attempt waiters resolve to the new current attempt up to `maxAttempts`. |
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

## The startup budget, and where the time goes

Everything the daemon does before `READY=1` — the row migration, durable fact
collection, unit-fact collection, the gcroot and GitHub sweeps, recovery-plan
hydration, job installation — is charged to `TimeoutStartSec`. It is never
charged to `WatchdogSec`: the service watchdog is not armed until `READY=1`, so
a long startup cannot miss a watchdog deadline and cannot be diagnosed from one.

That budget grows with the estate, not with the workload of any one job. The
first measurement at estate scale, on a coordinator carrying roughly 7,000
acknowledged events, was **61 s** — against a 90 s budget the daemon had merely
inherited from the manager's `DefaultTimeoutStartSec`, having declared none of
its own. The trend mattered more than the number: the same estate had taken
25–41 s a few days earlier, and about 5–8 s per heavy-workload day was being
added. A daemon that crosses its start timeout does not report a slow startup;
it is killed and restarted, forever, and the operator sees a restart loop.

Two things changed as a result.

**The budget is per phase.** The daemon sends `EXTEND_TIMEOUT_USEC=` at every
startup phase boundary, which is the mechanism systemd provides for exactly
this case. Each notification restarts the start timeout from the moment it is
received, so the limit is no longer "how long may the whole of startup take"
but "how long may any one phase of it take". An estate that has simply grown
keeps starting. A daemon wedged inside one phase still dies, on the same 90 s
clock, and `systemctl status tally-daemon` names the phase it was in:

```console
$ systemctl --user status tally-daemon
   Active: activating (start) since ...
   Status: "starting: unit-facts"
```

That 90 s is now declared by the shipped units rather than inherited, and it
matches `daemon::startup::STARTUP_PHASE_BUDGET`.

**The phases are reported.** Immediately before `READY=1` the daemon writes one
line naming every phase and its wall-clock. Before this, the journal was
completely silent from `Starting` to the first late-startup warning, so a slow
start could be measured but not attributed. The line below is a real one, from
a daemon opened on an empty state directory — the shape is what matters, and
every phase is present even when it costs nothing:

```console
$ journalctl --user -u tally-daemon | grep 'startup complete'
tally: startup complete in 0.008s of a 90s per-phase budget prepare=0.000s
row-migration=0.003s durable-facts=0.000s gcroots=0.000s unit-facts=0.000s
recovery-plan=0.000s storage=0.001s lease-engine=0.000s failure-stderr=0.000s
gh-orphan-sweep=0.000s install-jobs=0.003s initial-recovery=0.000s
```

The 61 s estate measurement above predates this line and has no per-phase
attribution; the next start on an estate of that size is the first one that
will.

The phase names are stable and pinned by a test. Grep one of them across
restarts to see which part of startup is growing:

```console
$ journalctl --user -u tally-daemon --since -7d \
    | grep -o 'unit-facts=[0-9.]*s'
```

`unit-facts` probes every acknowledged row's local execution state — one probe
per row — so it is the phase whose cost tracks estate size most directly, and
the first place to look when total startup grows. Which phase actually
dominates on a given estate is what this line is for; do not assume it. A lane
that adds pre-`READY` work is expected to add a phase for it, so its cost is
attributable here rather than folded into a neighbour's.

## Startup refuses pre-label unit-exit records

Campaign task labels entered the execution unit name, so a row whose
orchestration carries a `taskRef` now owns
`tally-job-<campaign>-<task>-<uuid>.service` where it previously owned
`tally-job-<uuid>.service`. Records written before that change name a unit this
binary never derives, and recovery refuses them:

```text
tally: recovery error: executor fact collection failed: 23 acknowledged row(s)
have unusable local execution facts:
  row <uuid> on this host (expected unit "tally-job-<campaign>-<task>-<uuid>.service"):
  unit exit record is invalid: record unit "tally-job-<uuid>.service" does not
  match expected unit "tally-job-<campaign>-<task>-<uuid>.service"
  [pre-label unit-exit record]
```

Every unusable record is listed in one pass, so the population is known before
the first repair rather than discovered one restart at a time. Validation stays
strict: nothing accepts the old name. Run the one-shot forward migration the
error names, which reads the same durable rows recovery reads and derives the
new name from the same function:

```console
$ tally migrate unit-exit-labels --state-dir <STATE_DIR>
$ tally migrate unit-exit-labels --state-dir <STATE_DIR> --apply
```

The first form prints the plan as JSON and writes nothing; run it first and read
`rewritten`. The second rewrites the `unit` field and nothing else — the
`invocationId`, `attempt`, `leaseEpoch`, `serviceResult`, and exit metadata
round-trip untouched, and the witness ledger is neither read nor written.
Running it again is a no-op (`alreadyLabeled`). Only records whose recorded name
is exactly the pre-label name for their own row are touched; anything else is
listed under `skipped` with the reason, and stays for a human.

Copy `<STATE_DIR>` from the refusal, which prints the daemon's own absolute
path. Without `--state-dir` the CLI resolves `$XDG_STATE_HOME/tally`, which is
**not** the NixOS module's `/var/lib/tally/state`. Run the command as the user
that owns that directory — under the shipped systemd units that is the service
user, not root. Exit records are written mode 0600 and nothing repairs
ownership afterwards, so a record rewritten under `sudo` is one the daemon can
no longer read: you would trade a name mismatch for a permission failure. A
directory that is not a coordinator's state tree is refused rather than reported
clean, so a mistyped path cannot masquerade as "nothing to migrate".

The pre-label name is a pure function of the record's file name
(`unit-exit/<uuid>.json` → `tally-job-<uuid>.service`), so no backup copy is
written: it would carry nothing the surviving file does not.

### The same rows' captures are stranded too, and nothing says so

Campaign task labels entered the *capture* stem in the same edit that changed
the unit name. `tally migrate unit-exit-labels` repairs the exit records, and it
is enough to bring a wedged coordinator back up — but it does not touch the
captures, which is a separate and much quieter loss.

For a row whose orchestration carries a `taskRef`:

- captures written by the old binary are at `capture/<uuid>.out`,
  `capture/<uuid>.adapter.err`, `capture/<uuid>.err`, archived under
  `capture/archive/<uuid>/`
- the current binary derives `capture/<uuid>.<task>.out` and so on

`tally query run` attaches `capturePath` and `stderrTail` to a failure by
resolving those names, and it has no fallback to the bare-uuid form. The capture
*generation* marker is keyed on the bare uuid in both binaries, so it still
matches — which is exactly what makes this quiet. The lookup succeeds and
reports that the failure has no capture, rather than reporting that it could not
find one. Nothing in the daemon's log, no startup refusal, and no field in the
query output says the bytes are still on disk.

Run the sibling one-shot to move them:

```console
$ tally migrate capture-labels --state-dir <STATE_DIR>
$ tally migrate capture-labels --state-dir <STATE_DIR> --apply
```

The first form prints the plan as JSON and moves nothing; read `renamed` first.
The second renames each entry within its own directory. Nothing is rewritten:
contents, modes and mtimes are untouched, and `unit-exit/<uuid>.json` and
`unit-exit/<uuid>.capture.json` — which are keyed on the bare uuid under both
binaries — are deliberately left alone. Running it again is a no-op
(`alreadyLabeled`). Where both the old and the new name exist for the same
stream, the entry is listed under `skipped` and left for a human: the command
does not choose between two captures.

The same `--state-dir` and ownership rules as `unit-exit-labels` apply — copy
the absolute path from the module's `stateDir` and run as the user that owns it,
because captures are mode 0600 and nothing repairs ownership afterwards.

The affected population is bounded by the historical count of rows carrying a
`taskRef`, so this is residue rather than a growth surface: a row dispatched by
the current binary has never had a bare-uuid stem.

### Rows dispatched to a remote executor

**The migration cannot repair these, on either host.** The labeled name is
derived from the durable rows, and those exist only on the coordinator: a worker
runs no tally daemon and has no `events/`, so the same command run there reads
zero rows and rewrites nothing. Running it on a worker and reading its clean
report as success is the one wrong turn to avoid here.

What the coordinator can do is tell you exactly what to write. Each such row
appears under `skipped` with the facts the hand repair needs:

```console
$ tally migrate unit-exit-labels --state-dir <STATE_DIR> \
    | jq '.skipped[] | select(.executor) | {executor, recordPath, preLabelUnit, expectedUnit}'
{
  "executor": "worker",
  "recordPath": "/var/lib/tally-worker/state/unit-exit/<uuid>.json",
  "preLabelUnit": "tally-job-<uuid>.service",
  "expectedUnit": "tally-job-<campaign>-<task>-<uuid>.service"
}
```

`recordPath` is resolved from the coordinator's `executors.<name>.stateDir`; if
no configuration is in scope it is omitted and the `reason` says which key to
read it from. On the owning host, as the account that owns that `stateDir`,
rewrite the `unit` field of each named record and change nothing else:

```console
$ jq --arg unit "$EXPECTED" '.unit = $unit' <RECORD> > <RECORD>.new \
    && mv <RECORD>.new <RECORD>
```

Then restart the daemon as above.

## Recover a killed flow runner

Use the original flow-run ID and the same script generation:

```console
$ tally flow run /nix/store/…-review.js \
    --flow-run-id 4f8608e1-608f-4e04-bf47-0e49fd9801f1 \
    --args '{"repository":"mecattaf/tally.nix"}'
```

The runner's own JSONL stream identifies each node as `created`, `attached`,
`reused`, `substituted`, or `terminal`, in the `disposition` field of its
`node-submitted` and `node-terminal` events. `RunReport` itself carries no
dispositions; read them from the stream, or from the node results the script
observed.

A replayed prefix must not create duplicate rows:

```console
$ tally query jobs \
    --flow-run 4f8608e1-608f-4e04-bf47-0e49fd9801f1
```

The node count and the set of `dedupKey` values must be unchanged, and each row's
`disposition` must still read `created` from the original run — a replay that
re-executed would have written new rows.

Do not change the script, arguments, catalog, or node payload while retaining the
run ID. The dedicated `*-changed-mid-run` identity errors and
`replay-divergence` are deliberate stop conditions, not recovery modes. When the
original inputs are gone for good — the ordinary case once a declarative
activation has moved their store paths — retire the run with
`tally flow supersede` and start the recorded successor; that transition is
durable, idempotent, and leaves the old run untouched. See
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
