# Executors

An executor owns process launch and collection. It does not decide what work
exists or whether the job deserves a lease. The coordinator makes those
decisions first, then sends the exact execution request either to the local
user manager or to one named SSH target.

## Local systemd execution

The production daemon creates a deterministic transient unit named from the
task UUID (or internal job UUID when no task UUID exists). A row carrying
`taskRef = crm/t07` uses `tally-job-crm-t07-<uuid>.service`; a row without one
keeps `tally-job-<uuid>.service`. Its `systemd-run`
argv includes `--user`, `--wait`, `--collect`, literal environment and working
directory, resource/hardening properties, capture paths, credentials, and the
workload after `--`.

An `ExecStopPost` recorder writes the unit's invocation ID, attempt, lease
epoch, service result, and exit fields. Before launch, tally fsyncs a capture
generation marker. Together those facts let recovery distinguish “never
launched,” “still running,” “exited,” and “launch may have happened but cannot
be proven.” The last case fails closed instead of replaying argv.

The standalone executor library has a direct-process fallback used by tests.
Both the production daemon and the remote helper call `require_systemd`, so a
missing `systemd-run` is an error in deployed operation. Production will not
launch work it could not re-adopt or reclaim after a crash.

When `gitAi.enable = true`, the prepared request carries a closed, bounded
custom-attribute map for authorship correlation. Its only admitted keys are
the required `taskUuid`, `attempt`, `leaseEpoch`, and `adapter`, plus optional
`flowRunId`, `nodeOrdinal`, and `taskRef` (at most seven total). A campaign node
with a task reference therefore preserves that reference through git-ai note
matching. Any executor request validation rejection is terminal before launch
with code `executor-validation-failed`; it is witnessed and does not fabricate
a capture or wait for an adapter projection.

## Daemonless SSH targets

An SSH executor is a named configuration containing host, user, key,
known-hosts file, remote tally program, and worker state directory. The
coordinator launches OpenSSH with an empty environment and fixed noninteractive
options: explicit key and host verification, no agent, password, keyboard
interaction, forwarding, proxy command, or user SSH config.

The remote command is always:

```text
<configured-tally-program> __remote-executor
```

Workload argv never appears in that command. A bounded versioned JSON request
is written on stdin, and the helper serves exactly one `ensure`, `probe`,
`adopt`, or `reclaim` operation. The worker needs a persistent systemd user
manager and state directory, but no tally daemon.

The request can carry an exact brief document so the worker can materialize its
own content-addressed copy. Evidence and semantic gates that refer to
worker-local paths are evaluated there. The reply returns bounded stdout and
stderr captures plus the terminal evidence facts. tally does not copy the
workspace or declared artifact/store contents between hosts.

## Capture streams and failure diagnostics

Every generation writes stdout to `<uuid>.out` and the adapter's raw stderr to
`<uuid>.adapter.err`. The raw stream remains byte-authoritative for configured
stderr scrapes and provider traces; routine harness chatter can therefore be
retained without looking like a failure.

When the canonical terminal lifecycle event is `failed`, the coordinator reads
at most the final 2 KiB and atomically materializes that UTF-8 diagnostic as
`<uuid>.err`. It is not a duplicate raw stream. A per-identity capture lock
couples the generation check with materialization, and startup reconstructs a
missing current projection from a failed witness. The same tail is copied into
the failed lifecycle record and terminal result; earlier bytes are marked as
omitted. A successful generation has no `<uuid>.err`, so external monitors may
use that filename's presence as a failure-only signal. Older generations use
the same suffixes under `capture/archive/<uuid>/`; pre-split `.err` captures
remain readable for compatibility.

## Recovery keeps execution singular

Leases remain in the coordinator while a job runs remotely. If SSH disappears,
the same request is retried and the lease is retained; transport loss is not a
terminal result.

After a coordinator restart, recovery probes the deterministic unit identity.
A running unit is adopted only when invocation ID, attempt, and lease epoch all
match. Adoption waits for that exact durable exit and never issues a fresh
launch. Reclaim carries the same generation fence. Missing, mismatched, future,
or otherwise indeterminate facts fail closed.

This behavior is intentionally narrower than a general distributed execution
platform. There is one central admission and lease authority, daemonless
workers, no workspace staging service, and no worker-side scheduling queue.

Local and remote launch, the fixed SSH argv, protocol bounds, evidence return,
and adoption live in `crates/tally-core/src/executor.rs`.
`crates/tally-core/src/recovery.rs` plans restart actions, and the daemon keeps
leases attached while executing them. Tests
`ssh_transport_is_fixed_and_never_contains_workload_argv`,
`durable_daemon_policy_refuses_direct_fallback`,
`matching_durable_exit_is_adopted_without_reexecution`,
`restart_probe_and_adoption_survive_worker_loss`, and
`remote_transport_loss_retains_the_lease_until_authoritative_completion` cover
the critical paths. The `flow-multi-host` flake check exercises the rendered
coordinator/worker topology.

List jobs assigned to one execution target and retain each fact's authority:

```console
$ tally query jobs --executor worker | jq '.items[] | {taskUuid, executor, unit, liveState, terminalVerdict, termination}'
```
