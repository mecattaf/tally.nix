# Host API reference

The host API is the only way flow JavaScript can cross into impure work. Node
calls assign an ordinal synchronously and return a promise; the runner submits
those ordinals to the daemon in order, even when several promises are outstanding.

The snippets below are script fragments. Put one inside an async root expression
and declare every named pool or selector in `meta`:

```javascript
export const meta = {
  name: "api-example",
  description: "Host API example",
  pools: ["worker", "claude-window", "codex-window"],
  argsSchema: { type: "object" },
  maxNodes: 20,
  selectors: ["pooled-review"]
};

(async () => {
  // Put an example here and return its value.
})();
```

## The common `NodeResult`

`job()`, `drv()`, and the four job sugars resolve to the same shape:

```json
{
  "taskUuid": "…",
  "verdict": "pass",
  "exitCode": 0,
  "witnessSeq": 42,
  "disposition": "created",
  "result": { "digest": "sha256:…" },
  "gates": { "status": "pass" }
}
```

| Field | Meaning |
|---|---|
| `taskUuid` | Durable task identity returned by the daemon. |
| `verdict` | `pass`, `substituted`, `clean-exit-no-artifact`, `failed`, `skipped`, `cancelled`, `pool-vanished`, `preempted`, or `runtime-exceeded`. |
| `exitCode` | Process exit code when one exists. |
| `witnessSeq` | Positive sequence of the terminal witness. Promise observation is ordered by this value. |
| `disposition` | How this invocation met the work: `created`, `attached`, `reused`, `substituted`, or `terminal`. |
| `result` | Optional `finalMessage` projection. JSON text is decoded; other text remains a string. |
| `gates` | Optional gate projection from completion evidence. |
| `error` | Optional `{ code, message, details }`, notably for settled result-schema failures. |

Without settle mode, only `pass` and `substituted` verdicts resolve. Other
terminal verdicts reject with `FlowTerminalError`/`terminal-failure`. A declared
`resultSchema` must receive a result and validate it; otherwise the promise rejects
with `FlowResultError`/`result-schema-mismatch`.

Settle mode changes those two post-terminal failures into a resolved `NodeResult`
so the script can decide what to do. It does **not** turn invalid specs, admission
errors, daemon errors, key conflicts, or replay divergence into results.

## `job(spec, options?)`

`job()` is the general node primitive:

```javascript
const result = await job(
  {
    argv: [args.worker, "inspect"],
    adapter: "shell",
    pools: ["worker"],
    priority: "low",
    runtimeMaxSec: 600,
    key: "inspect",
    label: "inventory-inspection",
    brief: { inventoryPath: args.inventoryPath },
    evidence: ["exit:0", `artifact:${args.reportPath}`, "hash:sha256"],
    resultSchema: {
      type: "object",
      required: ["digest"],
      properties: { digest: { type: "string" } },
      additionalProperties: false
    }
  },
  { settle: true }
);
```

The first argument must be a JSON-serializable object. Its public fields are:

| Field | Contract |
|---|---|
| `argv` | Non-empty string array with a non-empty executable and no NUL bytes. Exactly one of `argv` or `adapter` + `prompt` is required. |
| `adapter` | Adapter name. It defaults to `shell` for an `argv` node. |
| `prompt` | Non-empty prompt for a named adapter. The host converts it to the structured brief transport; it cannot be combined with `argv` or `brief`. |
| `pools` | Required non-empty, duplicate-free array. Every ordinary pool must appear in `meta.pools`. The host sorts the array before hashing and submission. |
| `executor` | Optional configured executor name. Absence means the daemon's local executor. |
| `priority` | Optional `interrupt`, `high`, `medium`, or `low`. |
| `runtimeMaxSec` | Optional positive process deadline. |
| `evidence` | Optional canonical evidence list: `exit:<0..255>`, `artifact:<absolute-path>`, `store:<nix-store-path>`, and at most one `hash:sha256[:<digest>]`. |
| `evidenceClass`, `manifestHash` | Optional evidence-policy values passed to the daemon. |
| `workspace` | Optional object with exactly `repo`, `baseRev`, `branch`, and absolute `worktreePath` strings. |
| `brief` | Optional structured JSON object delivered out of band through `TALLY_BRIEF`. |
| `key` | Optional flow-local author key, rendered as `flow:<run-id>:k:<key>`. It must be unique in one evaluation. |
| `dedupKey` | Optional raw, potentially cross-run key. It is mutually exclusive with `key`; use it only when cross-run identity is intentional. |
| `label` | Optional human-readable node label stored in orchestration provenance. |
| `env` | Optional string map. Names beginning `TALLY_` and `CREDENTIALS_DIRECTORY` are reserved. |
| `resultSchema` | Optional valid JSON Schema for the projected result. It is checked by the runner after terminal acknowledgement. |

