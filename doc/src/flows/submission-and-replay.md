# Submission identity and replay

The runner has no journal and does not checkpoint a JavaScript heap. Its recovery
algorithm is simpler: execute the script again from the first byte, derive the
same node identities and payloads, and ask the daemon what already happened.
Durable jobs and terminal witnesses are the state.

That design is trustworthy only when replay is strict. Tally refuses to continue
past a script or payload mismatch instead of guessing that two pieces of work were
"close enough."

## Three identity choices

Every node has a deduplication key. Choose its scope deliberately:

| Script field | Rendered key | Intended scope |
|---|---|---|
| neither `key` nor `dedupKey` | `flow:<flowRunId>:<ordinal>` | Default identity for this exact call order in one run. |
| `key: "review"` | `flow:<flowRunId>:k:review` | Named, flow-local identity that survives refactoring around the call order. |
| `dedupKey: "monthly-review-2026-07"` | unchanged | Advanced cross-run identity. The author owns its global uniqueness and payload stability. |

An ordinal is assigned synchronously when a host call is made, before the returned
promise settles. `parallel()` invokes thunks in array order, so its ordinal stream
is deterministic. Reusing the same `key` twice in one evaluation fails immediately
with `FlowKeyError`/`duplicate-key`; setting both fields fails with `key-conflict`.

The daemon also records two hashes:

- `scriptHash` is SHA-256 over the exact flow source bytes and is stamped on every
  node in the run.
- `payloadHash` covers the canonical work request, including argv, normalized
  pools, adapter and options, workspace, evidence, runtime, resolved pool
  credentials, and the structured brief hash.

Admission metadata is deliberately outside the work payload hash. That includes
the lookup key itself, priority, label, and orchestration fields such as
`maxNodes`, prompt/skill revision, and selection; `resultSchema` is also excluded
because it is a runner-side projection check. Editing any literal in the source
still changes `scriptHash` for an existing run, but changing invocation data does
not.

In particular, a `flowRunId` pins source bytes—not `args`, catalog bytes, or the
`--max-nodes` flag. Exact replay therefore requires the original invocation as an
operator invariant. If changed arguments retain the same key but alter canonical
work, the payload check catches them. If they derive a different author key, that
different key can create another row instead of comparing with the old ordinal.
Likewise, the daemon applies the `maxNodes` carried by each new submission, so a
larger flag at a later frontier can enlarge the run without payload divergence.

There is one sharp catalog consequence. Member-selection provenance—including
`catalogHash`, selected member ID, and roster—is orchestration metadata, so it is
also outside `payloadHash`; the run-level script-history scan pins only
`scriptHash`. A catalog change that alters adapter, pools, launch options, or
another canonical work field causes payload divergence. An ID-only or order-only
catalog change with otherwise identical work can instead reuse the old node.
Always replay a run with the exact original catalog bytes. A content-addressed
declarative catalog makes that discipline practical, but the runner does not
enforce it as a separate pin.

## The submission disposition table

Flow nodes always use full-mode admission. The result's `disposition` says how the
current invocation met the durable work; it is separate from the terminal
`verdict`.

| Outcome | What it means | Work performed now |
|---|---|---|
| `created` | No reusable or attachable work with the key and payload exists, or prior evidence was no longer reusable. | A new durable row is admitted and the runner awaits it. |
| `attached` | Exactly one live row has the same key and payload hash. | No duplicate row; await the same task and exact attempt. |
| `reused` | The governing terminal witness has the same payload, a passing verdict, and evidence that still probes successfully. | No node or lease; return the recorded terminal result and witness sequence. |
| `substituted` | A `drv()` node's declared outputs are already available or substitutable. | No build row or build lease; append a cheap substituted witness. |
| `terminal` | The governing witness has the same payload but a non-passing terminal verdict. | Do not retry it implicitly; return that recorded failure. |
| `dedup-key-conflict` | The key exists with a different payload, or the daemon cannot identify one unambiguous live candidate. | Reject admission. A same-run ordinal conflict is promoted to fatal `replay-divergence`. |
| evidence drift | A matching prior pass exists, but an artifact hash, declared digest, artifact availability, or store-path probe no longer matches. | Refuse reuse and return `created` with `reusedRejected`; execute fresh work. |

Artifact drift is not a sixth successful disposition. It is a disclosed reason for
creating new work. The daemon distinguishes `artifact-drift`,
`declared-hash-mismatch`, `artifact-unavailable`, `store-path-invalid`, and
`store-path-drift`. The current JavaScript `NodeResult` does not expose
`reusedRejected`, so a script cannot branch on that reason; it observes the fresh
`created` result. Operators can see the admission disclosure in the daemon's
protocol/conformance evidence.

A reused result retains its original verdict—normally `pass`—and original
`witnessSeq`; the word `reused` belongs to `disposition`. Likewise, `terminal`
returns the recorded failure verdict. Full mode never turns that failure into a
new attempt merely because the runner restarted.

## Replay from a killed runner

Suppose a runner is killed after three completed nodes while a fourth is still
running. Start the same script with the same `flowRunId`, arguments, catalog, and
configuration:

1. The runner scans existing nodes for the run and checks their one recorded
   script hash.
2. It executes the JavaScript from the top and assigns the same ordinals.
3. Completed passing nodes return `reused` results (subject to evidence probes).
   A completed failure returns `terminal`.
4. The still-live node returns `attached` and the runner awaits its exact attempt.
5. The first genuinely new frontier node is `created`, and execution continues.

No completed worker is re-inferred simply to reconstruct an in-memory value. The
value comes from the recorded terminal projection. For configured `finalMessage`
adapters, the live client joins that projection after the canonical terminal
acknowledgement and can rebuild it from retained adapter attestations after a
daemon restart. A required projection that does not appear within the bounded
join becomes `result-schema-mismatch` when the node declared a schema.

