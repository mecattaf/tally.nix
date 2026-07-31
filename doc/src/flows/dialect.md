# The dialect

Flow files use a deliberately small JavaScript dialect hosted by Boa. Ordinary
deterministic language features—objects, arrays, functions, promises, `async` and
`await`—remain available. Sources of ambient state are removed, and all impure
work crosses the [host API](host-api.md) as a tally node.

The source is JavaScript, not TypeScript. The one `export const meta` declaration
is parsed as a module prelude and then its `export` token is blanked; the remaining
program is evaluated as a Script with no module loader.

## The literal `meta` block

Every file has exactly one permitted export:

```javascript
export const meta = {
  name: "inventory-review",
  description: "Review a fixed inventory with a bounded model pool",
  pools: ["worker-gpu"],
  argsSchema: {
    type: "object",
    required: ["inventoryPath"],
    properties: {
      inventoryPath: { type: "string", pattern: "^/" }
    },
    additionalProperties: false
  },
  maxNodes: 8,
  iterationCap: 4,
  selectors: ["pooled-strongest"]
};
```

`meta` must be a JSON-compatible object literal. Function calls, identifiers,
template expressions, computed keys, object methods, shorthand properties,
spreads, array holes, `undefined`, and `BigInt` are rejected with
`FlowMetaError`/`meta-nonliteral`. Imports and any second export are also rejected.
The checker blanks only the `export` token before evaluating the file, so source
line and column positions stay stable and the script still has a local `meta`
constant.

The accepted fields are exact; unknown fields produce `meta-invalid`:

| Field | Contract |
|---|---|
| `name` | Required non-empty string and the flow name stamped on every node. |
| `description` | Required non-empty string for people and check output. |
| `pools` | Required array of unique, non-empty child pool names. Every ordinary node pool must appear here. |
| `argsSchema` | Required valid JSON Schema. `args` is checked when arguments are supplied. |
| `maxNodes` | Optional positive integer. The runtime uses the smaller of this value and `--max-nodes`. |
| `iterationCap` | Optional positive per-host-call-site cap; default `64`. It is distinct from the whole-run node cap. |
| `selectors` | Optional unique array, default empty. Every `members()` class must be declared here. |

### Width: the three caps and the host's fanout cap

Three different numbers bound how much a flow can materialize, and they are not
interchangeable:

| Bound | Kind | Scope |
|---|---|---|
| `meta.maxNodes` (intersected with `--max-nodes`) | Total | Nodes materialized over the whole run |
| `meta.iterationCap` | Total | Executions of one node-producing call site |
| `services.tally.enqueue.fanoutCap` | Width | Children of one parent job outstanding *at the same time* |

The effective width of a flow is therefore `min(maxNodes, iterationCap,
fanoutCap)`. Only `fanoutCap` is a width: the daemon charges a parent when a
child is created and returns the charge when that child reaches a terminal, so a
strictly sequential flow of 500 nodes never exceeds a `fanoutCap` of 64, while
65 concurrent nodes do. Full-mode admission defers the charge until `created` is
known, so a replayed prefix that answers `reused` or `terminal` costs no fanout.

The two ways of starting a flow are asymmetric. A declaratively registered flow
runs as a tally job, so its nodes are that job's children and `fanoutCap`
applies. A manual `tally flow run` invoked from a shell has no parent job, so
nothing charges fanout and the cap does not apply — the same script can succeed
by hand and be refused under its own timer.

Because a declared budget is checkable and a script's concurrency is not, the
generation build fails when a script's explicit `meta.maxNodes` exceeds the
host's `services.tally.enqueue.fanoutCap`. That is deliberately conservative: a
wave-based flow that declares a large total but only ever opens a few nodes at a
time would have run. The build still stops, because the declaration is the only
statement of width the host can read. Raise
`services.tally.enqueue.fanoutCap` to the declared budget, or lower
`meta.maxNodes` to what the flow really needs. A script that declares no
`meta.maxNodes` is not checked this way.

`flow` and `build` are reserved pools in declarative registrations. The Nix
checker rejects them in `meta.pools`: the runner owns `flow`, while `drv()` adds
`build` without requiring it in metadata.

There is no `meta.budgetPool` field in the shipped dialect, and the former Nix
`budgetPool` option has been removed because it never created a lease or render
channel. The runner always requests `flow` as its base pool.
Likewise, `workloadMutex` is a Nix registration option rather than flow
metadata. It adds one validated capacity-1 mutex to the generated runner's
`flow` lease for the process lifetime; it does not change node pool sets.