There is deliberately no `consumptionEstimate` field. An unknown field is
`FlowSpecError`/`unknown-spec-field`, and configured flow checking rejects a
declared `windowed-consumption` pool with
`FlowPoolError`/`windowed-consumption-excluded`. Priorities, rather than
estimates, control contention between flow workloads.

The optional second argument may contain only boolean `settle`. Shape and value
errors are `FlowSpecError`/`invalid-options`. Other important classes are
`FlowPoolError` for pool declarations, `FlowEvidenceError` for evidence,
`FlowEnvironmentError` for environment names, `FlowKeyError` for key misuse,
`FlowLoopError` for a call-site cap, `FlowAdmissionDenied` or
`FlowNodeCapError` from admission, `FlowDedupKeyConflict` for a raw key collision,
`FlowReplayError` for fatal same-run divergence, and `FlowTerminalError` or
`FlowResultError` after completion.

## `drv(spec, options?)`

`drv()` is the store-native form of a node:

```javascript
const built = await drv({
  drvPath: "/nix/store/00000000000000000000000000000000-report.drv",
  outputs: [
    {
      name: "out",
      path: "/nix/store/11111111111111111111111111111111-report"
    }
  ]
});
```

The spec has exactly `drvPath` and non-empty `outputs`. The derivation must be a
canonical `/nix/store/...drv` path. Output names must be unique; the host sorts
them by name, and every output path must be a canonical Nix store path. The second
argument has the same `{ settle }` shape as `job()`.

The mapping is fixed: shell adapter, `build` pool, `nix build --no-link
<drvPath>^*`, `store:<path>` evidence for every output, and global dedup key
`drv:<drvPath>`. Authors do not declare `build` in `meta.pools`. A Home Manager
flow registration adds the reserved pool with capacity 2; other deployments must
provide that pool themselves.

If all outputs are already available or substitutable, the daemon creates no job
row and takes no build lease. It appends a cheap witness and returns verdict and
disposition `substituted`. Otherwise the promise follows ordinary job admission.
The task UUID is deterministically seeded from the flow run and ordinal so replay
uses the same identity.

Malformed paths, duplicate output names, unknown fields, or a shape inconsistent
with the fixed mapping raise `FlowSpecError`/`invalid-derivation`. After mapping,
the ordinary admission, replay, terminal, and settle rules apply.

## `claude()`, `codex()`, `local()`, and `sh()`

The sugars fill adapter-specific fields and then use `job()`:

| Call | Fixed adapter | Pools |
|---|---|---|
| `claude(prompt, opts?)` | `claude-code` | exactly `["claude-window"]` |
| `codex(prompt, opts?)` | `codex` | exactly `["codex-window"]` |
| `local(prompt, { member, ...opts })` | selected catalog member's `adapter` | selected member's `pools` |
| `sh(argv, opts)` | `shell` | caller supplies `opts.pools` |

```javascript
const implementation = await codex("Implement the checked plan", {
  key: "implementation",
  workspace: {
    repo: "mecattaf/tally.nix",
    baseRev: "main",
    branch: "flow-example",
    worktreePath: "/worktrees/flow-example"
  },
  resultSchema: { type: "object" }
});

const review = await claude(
  `Review this result: ${JSON.stringify(implementation.result)}`,
  { key: "review" }
);

const verified = await sh(["git", "status", "--short"], {
  pools: ["worker"],
  key: "verify",
  evidence: ["exit:0"]
});
```

Agent prompts are carried in a structured brief rather than interpolated into
adapter argv. The host stamps a SHA-256 `promptRevision`; it also stamps the
configured adapter skill revision when one exists. These values become witnessed
orchestration provenance. `claude` and `codex` reject attempts to override their
adapter, pools, argv, prompt, or brief with
`FlowSpecError`/`sugar-option-conflict`. `sh` fixes argv and adapter but permits a
structured `brief`.

The first argument to `claude()`, `codex()`, and `local()` must be a string; a
different type is `FlowSpecError`/`invalid-argument`. The sugar currently accepts
an empty string because it places that value directly in the structured brief,
even though a raw `job()` `prompt` must be non-empty. `sh()` takes the same argv
shape as `job()` and reports malformed input through the ordinary spec errors.

`local()` requires the exact member object returned by `members()`, including its
selection provenance:

```javascript
const member = members("pooled-review", { count: 1 })[0];
const result = await local("Review the candidate", {
  member,
  key: `review-${member.id}`,
  settle: true
});
```

