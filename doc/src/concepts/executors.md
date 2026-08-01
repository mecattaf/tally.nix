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
