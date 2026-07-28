# Two more cookbook recipes

The [pooled-review cookbook](pooled-review.md) covers catalog selection, bounded
repair, and quorum. These two recipes cover the other shapes an author reaches
for first: fanning out over a worklist the flow itself discovered, and saying
"this input is unacceptable" without pretending the script broke.

Both ship as executable examples that the flake checks against representative
arguments:

- [`worklist-fanout.js`](https://github.com/mecattaf/tally.nix/blob/main/examples/flows/worklist-fanout.js)
- [`domain-failure.js`](https://github.com/mecattaf/tally.nix/blob/main/examples/flows/domain-failure.js)

## Bounded fan-out over a witnessed worklist

The flow does not know its own work at authoring time. A first node discovers
it, and every later node is derived from that node's witnessed result.

### The brief and the result schema are the contract

```javascript
const worklist = await sh(
  ["tally-worklist", "--repository", args.repository, "--label", args.label],
  {
    pools: ["worker-cpu"],
    key: "worklist",
    evidence: ["exit:0"],
    resultSchema: worklistSchema
  }
);
```

The pair — what the node is asked for, and the schema its answer must satisfy —
is the whole interface. Nothing downstream reads free text. A worker that
answers off-contract fails at this node, where the failure names one node, rather
than corrupting every key derived from its answer.

Constrain the identifiers, not just their type. `worklistSchema` requires
`id` to match `^[a-z0-9-]+$`, because those ids become node keys; an id
containing a colon would produce a key that reads like a different addressing
scheme.

### Keys come from the witnessed result

```javascript
const items = worklist.result.items.slice(0, args.waveSize);
```

`items` is a projection of a witnessed value and of checked `args`, so a second
run of the same flow run ID re-derives exactly the same keys in the same order
and replays the admitted prefix. Deriving a key from anything the host cannot
reproduce — a counter, an iteration order over an unordered set — turns replay
into `replay-divergence`.

### Combinator settle keeps one failure from suppressing the culmination

```javascript
const wave = await parallel(
  items.map(item => () =>
    sh(["tally-task", "--id", item.id], { /* … */ })
  ),
  { settle: true }
);
```

Without `{ settle: true }`, the first failed task rejects `parallel()` with
`FlowAggregateError` and the culmination never runs — so a wave of twelve tasks
produces nothing because one of them failed. With it, each branch is
`{ ok: true, value }` or `{ ok: false, error }` and the script decides.

This is combinator settle, not node settle. `sh(..., { settle: true })` turns a
failed *terminal* into a resolved value; `parallel(..., { settle: true })` turns
a *rejecting branch* into an outcome record. The recipe uses both: node settle on
the repair pass, so a second failure is data rather than a rejection.

Note that a thunk must return a promise. `() => { sh(...) }` creates its node and
returns `undefined`; that is `FlowCombinatorError`/`parallel-invalid` naming the
thunk index, and it fails the combinator even under `{ settle: true }`.

### Budget the repair headroom in `maxNodes`

```javascript
// 1 worklist + 8 tasks + 8 repairs + 1 culmination.
maxNodes: 18,
iterationCap: 8,
```

`maxNodes` is an arithmetic worst case, not a guess: one repair per item, and no
repair of a repair. `iterationCap` is separate — it bounds how many times one
*call site* may produce a node, and the fan-out has two such sites, each capped
at the eight-item wave.

### The three caps

`maxNodes` and `iterationCap` are totals; `services.tally.enqueue.fanoutCap` is a
width. Effective width is `min(maxNodes, iterationCap, fanoutCap)`. This recipe's
wave opens at most eight nodes at once, so it fits comfortably under the default
`fanoutCap` of 64 — but a wave sized from an unbounded worklist would not, and a
declaratively registered flow *is* bounded because its runner is a parent job.
See [Width: the three caps and the host's fanout
cap](dialect.md#width-the-three-caps-and-the-hosts-fanout-cap).

### Pool declaration is the author's duty

`meta.pools` lists `worker-cpu` and `claude-window`. The first is chosen by the
script on each `sh()` call. The second is *fixed* by `claude()` — the sugar sets
its own pool, and setting `pools` on a `claude()`, `codex()`, or `local()` call is
`sugar-option-conflict` at `tally flow check`. It still has to be declared,
because activation closes `meta.pools` against the configured pool set.

## Expressing domain failure

A custom `throw` in a flow script becomes `FlowScriptError`/`script-evaluation`
and exits 10 — the same class as a typo or a null dereference. An operator
reading exit 10 at 2am cannot tell "this invoice is unpayable" from "the script is
broken", and neither can a retry policy.

So a domain outcome is data. Make the node answer with a discriminated envelope
and let `resultSchema` enforce it:

```javascript
const decisionSchema = {
  type: "object",
  required: ["status"],
  oneOf: [
    {
      required: ["status", "amount"],
      properties: {
        status: { const: "payable" },
        amount: { type: "number", exclusiveMinimum: 0 }
      },
      additionalProperties: false
    },
    {
      required: ["status", "reason"],
      properties: {
        status: { const: "rejected" },
        reason: { type: "string", enum: ["duplicate", "unbalanced", "expired"] }
      },
      additionalProperties: false
    }
  ]
};
```

Branching on the discriminant is then ordinary control flow:

```javascript
if (decision.result.status === "rejected") {
  await sh(["tally-invoice", "--file-rejection", args.invoice, decision.result.reason], {
    /* … */
  });
  return { invoice: args.invoice, paid: false, reason: decision.result.reason };
}
```

Neither branch throws, so neither is reported as a script defect. The decision is
in the witnessed result, where a later node and an operator can both read it, and
the rejection reason is drawn from a closed enum rather than a free string.

A genuine infrastructure failure is still a failure. The node's own verdict
rejects its promise, the run exits on the terminal error, and nothing pretends
the invoice was paid. The envelope separates *the answer was no* from *we could
not get an answer* — which is exactly the distinction exit 10 was destroying.