Although the decoder recognizes a bare member ID, the subsequent provenance check
requires the returned object; a string therefore fails
`FlowSelectorError`/`selection-provenance-missing` in the shipped implementation.
An unknown member, stale catalog hash, or forged selection raises another
`FlowSelectorError`. Missing catalog data is `FlowCatalogError`.

All four option objects accept the applicable `job()` fields plus `settle`; fields
fixed by the sugar raise `sugar-option-conflict`, other unknown fields raise
`unknown-spec-field`, and the resulting promise has the ordinary `NodeResult` and
failure behavior.

Each sugar fixes a different set. `claude()` and `codex()` fix `adapter`, `pools`,
`argv`, `prompt`, and `brief`; `local()` fixes those plus `adapterOptions` and
`selection`; `sh()` fixes only `adapter`, `argv`, and `prompt`, so `sh()` is the
one sugar whose caller chooses `pools`. A literal options object that sets a fixed
field is rejected by `tally flow check` and by the generation build, not only at
evaluation time — `claude(prompt, { pools: [...] })` fails at switch.

## `parallel(thunks, options?)`

`parallel()` takes an array of zero-argument functions. Thunks are invoked in
array order, so their node ordinals are deterministic, and all returned promises
may remain in flight together:

```javascript
const results = await parallel([
  () => sh([args.worker, "a"], { pools: ["worker"], key: "a" }),
  () => sh([args.worker, "b"], { pools: ["worker"], key: "b" })
]);
```

Success returns values in input order. The default waits for every branch and then
throws `FlowAggregateError`/`aggregate-failure` if any failed; the error has an
`outcomes` array of `{ ok: true, value }` or `{ ok: false, error }`.

With combinator settle mode, that outcome array is returned instead:

```javascript
const outcomes = await parallel(
  workers.map(worker => () => sh([worker], { pools: ["worker"] })),
  { settle: true }
);
```

This is separate from node settle mode. A `sh(..., { settle: true })` terminal
failure is a successful branch whose `value` is a failed `NodeResult`; a rejecting
`sh()` becomes `{ ok: false, error }`. Non-function entries or options other than
boolean `settle` raise `FlowCombinatorError`/`parallel-invalid` synchronously.

Every thunk must return a promise. A thunk that returns anything else — most often
the brace mistake `() => { sh(...) }`, which creates a node and then returns
`undefined` — raises `FlowCombinatorError`/`parallel-invalid` naming the thunk's
index, located at the `parallel()` call. That failure is not a branch outcome: it
fails the combinator even under `{ settle: true }`, because it is an authoring
mistake rather than a domain failure.

## `pipeline(items, ...stages, options?)`

`pipeline()` creates one promise chain per item. Each stage receives
`(previous, originalItem, index)`:

```javascript
const outputs = await pipeline(
  ["a", "b"],
  (_previous, item) =>
    sh([args.worker, "prepare", item], {
      pools: ["worker"],
      key: `prepare-${item}`
    }),
  (prepared, item) =>
    sh([args.worker, "publish", prepared.result.path], {
      pools: ["worker"],
      key: `publish-${item}`
    })
);
```

There is no stage barrier. Item `a` may enter `publish` as soon as its `prepare`
promise is observed, even while item `b` is still preparing. Continuations from
the just-observed node are allowed to materialize their next ordinal before the
runner releases another ready terminal result.

The return and `{ settle: true }` shapes match `parallel()`. A rejecting stage
skips that item's later stages, but other item chains continue. A settled failed
`NodeResult` is a resolved value and reaches the next stage unless the script
checks it. Bad item, stage, or option shapes raise
`FlowCombinatorError`/`pipeline-invalid`; branch failures aggregate as
`FlowAggregateError`/`aggregate-failure`.

Every stage must return a promise, including a stage that only reshapes the
previous value — declare such a stage `async`. A stage that returns anything else
raises `FlowCombinatorError`/`pipeline-invalid` naming the stage index and the item
index, located at the `pipeline()` call, and fails the combinator even under
`{ settle: true }`.

## `log(value)`

`log()` queues one JSON lifecycle event and returns `undefined`:

```javascript
log({ phase: "review", subjects: args.subjects.length });
const result = await sh([args.worker], {
  pools: ["worker"],
  key: "review"
});
log({ phase: "complete", witnessSeq: result.witnessSeq });
```

A log immediately before a node is emitted only when that node's disposition is
`created`. Replaying an already admitted prefix therefore suppresses its earlier
logs. Logs after the final node have no following disposition to key against and
are flushed at script exit, so a tail log can appear again on replay. `undefined`
is recorded as JSON `null`. A cyclic or otherwise unconvertible value throws a
JavaScript error and becomes a script failure. A later serialization or write
failure in the lifecycle output itself is
`FlowCaptureError`/`lifecycle-serialization` or `lifecycle-write`.

