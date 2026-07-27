# Pooled-review cookbook

[`pooled-review.js`](https://github.com/mecattaf/tally.nix/blob/main/examples/flows/pooled-review.js)
is the complete small example of catalog selection, bounded repair, quorum, and
dissent-preserving reduction. It asks three catalog members for independent
reviews, repairs each bad or missing review once, and then asks one member to
reduce the surviving evidence without erasing disagreement.

The example is deliberately strict. A candidate is not accepted merely because a
process exited successfully: its projected result must satisfy a JSON Schema, its
node must have a `pass` verdict, and the quorum must contain the requested number
of valid members.

## 1. Declare the resource and selection boundaries

The header tells activation and the runner what the script can use:

```javascript
export const meta = {
  name: "pooled-review",
  pools: ["worker-gpu"],
  argsSchema: {
    type: "object",
    required: ["subject", "minimumValid"],
    properties: {
      subject: { type: "string", minLength: 1 },
      minimumValid: { type: "integer", minimum: 1, maximum: 3 }
    },
    additionalProperties: false
  },
  maxNodes: 8,
  selectors: ["pooled-strongest"]
};
```

`worker-gpu` is the workload pool used by every selected member. It is not the
runner's own pool: generated flow runners always acquire only `flow`. The catalog
class is declared in `selectors`, so a literal call to
`members("pooled-strongest", ...)` can be checked during activation.

The eight-node ceiling is an actual worst-case count:

- three initial candidates;
- at most three candidate repairs; and
- one reducer plus at most one reducer repair.

Adding another retry without raising `maxNodes` would turn the final admission
into `FlowNodeCapError`/`flow-node-cap` instead of silently expanding the run.

## 2. Render a typed catalog with Nix

Keep the roster in the consuming configuration and let the supported helper
render the versioned catalog:

```nix
let
  reviewCatalog = tally.lib.tally.mkCatalog {
    inherit pkgs;

    classes.pooled-strongest.diversity = [ "family" "maker" ];
    pools = [ "worker-gpu" ];

    members = {
      qwen-coder = {
        order = 10;
        family = "qwen";
        maker = "alibaba";
        classes = [ "pooled-strongest" ];
        adapter = "pi";
        pools = [ "worker-gpu" ];
        launch.model = "qwen-coder";
      };
      llama-review = {
        order = 20;
        family = "llama";
        maker = "meta";
        classes = [ "pooled-strongest" ];
        adapter = "pi";
        pools = [ "worker-gpu" ];
        launch.model = "llama-review";
      };
      mistral-review = {
        order = 30;
        family = "mistral";
        maker = "mistral";
        classes = [ "pooled-strongest" ];
        adapter = "pi";
        pools = [ "worker-gpu" ];
        launch.model = "mistral-review";
      };
    };
  };
in {
  services.tally = {
    enable = true;

    pools.worker-gpu = {
      resource = "vram";
      capacity = 1;
    };

    flows.pooled-review = {
      script = ./flows/pooled-review.js;
      catalog = reviewCatalog;
      args = {
        subject = "the change under review";
        minimumValid = 2;
      };
    };
  };
}
```

The member attribute name becomes `id` unless an explicit ID is supplied. `order`
controls emitted array order, with the attribute name as a deterministic tie
breaker. A disabled member remains in the Nix source but is not emitted. The
helper validates member, class, pool, diversity, launch, and ordering data with
typed Nix submodules and validates the rendered file with the real
`tally flow check` binary.

The consuming flow check then evaluates the script with that catalog. It rejects
an empty declared selector, an excessive literal `count`, an unsupported literal
diversity key, or a member that refers to an unknown pool before activation.
Dynamic selector arguments retain the same checks at runtime.

The shipped catalog channel is narrower than an environment-based design: the
generated runner receives `--catalog <store-path>` only when `catalog` is set.
There is no `TALLY_FLOW_CATALOG` fallback. A manual invocation must therefore say:

```console
$ tally flow run ./flows/pooled-review.js \
    --flow-run-id pooled-review-manual-1 \
    --catalog "$catalog_path" \
    --args '{"subject":"the change under review","minimumValid":2}'
```

Before scheduling it, check the exact pair that will run:

```console
$ tally flow check ./flows/pooled-review.js --catalog "$catalog_path"
```

## 3. Select members; do not infer concurrency

The script resolves a deterministic roster synchronously:

```javascript
const selected = members("pooled-strongest", {
  count: 3,
  diversity: "family"
});
const requiredMembers = selected.map(member => member.id);
```

`members()` returns catalog objects carrying selection provenance. Pass those
objects unchanged to `local()`; a bare member ID is not a usable substitute in
the shipped implementation. Family diversity round-robins groups before taking
three entries. It is a preference order, not a guarantee that all three families
are distinct when the catalog cannot supply that.

Selection is membership, not capacity. All three member objects declare the same
`worker-gpu` pool, and the configuration above gives that pool capacity 1. The
three submitted jobs can coexist durably, but only one holds the pool lease at a
time. Raise capacity only when the host can safely co-reside the corresponding
workloads; do not change the selector to pretend concurrency exists.

## 4. Capture every initial outcome

The candidate contract is a recommendation and an evidence array. Each local
node uses both node settle mode and a schema:

```javascript
const initial = await parallel(
  selected.map(member => () =>
    local(`Review this subject and cite concrete evidence: ${args.subject}`, {
      member,
      settle: true,
      resultSchema: candidateSchema,
      label: `review-${member.id}`
    })
  ),
  { settle: true }
);
const rows = initial.map((outcome, index) =>
  attributed(selected[index], outcome)
);
```

The two settle switches have different jobs. `local(..., { settle: true })`
turns a terminally failed node into a returned `NodeResult`. The outer
`parallel(..., { settle: true })` turns a rejected branch into an
`{ ok: false, error }` outcome instead of throwing an aggregate error. Successful
branches are `{ ok: true, value: NodeResult }`. `attributed()` binds each wrapper
back to the member selected at the same array index.

Without both layers, one failed reviewer could abort the JavaScript before the
script had classified the complete roster. Neither layer changes the durable
verdict or makes a failure pass.

## 5. Classify, repair once, and classify again

The first quorum call is a diagnostic pass:

```javascript
let assessed;
try {
  assessed = quorum({
    results: rows,
    minimumValid: args.minimumValid,
    requiredMembers,
    allowPartial: true
  });
} catch (error) {
  if (error.code !== "quorum-not-met") throw error;
  assessed = error.quorum;
}
```

`allowPartial: true` means a roster need not be entirely valid, but it does not
weaken `minimumValid`. If too few pass, `quorum()` throws
`FlowQuorumError`/`quorum-not-met` and attaches the same summary at
`error.quorum`. That summary partitions the exact required roster into `valid`,
`invalid`, and `missing` rows.

The example repairs every invalid or missing member once:

```javascript
for (const memberId of repairIds) {
  const member = selected.find(candidate => candidate.id === memberId);
  const repaired = await local("Return only the required review contract", {
    member,
    key: repairKey(member),
    settle: true,
    resultSchema: candidateSchema,
    label: `repair-${member.id}`
  });
  rows[requiredMembers.indexOf(memberId)] = attributed(member, repaired);
}
```

`repairKey(member)` is only the convention `<member-id>@1`; it does not manage a
retry counter. The `for` loop and single call enforce the one-repair bound. Repairs
are sequential in this example, although the capacity-1 pool would serialize
them even if they were submitted together.

The second `quorum()` is intentionally not caught. If the repaired roster still
has fewer than `minimumValid` passing results, the flow fails rather than
manufacturing a reduction from inadequate evidence.

## 6. Reduce without deleting dissent

The reducer receives `accepted.valid`, including each member attribution, and its
schema requires every conclusion to name non-empty `support` and possibly empty
`conflict` member-ID arrays. It runs on the first selected member under the stable
flow-local key `dissent-reducer`.

The initial reducer uses node settle mode so the script can make exactly one
repair decision. If its verdict is not `pass` or it carries an error, the repair
uses `dissent-reducer@1` without settle mode. A second failure is therefore loud
and terminal.

Finally:

```javascript
return {
  quorum: accepted,
  reduction: dissent({
    conclusions: reducer.result.conclusions,
    excluded: accepted.invalid
      .concat(accepted.missing)
      .map(row => ({ memberId: row.memberId, reason: row.status }))
  })
};
```

`dissent()` validates attribution shape; it does not judge the prose. It rejects
missing support, duplicate support or conflict IDs, overlap between those arrays,
and malformed excluded rows. The returned reduction preserves the normalized
conclusions plus every roster member excluded from the accepted evidence.

## Failure and replay predictions

| Event | Result |
|---|---|
| One initial reviewer fails or violates the candidate schema | It is classified `invalid`; that member receives its one repair. |
| A selected result never appears in the supplied rows | It is classified `missing`; that member receives its one repair. |
| Too few candidates are valid after repair | The final `quorum()` throws `quorum-not-met`; no reducer node is admitted. |
| The first reducer fails its schema | One reducer repair is admitted; a bad repair rejects the flow. |
| The runner dies during the batch | Replay reselects the same roster, reuses completed nodes, attaches to live ones, and follows the same repair decisions. |
| Catalog launch data, adapter, pools, or arguments change under the same run ID | If canonical work changes, the affected node diverges and the runner stops. |
| Only catalog member identity, order, or provenance changes while execution data stays identical | Selection is outside `payloadHash`, so an old node can reuse and be attributed under the newly selected row. Do not replay with changed catalog bytes. |
| The GPU pool has capacity 1 | Jobs wait and drain sequentially; this is not a quorum failure. |

This pattern is useful when the disagreement is part of the evidence. If the
desired result is simply “first success wins,” this cookbook is the wrong shape:
the flow engine intentionally provides neither an unwitnessed completion race nor
a cancellation primitive that would make that race replay-safe.
