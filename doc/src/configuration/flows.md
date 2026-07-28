# `services.tally.flows`

`services.tally.flows` registers store-pinned JavaScript flows in the Nix
configuration. Each entry is checked while the system or Home Manager
generation is built. Home Manager can additionally turn an entry into a
scheduled user service; NixOS cannot.

This chapter explains how the twelve leaf options compose. The generated
[shared-core flow options](core-options.md#servicestallyflows) remain the
authority for types, defaults, examples, and declaration locations.

## The twelve options

| Option | Contract |
|---|---|
| [`script`](core-options.md#servicestallyflowsnamescript) | Required Nix path to the JavaScript source. Nix places it in the store, and its content hash participates in the runtime `scriptHash` identity. |
| [`onCalendar`](core-options.md#servicestallyflowsnameoncalendar) | Optional systemd calendar expression. Its default is `null`, which keeps the flow registered and checked without creating a scheduled producer. |
| [`args`](core-options.md#servicestallyflowsnameargs) | JSON-serializable attributes exposed to the script as `args`, validated against `meta.argsSchema`, and pinned per run by `argsHash`. The default is `{}`. |
| [`priority`](core-options.md#servicestallyflowsnamepriority) | Priority of the runner job. The default is `low`; child nodes carry the priorities declared in the script instead. |
| [`dedupKey`](core-options.md#servicestallyflowsnamededupkey) | Scheduled-run existence key with bounded `strftime` expansion. The default template is `flow-<name>-%Y-%m-%d`, where `<name>` is the registry key. |
| [`runtimeMaxSec`](core-options.md#servicestallyflowsnameruntimemaxsec) | Optional `RuntimeMaxSec` watchdog for the runner. Its configured default is 43,200 seconds; `null` explicitly removes the watchdog. |
| [`evidence`](core-options.md#servicestallyflowsnameevidence) | Canonical evidence required of the runner job. It defaults to `[ "exit:0" ]`; node evidence remains part of each node specification. |
| [`maxNodes`](core-options.md#servicestallyflowsnamemaxnodes) | Per-run admission backstop, default 1,000. It must be at least a literal `meta.maxNodes`; at runtime the smaller applicable bound governs. |
| [`catalog`](core-options.md#servicestallyflowsnamecatalog) | Optional Nix path to a selector catalog. It is passed through `--catalog`, pinned per run by the exact-byte `catalogHash`, and required when `meta.selectors` is non-empty. |
| [`workloadMutex`](core-options.md#servicestallyflowsnameworkloadmutex) | Optional capacity-1 mutex pool co-leased with `flow` for the lifetime of the runner process. |
| [`extraEnv`](core-options.md#servicestallyflowsnameextraenv) | String environment added to the runner invocation. It defaults to `{}` and may not use `TALLY_*` or `CREDENTIALS_DIRECTORY` names. |
| [`credentials`](core-options.md#servicestallyflowsnamecredentials) | Credential name-to-source-path map passed to the runner through systemd `LoadCredential`. It defaults to `{}`; the JSON never contains secret contents. |

There are exactly twelve leaves in the generated reference. The documentation
flake check counts them, so adding or removing a flow option requires an
intentional contract update rather than silently drifting this table.

### Script and arguments

`script` is a Nix path, not a mutable pathname looked up when the timer fires.
The scheduled producer receives the resolved store path. The flow runtime
hashes the content and refuses to let one flow-run identity observe two script
generations.

`args` is serialized once into the generated producer's direct argv. Put values
known at generation time there: executable paths, repositories, bounded input
lists, and fixed policy. The checker evaluates those exact JSON bytes against
the literal `meta.argsSchema`. Flow code should derive work from `args`,
literals, metadata, and witnessed prior results rather than ambient state.

`extraEnv` is for non-reserved process settings, not another argument or secret
channel. `credentials` supplies opaque files through the systemd credential
directory. Flow scripts have no general environment, filesystem, or process
API; only the runner and the jobs it submits cross those boundaries.

### Scheduling identity

When `onCalendar` is non-null, Home Manager generates a calendar producer named
`flow-<name>`. Its enqueue launches the configured tally package with the
equivalent of:

```text
tally flow run <script> --args <json> --max-nodes <maxNodes> [--catalog <path>]
```

The producer uses the `shell` adapter and always requests the reserved `flow`
pool. When `workloadMutex` is non-null, it requests that one pool as well. It
copies `priority`, `dedupKey`, `runtimeMaxSec`, `evidence`, `extraEnv`, and
`credentials` from the flow entry. There is no extra catalog environment
variable: a non-null `catalog` becomes the explicit `--catalog` flag.

`dedupKey` is expanded for a scheduled firing using the supported `strftime`
grammar. The default gives one existence key per flow name and calendar day.
Choose a different bounded template when the intended cadence or idempotence
window is different. An empty key or an unsupported conversion fails the
checked configuration instead of waiting for the timer.

An `onCalendar = null` entry still goes through every build-time check, but no
`flow-<name>` producer unit is created for it. A flow without `workloadMutex`
may be run directly with `tally flow run`; that manual path holds no runner
lease and bypasses the normal depth and fanout parent caps. A flow with
`workloadMutex` must use the admitted-parent invocation described below.

### Runner evidence and bounds

`runtimeMaxSec` bounds the generated runner process. It does not replace the
daemon's per-node runtime controls. Likewise, `evidence` describes the runner's
canonical completion. A runner with the default evidence must exit zero; every
child has the evidence declared by its own node specification.

`maxNodes` bounds non-deleted rows for one flow-run identity. Rows that finish
normally do not free space under this cap; cancelled rows are projected as
Deleted and do. A literal `meta.maxNodes` may make the script stricter, but the
Nix value may not be smaller than that declaration:

```text
tally flow <name> maxNodes <configured> is less than script meta.maxNodes <declared>
```

The generated runner passes the Nix value as `--max-nodes`. Runtime admission
also stamps the bound into each submission, so replay cannot evade it by
waiting for earlier nodes or restarting the runner.

## Automatically declared pools

Any non-empty Home Manager `services.tally.flows` registry weakly declares two
reserved pools:

| Pool | Weak defaults | Owner |
|---|---|---|
| `flow` | `resource = "cpu-slot"`, capacity 8, cooperative enforcement, no hard preemption | Every generated runner requests this pool for its lifetime. |
| `build` | `resource = "build-slot"`, capacity 2, cooperative enforcement, no hard preemption | The flow host's `drv()` helper adds this pool to derivation nodes. |

They are weak defaults, so an operator may deliberately override their pool
settings. Their names remain reserved: a script must not list `flow` or `build`
in `meta.pools`. The checker rejects either spelling because the runner and
`drv()` helper own those implementation leases.

Child nodes otherwise request the pools in their node specifications. The flow
runner does not acquire all child pools up front and does not retain a child's
lease across an `await`.

## `workloadMutex`

`workloadMutex` is one typed pool name, not an arbitrary list of extra runner
pools. When non-null, the named pool must exist, must not be `flow` or `build`,
and must use `resource = "mutex"`, `capacity = 1`, and `co-residency`. A
windowed-consumption pool is rejected. The generated runner is admitted with:

```json
{"pool":["flow","<workloadMutex>"]}
```

The workload mutex is held for the lifetime of the runner *process*, not the flow *run*. If the runner is killed, preempted, or exceeds runtimeMaxSec, the mutex is released and another run may take it. A replay of the interrupted run must re-acquire the mutex and will block until it is free, while its already-created children remain durable and may complete in the meantime. This is weaker than the exclusion a single long-lived bash parent provides; the difference is inherent to the replay model.

That weaker guarantee is intentional. For example, if run A's process dies,
run B may acquire the mutex before run A replays. Run A's replay then queues
behind B; once B releases, the same durable run resumes and can reuse or attach
to its existing children.

Direct `tally flow run` has no parent lease, so it is not a sanctioned
invocation for a flow that declares `workloadMutex`. Enqueue the runner as a
job holding both pools instead; the child-capable runner must not use
`--no-enqueue`:

```console
$ tally enqueue \
    --pool flow \
    --pool monthly-review \
    --dedup-key manual-monthly-review-2026-07 \
    -- tally flow run /nix/store/...-monthly-review.js \
      --args '{"period":"2026-07"}' --max-nodes 200
```

The admitted job supplies `TALLY_TASK_UUID` and `TALLY_JOB_ID`, so
`tally flow run` derives the durable flow-run identity and applies ordinary
parent depth and fanout accounting. With a configured registry, the CLI matches
the invoked script path to `services.tally.flows`, canonicalizing existing
paths. A matching `workloadMutex` registration without that inherited parent
identity fails before daemon connection with
`workload-mutex-parent-required`. A matching registration without a mutex is
allowed to run directly and keeps the documented depth/fanout bypass.

Tally runs on one machine for one trusted user, using local AI plus
authenticated Claude Code and Codex subscriptions.

## `budgetPool` has been removed

`services.tally.flows.<name>.budgetPool` never created a lease or render
channel, so retaining it would advertise behavior tally did not honor. A
declaration now fails checked configuration with guidance to remove it. Use
node priorities for contention between flow workloads, or `workloadMutex` for
one process-scoped capacity-1 runner mutex. Neither mechanism creates a
run-wide consumption budget.

## Generation-time validation

Both wrappers make the rendered configuration depend on a checked derivation.
Consequently a bad declaration stops `home-manager switch` or a NixOS
generation build before activation. The configured tally package, rather than
a second Nix reimplementation of the flow language, performs the checks.

The pipeline is:

1. Render the complete runtime JSON and run
   `tally --mode check-config --config <rendered-json>`.
2. For each entry, run
   `tally --config <rendered-json> flow check <script>` and capture its
   normalized metadata.
3. Reject any `meta.pools` name absent from `services.tally.pools`.
4. Reject any `meta.pools` entry configured with a
   `windowed-consumption` predicate.
5. Reject reserved `flow` or `build` in `meta.pools`.
6. Compare a literal `meta.maxNodes` with the configured `maxNodes`.
7. Run `tally --config <rendered-json> flow check <script> --args <configured-json>`.
8. If `catalog` is non-null, run
   `tally --config <rendered-json> flow check <script> --catalog <store-path>`;
   otherwise reject any declared selector.

The first `flow check` parses the module, validates the literal `meta` object,
applies the deterministic-global lint, checks literal pool declarations, and
reparses the normalized script. The argument pass applies `meta.argsSchema`.
The catalog pass validates catalog schema and semantics and resolves literal
selector requests that can be known at build time.

These checks do not start the daemon and do not execute impure nodes. They prove
that the registered control program and its static inputs fit the deployed
configuration.

## Home Manager deployment only

Home Manager is the only wrapper that turns flows into runnable deployment
objects. It:

- adds the reserved pools when at least one flow is declared;
- creates `flow-<name>` calendar producers for entries with `onCalendar`;
- renders the associated systemd user services and timers; and
- passes runner credentials and environment through the generated unit.

The NixOS wrapper exposes the same twelve options and evaluates the same checked
derivation, so it can reject an invalid flow declaration. It does not add the
reserved pools or render flow producer services and timers. A NixOS flow entry
that evaluates successfully is therefore validation, not deployment.

## Flows are excluded from windowed consumption

Flow nodes deliberately have no `consumptionEstimate` field. A checked flow
may not declare a pool whose configured predicate is `windowed-consumption`.
The configured `tally flow check` fails before activation with the named error
`FlowPoolError`/`windowed-consumption-excluded`, identifying the pool and
stating that priorities are the control for contention between workloads.

This is a design position, not a temporary dialect limitation. Estimates do
not block flow work from completing. Give ordinary wave nodes a low priority;
a more important ask can then intercede midway through the run by design.
There is no runner-budget option that changes this rule.

The exclusion is flow-specific. Direct and producer enqueues retain the
kernel's existing windowed-consumption mechanism and may supply
`consumptionEstimate` when that admission policy is appropriate.
