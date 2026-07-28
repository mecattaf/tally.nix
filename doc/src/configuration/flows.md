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
| [`budgetPool`](core-options.md#servicestallyflowsnamebudgetpool) | Optional pool name with existence-only validation. It does not alter the runner or node pool sets. |
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

The producer uses the `shell` adapter, requests only the reserved `flow` pool,
and copies `priority`, `dedupKey`, `runtimeMaxSec`, `evidence`, `extraEnv`, and
`credentials` from the flow entry. There is no extra catalog environment
variable: a non-null `catalog` becomes the explicit `--catalog` flag.

`dedupKey` is expanded for a scheduled firing using the supported `strftime`
grammar. The default gives one existence key per flow name and calendar day.
Choose a different bounded template when the intended cadence or idempotence
window is different. An empty key or an unsupported conversion fails the
checked configuration instead of waiting for the timer.

An `onCalendar = null` entry still goes through every build-time check. It can
be run manually with `tally flow run`, but no `flow-<name>` producer unit is
created for it.

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

## `budgetPool` is existence-check-only

The generated option description is deliberately explicit:

> Optional pool name checked for existence only. It is not added to the runner
> or node pool set and creates no render channel; the declarative runner remains
> admitted solely through the reserved "flow" pool.

When non-null, `budgetPool` must name an entry in `services.tally.pools`. That is
the complete effect. The value is not added to the runner's pool list, passed to
`tally flow run`, exported in an environment variable, or attached to child
submissions. It therefore cannot reserve programmatic budget for the duration
of a flow.

This option should not be mistaken for a general run-lifetime lease. The actual
generated runner pool set is always:

```json
{"pool":["flow"]}
```

## Generation-time validation

Both wrappers make the rendered configuration depend on a checked derivation.
Consequently a bad declaration stops `home-manager switch` or a NixOS
generation build before activation. The configured tally package, rather than
a second Nix reimplementation of the flow language, performs the checks.

The pipeline is:

1. Render the complete runtime JSON and run
   `tally --mode check-config --config <rendered-json>`.
2. For each entry, run `tally flow check <script>` and capture its normalized
   metadata.
3. Reject any `meta.pools` name absent from `services.tally.pools`.
4. Reject reserved `flow` or `build` in `meta.pools`.
5. Compare a literal `meta.maxNodes` with the configured `maxNodes`.
6. Run `tally flow check <script> --args <configured-json>`.
7. If `catalog` is non-null, run
   `tally flow check <script> --catalog <store-path>`; otherwise reject any
   declared selector.

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

## Current windowed-consumption limitation

Manual and producer enqueues can supply `consumptionEstimate`, which every
`windowed-consumption` pool requires before admission. The shipped flow
`NodeSpec` has no `consumptionEstimate` field. A flow node assigned to such a
pool is therefore rejected with:

```text
windowed-consumption pool "<name>" requires consumptionEstimate
```

Setting the Nix `budgetPool` option does not work around this limitation: as
described above, that field only checks that a pool name exists. Use a
co-residency pool for flow nodes today, or submit budgeted work through a
surface that can carry an authoritative estimate. Do not infer consumption
from adapter argv or scraped usage; the estimate is an admission input, while
meter observations can only clamp headroom after the fact.
