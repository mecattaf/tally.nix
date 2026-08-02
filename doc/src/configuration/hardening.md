# Hardening presets

Adapter hardening is an opt-in set of systemd properties for transient job
units. Omitting
[`hardening`](core-options.md#servicestallyadaptersnamehardening), or setting it
to `null`, intentionally selects the compatibility behavior: no hardening
preset is applied and evaluation emits no warning. `none` spells that behavior
explicitly. There is no scheduled default change.

The presets are defense in depth for trusted operator-selected programs. They
are not a hostile-code sandbox or an authorization boundary between jobs.

## Presets require a transient unit

Every property below is carried by the transient unit that `systemd-run`
creates for the job. The executor also has a direct-process backend used when
`systemd-run` is absent, and that path applies no preset, no `ReadWritePaths=`,
and none of the transient unit's resource limits.

The direct backend is off by default and must be requested explicitly with
`Executor::with_direct_fallback`. The packaged daemon and the remote-execution
helper both call `Executor::require_systemd`, so a NixOS deployment never
silently degrades to an unhardened launch: a missing `systemd-run` fails the
job with a spawn error instead of running it unconstrained. Library consumers
that opt into the direct backend give up hardening for every job that takes it,
whatever `hardening` the adapter declares.

## Property contract

The table is a checked fixture. A flake check extracts every systemd property
spelled in backticks and requires that exact string to occur in the executor
implementation.

{{#include hardening-properties.md.inc}}

`production` deliberately excludes three tempting controls:

- `MemoryDenyWriteExecute` breaks JavaScript JITs used by agent CLIs.
- `RestrictNamespaces` breaks the inner sandboxes used by Codex and Claude.
- `PrivateNetwork` and `IPAddressDeny` block the dynamic API egress those
  agents need.

Neither `strict` nor `production` restricts destinations or ports. Their
address-family allowlist permits IPv4, IPv6, and Unix sockets.

## Writable paths

`workspace` retains the compatibility-era write grant to the complete tally
state directory, plus the declared workspace. Use it only for trusted programs.

`strict` and `production` narrow writes to the paths needed by one execution:

- its declared workspace, plus the repository paths required when Git AI is
  enabled;
- the `unit-exit` directory used by the `ExecStopPost` recorder;
- only the current execution's stdout and raw adapter-stderr capture files;
- the exec-attestation ledger when that wrapper is enabled;
- the current GitHub context file, when present;
- the declared gate-manifest path, when present; and
- the adapter's
  [`extraWritablePaths`](core-options.md#servicestallyadaptersnameextrawritablepaths).

systemd opens `<uuid>.out` and `<uuid>.adapter.err`; after a failed terminal
verdict, the daemon atomically creates the failure-only, bounded `<uuid>.err`
diagnostic projection outside the job unit. The `ExecStopPost` recorder atomically replaces the exit record, the
exec-attestation wrapper appends the ledger, and the job may write its declared
gate manifest. A cooperative yield hook does not need a state-directory write
grant: it calls the daemon over `TALLY_SOCKET`.

The per-execution capture lock is deliberately absent from that list. It lives
in `capture-lock/<uuid>.capture.lock`, a sibling of `unit-exit` that neither
`strict` nor `production` grants and that the daemon creates 0600 for itself. It
used to sit in `unit-exit`, where a job under any preset could open and hold it —
and the daemon waits on it while materializing a failure excerpt. Locks left in
`unit-exit` by an older daemon are never taken again; `tally gc` drains them.

`workspace` and `none` are exceptions, because neither narrows anything here:
`workspace` grants the state directory whole and `none` emits no
`ReadWritePaths=` at all, so a job under either can still reach `capture-lock/`.
The relocation moves that surface off the narrowing presets; it does not remove
it from the two that were never narrowing. This is the same trusted-programs-only
caveat those presets already carry, and it is why the daemon additionally gives
up on a contended lock after a bounded wait instead of trusting the filesystem.

Every extra writable path must be absolute, contain no systemd `%` specifier,
already exist when the job starts, and be writable by the daemon user. Grant the
smallest directory or file that works. For example:

```nix
services.tally.adapters.codex = inputs.tally.lib.adapters.mkAdapter {
  argv = [ "codex" "exec" "--json" "--" ];
  hardening = "production";
  extraWritablePaths = [ "/var/lib/tally-agent/.codex" ];
};
```

`ProtectHome=read-only` otherwise makes home-directory state read-only. An
extra path reopens only the named path; it does not weaken the rest of the
filesystem policy.

`extraWritablePaths` only takes effect alongside a preset. With `hardening`
unset or set to `none` the unit receives no `ReadWritePaths=` property at all,
so the declaration is inert rather than restrictive — the job's filesystem
access is already unconstrained there.

## What strict does not do

- The network remains open. `AF_INET` and `AF_INET6` are allowed.
- The tally daemon socket remains reachable. `AF_UNIX` and `TALLY_SOCKET` are
  preserved so jobs can use cooperative yield and the job-originated RPCs.
- Pool enforcement can be cooperative. A preset does not turn a cooperative
  resource policy into a kernel-enforced quota.
- Every local job runs as the daemon's Unix user. Presets do not create a UID,
  credential, or trust boundary between jobs. In particular, the shared
  `unit-exit` directory and any shared extra writable path remain writable by
  other jobs using that same identity. That is why nothing the daemon blocks on
  lives there: the capture lock moved to `capture-lock/`, which `strict` and
  `production` do not grant. Under `workspace` or `none` a job can still reach
  it, so the guarantee that actually holds for every preset is the bounded wait —
  the daemon gives up on a contended lock rather than stalling, and a dispatch
  that loses the lock is recorded as preempted, never as a job failure.
- `LoadCredential=` keeps working independently of the preset. A credential
  delivered to a job is still available to that job.

Use a separate operating-system identity or a VM/container boundary when code
is not trusted as the daemon user. The preset names do not promise that level
of isolation.

## Choosing a preset

| Adapter shape | Recommended posture |
|---|---|
| A trusted command that requires normal host behavior | Leave `hardening` unset, or choose `none` explicitly. |
| A trusted compatibility tool that needs broad tally-state and workspace writes | Use `workspace`; do not describe it as isolation. |
| An API client that works with read-only system and home trees | Start with `strict`, then declare only necessary `extraWritablePaths`. |
| A production agent known to tolerate the syscall, device, and kernel controls | Use `production` with narrowly provisioned agent-state paths. |
| Hostile or mutually distrusting code | Do not rely on these presets; move the execution across a real OS/virtualization trust boundary. |

Test the selected adapter against the exact CLI version and workload before
deployment. Hardening failures surface as job failures; tally does not silently
relax a requested preset.