Prefix `log()` calls are suppressed when their following node is not `created`.
Tail logs are flushed without a following disposition and can repeat; logs are
diagnostics, not replay state.

## Daemon restart is a transport event

The runner uses one multiplexed daemon connection. On a broken connection, epoch
change, or restart-related await error, it replaces that connection and reissues
the idempotent query, submission, or `queue.await_job` call. Await includes the
attempt number. When a daemon-side automatic requeue has advanced the same task
UUID, a stale requested attempt follows the durable row's current attempt; a
future attempt remains an error rather than being silently rewritten.

The daemon recovers durable rows and reconciles live executor work. The shipped
multi-host VM check kills the coordinator daemon while a remote child is running,
adopts that child into the new lease epoch, and lets a replaying runner attach
without launching a second remote process. If both daemon and runner disappear,
restart the daemon first and then replay the runner as above.

## The observation-order law

Parallel workers may finish in any order. Flow JavaScript must not learn timing
from that race, so promise resolution follows terminal witness order:

1. node submissions cross admission in ascending ordinal order;
2. completed host futures wait in a set ordered by `(witnessSeq, ordinal)`;
3. the runner releases exactly one lowest-witness result at a time; and
4. that promise's continuation may materialize its next node before another ready
   result is released.

This is why `pipeline()` has no hidden stage barrier while remaining replayable.
An item that finishes stage one can submit stage two before a slower sibling
finishes stage one, but that progress follows witnessed observation order rather
than wall-clock promise polling. `Promise.all` still returns values in input order.

## Divergence is a safety feature

Two failures deliberately stop a run before it can create a new history.

### `script-changed-mid-run` — exit 20

Before evaluating the script, the runner queries every durable node with the
`flowRunId`. If the recorded `scriptHash` differs from the current source, it exits
20. The enqueue response repeats the same check to close the race between that
scan and a concurrent runner's first submission.

Restore the exact original script bytes and replay, or use a new `flowRunId` for
an intentional new run. Do not edit a mutable script path in place and reuse the
old run ID. Declarative flows avoid that trap because the script argument is a
content-addressed Nix store path.

### `replay-divergence` — exit 20

If a same-run ordinal or flow-local key re-derives a different `payloadHash`, the
runner reports both hashes, ordinal, and available labels, marks the replay error
fatal, and admits nothing past that point. Common causes are changed `args` that
retain the key while feeding a canonical work field, changed adapter or pool
configuration, changed resolved credentials, or deriving a spec from an
unwitnessed input.

Restore the original inputs and configuration, then replay. If the changed work is
intentional, start a new run identity. Changing a key merely to evade the check
would fork the history instead of explaining it.

A raw cross-run `dedupKey` collision with changed work normally remains
`FlowDedupKeyConflict`/`dedup-key-conflict` and exits 1. It becomes replay
divergence only when the conflicting candidate identifies the same flow run and
ordinal. This distinction is why raw keys should be rare and domain-specific.

## Predicting common failures

| Event | Observable outcome |
|---|---|
| Runner killed | Re-execute from the top with the same identity; completed nodes reuse, a live node attaches, and only the frontier creates work. |
| Runner exceeds `MemoryMax` and is OOM-killed | Treat it as a killed runner, not a catchable JavaScript error. Re-execute from the top with the same identity; admitted children remain durable and replay reuses or attaches them. |
| Daemon restarted while runner waits | The client reconnects and re-awaits the exact attempt; recovered/adopted work supplies the terminal result. |
| Script edited after any node exists | `script-changed-mid-run`, exit 20, before new admission. |
| Same key, changed payload | Same-run identity: fatal `replay-divergence`, exit 20. Raw cross-run identity: `dedup-key-conflict`, exit 1. |
| Arguments changed so an author-derived key also changes | The new key can create a second history at that ordinal; arguments are not independently pinned. Replay with the original arguments. |
| `--max-nodes` increased on replay | The cap is orchestration metadata, not payload identity; a later new frontier can use the larger cap. |
| Catalog changed, but selected work payload stayed byte-identical | Selection provenance is not payload identity; the prior node can reuse. Restore the original catalog before replaying. |
| Prior artifact changed or vanished | Reuse is rejected with a drift reason and a fresh node is `created`. |
| Prerequisite has a non-pass verdict | Default `await` rejects `terminal-failure`, exit 1, so dependent code is not run. Node settle mode returns the failed `NodeResult` for an explicit decision. |
| Script syntax, determinism, loop, microtask-budget, wall-clock-budget, or runtime-limit failure | Structured script failure, exit 10. Already admitted children remain durable and are handled on the next replay. |
| Missing or malformed runner identity | Startup failure, exit 2. |

Boa does not impose a separate JavaScript heap quota. When the flow runner is
itself a tally job, including a declaratively rendered runner, daemon execution
gives its process the finite `--memory-max-bytes`/systemd `MemoryMax` limit. An
ad-hoc `tally flow run` launched outside the daemon instead inherits the
operator's process limits and should likewise run under a finite memory limit.
Crossing that boundary terminates the runner process, so no `FlowError` can be
emitted from that process; the durable ledger and same-identity replay are the
recovery mechanism.

## The author rule

A node specification may be built only from `args`, literals, `meta`/`flowMeta`,
and prior witnessed results. Do not use a clock, random value, environment lookup,
filesystem scan, network response, promise completion race, or mutable global
process state. Put such discovery in a node, witness a compact result, and derive
later work from that result.

This rule is not stylistic advice. It is what makes a killed runner able to prove
that its next payload is the same payload—or stop honestly when it is not.