During execution the host also exposes the checked metadata as read-only
`flowMeta`, and exposes the checked invocation data as read-only `args`. Treat the
objects beneath those bindings as immutable inputs.

## What is unavailable

The host has no filesystem, network, module loader, process, or environment API.
There is no `require`, `fetch`, or Node.js `process`, and module imports are
forbidden. Use `job()` or its sugar when work needs those capabilities.

The determinism checker and runtime additionally close JavaScript ambient-state
escape hatches:

| Surface | Shipped behavior |
|---|---|
| `Date` | A direct use fails `tally flow check` with `determinism-violation`; the global is also deleted at runtime. |
| `Math.random()` | Direct access, and a computed access whose key is a string literal such as `Math["random"]`, both fail the static check. A computed access through a variable reaches a replacement that throws `FlowDeterminismError`/`determinism-violation`. |
| `WeakRef`, `FinalizationRegistry` | Rejected statically and deleted from the runtime. |
| `eval`, `Function` | Direct uses are rejected; Boa's compile-strings hook rejects indirect runtime compilation too. |
| Timers | No timer API is installed. A Boa timeout job that is nevertheless reached aborts the run, but it is classified `FlowScriptError`/`script-evaluation` — its message reads `FlowDeterminismError [determinism-violation]: timer jobs are forbidden` while its code does not. Both exit 10. |

The engine has distinct non-catchable runtime backstops. A flow gets 1,000,000
loop iterations and 512 recursive calls; breaching either is
`FlowRuntimeLimitError`/`runtime-limit`. Its two fixed evaluation budgets are:

| Budget | Fixed limit | Error |
|---|---:|---|
| Promise and generic microtasks | 100,000 | `FlowRuntimeBudgetError`/`microtask-budget` |
| Total wall clock, including awaited host work | 24 hours | [`FlowRuntimeBudgetError`/`wall-clock-budget`](submission-and-replay.md#continuation-after-budget-exhaustion) — replay the same run to continue |

Separately, each node-producing call site may execute only `meta.iterationCap`
times; the next call throws `FlowLoopError`/`iteration-cap`. The cap counts
node-producing calls only — `job()`, `drv()`, and the four sugars — so ordinary
loops, `members()`, and `log()` do not consume it. Set a deliberate higher cap
for a bounded fan-out rather than relying on an engine backstop.

```javascript
// Deterministic: the bound and order come from checked args.
const results = [];
for (const input of args.inputs) {
  results.push(await sh([args.worker, input], {
    pools: ["worker"],
    key: `input-${input}`,
    evidence: ["exit:0"]
  }));
}
```

## What `tally flow check` proves

The standalone checker always parses the module, extracts and validates `meta`,
runs the static determinism lint, checks literal pool declarations, and reparses
the normalized source as a Script. Optional flags add two checks:

- `--args JSON` validates the invocation against `meta.argsSchema`.
- `--catalog PATH` validates the catalog schema and semantics, proves every
  declared selector is non-empty, and resolves direct `members()` calls whose
  selector and options are literals. Dynamically assembled selector requests stay
  runtime-validated.

The selector catalog travels only through the `--catalog <store-path>` flag. A
declarative flow emits that flag only when `services.tally.flows.<name>.catalog`
is non-null. There is no `TALLY_FLOW_CATALOG` environment channel.

The Nix module makes these checks part of the generated configuration derivation.
For each declared flow it additionally proves that:

1. every metadata pool exists in `services.tally.pools`;
2. reserved `flow` and `build` are absent from `meta.pools`;
3. the configured `maxNodes` is at least `meta.maxNodes`;
4. configured `args` match the schema; and
5. selector use has a catalog and literal requests fit that catalog.

The last point is intentionally narrower for dynamic code. For example,
`members(args.selector, { count: args.count })` cannot be resolved during the
build; the same catalog resolver checks it when the script runs.

## Keep the replay surface small

Everything knowable at generation time should enter through `args`. A node result
should be compact JSON—identities, paths, digests, and decisions—not an opaque
dump of a worker filesystem. Construct every later node from literals, the checked
inputs, and already witnessed results. This keeps a replayed script able to derive
the same keys and payload hashes without access to the original process state.

The dialect is deterministic; workers are not assumed to be. Their effects become
safe inputs only after tally records a terminal witness and returns a `NodeResult`.