## `members(selector, options?)`

`members()` synchronously resolves a declared class from the active catalog:

```javascript
const selected = members("pooled-review", {
  count: 3,
  diversity: "maker"
});
```

`selector` is a non-empty string present in `meta.selectors`. `count` is an
optional positive integer and defaults to every matching member. `diversity` is
optional and is exactly `family` or `maker`. It round-robins the matching catalog
groups before taking `count`; it is a preference order, not a promise that all
returned diversity values are distinct.

The result is an array of catalog member objects in deterministic catalog order.
Each object includes `id`, `family`, `maker`, `classes`, `adapter`, `pools`,
`launch`, optional catalog metadata, and a host-added `selection` object containing
the selector, catalog hash, selected member ID, and the complete selected ID list.
That provenance is what `local()` verifies.

Resolution also emits a `selector-resolved` event to the runner's lifecycle
stream before a selected member can be submitted. It contains the selector,
options, catalog hash, and ordered member IDs. The event is runner capture rather
than a new daemon RPC; the same resolution is persisted on every selected node
through its orchestration `selection` object.

That selection object is provenance, not work identity: the daemon deliberately
excludes orchestration metadata from `payloadHash`. Changing member adapter,
pools, or launch options normally changes the work payload, while changing only
a member ID, order, or catalog hash does not necessarily do so. Independently,
the runner pins the exact catalog bytes for the entire `flowRunId`; any catalog
byte change fails as `catalog-changed-mid-run` before node reuse or admission.

Missing or undeclared classes, zero or excessive counts, unsupported diversity,
and insufficient members are typed `FlowSelectorError`s. No catalog or an invalid
catalog is `FlowCatalogError`. Literal calls are also checked during activation;
dynamic calls fail at runtime.

## `attributed(member, candidate)` and `repairKey(member)`

These small synchronous helpers keep pooled results tied to their source:

```javascript
const row = attributed(selected[0], outcomes[0]);
const repair = await local("Repair the contract", {
  member: selected[0],
  key: repairKey(selected[0]),
  settle: true
});
```

`attributed()` accepts a non-empty member ID or member object and returns
`{ memberId, candidate }`. A bad member raises
`FlowDissentError`/`attribution-invalid`. `repairKey()` accepts the same identity
and returns the single-attempt convention `<member-id>@1`; a bad member raises
`FlowRepairError`/`repair-member-invalid`. The helper does not count attempts—the
returned key and the script's control flow make the bound explicit.

## `quorum(declaration)`

`quorum()` classifies attributed results against an exact required roster:

```javascript
const accepted = quorum({
  results: rows,
  minimumValid: 2,
  requiredMembers: selected.map(member => member.id),
  allowPartial: true
});
```

The declaration has exactly four fields. `results` and `requiredMembers` are
arrays; required IDs are non-empty and unique. `minimumValid` is an integer from 1
through the roster length. `allowPartial` defaults to `false`. A result is valid
only when its outcome succeeded, its node verdict is exactly `pass`, and it has no
`error`.

The return shape is:

```javascript
{
  requiredMembers,
  minimumValid,
  allowPartial,
  valid: [{ memberId, result: nodeResult }],
  invalid: [{ memberId, status: "invalid", outcome }],
  missing: [{ memberId, status: "missing" }]
}
```

With `allowPartial: false`, every required member must be valid regardless of the
minimum. With `true`, reaching `minimumValid` is enough. Bad declarations and
duplicate attributed rows raise `FlowQuorumError`/`quorum-invalid`. A failed
threshold raises `FlowQuorumError`/`quorum-not-met` and attaches the same summary
as `error.quorum`, allowing one bounded repair pass before trying again.

## `dissent(declaration)`

`dissent()` validates, copies, and returns an attributed conclusion ledger:

```javascript
const reduction = dissent({
  conclusions: [
    {
      conclusion: "Ship after the migration test",
      support: ["reviewer-a", "reviewer-b"],
      conflict: ["reviewer-c"]
    }
  ],
  excluded: [{ memberId: "reviewer-d", reason: "invalid" }]
});
```

Every conclusion must have a value, a non-empty unique `support` array, and a
unique `conflict` array disjoint from support. Both arrays contain member IDs.
`excluded` defaults to empty and each row has exactly string `memberId` and
`reason`. The result is `{ conclusions, excluded }` with copied arrays.

Bad declaration shapes raise `FlowDissentError`/`dissent-invalid`; missing,
duplicate, overlapping, or malformed attribution raises
`FlowDissentError`/`dissent-attribution-missing`. The helper validates attribution
shape, not the truth of the conclusion or membership in a particular quorum; the
script must supply the accepted member IDs.
