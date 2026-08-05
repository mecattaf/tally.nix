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

Every flow node records four hashes:

- `scriptHash` is SHA-256 over the exact flow source bytes and is stamped on every
  node in the run.
- `argsHash` is SHA-256 over the runner's compact JSON serialization of the
  parsed arguments. Object member order is preserved; insignificant input
  whitespace is not.
- `catalogHash` is SHA-256 over the exact catalog file bytes, or `null` when the
  run has no catalog.
- `payloadHash` covers the canonical work request, including argv, normalized
  pools, adapter and options, workspace, evidence, runtime, resolved pool
  credentials, and the structured brief hash.

Admission metadata is deliberately outside the work payload hash. That includes
the lookup key itself, priority, label, human `taskRef`, and orchestration fields
such as `scriptHash`, `argsHash`, `catalogHash`, `maxNodes`, prompt/skill revision,
and selection; `resultSchema` is also excluded because it is a runner-side
projection check. The first three orchestration hashes are nevertheless run identity: every
later invocation of the same `flowRunId` must match them before work can reuse,
attach, or be created. The startup history scan and the admission response both
enforce this, closing the race between two concurrent runners.

`--max-nodes` remains outside that run identity. The daemon applies the
`maxNodes` carried by each new submission, so a larger flag at a later frontier
can enlarge the run without payload divergence. Prompt/skill revisions and
selection provenance remain node provenance rather than separate run pins.

Catalog selection provenance—including selected member ID and roster—also stays
outside `payloadHash`. The separate run-level `catalogHash` means that any catalog
byte change now stops replay as `catalog-changed-mid-run`, even when selected
execution data and the resulting work payload would be identical. Declarative
catalogs make the required bytes content-addressed, while manual invocations get
the same fail-closed check.

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

Whichever way a node is met, the run that submitted it becomes a durable member
of the task it was handed: the admission appends `(flowRunId, taskUuid)` to
`<dataDir>/flow-membership.jsonl` and fsyncs it before answering. That matters
most for `attached`, `reused`, and `terminal`, which write no row of their own —
the row belongs to whichever run created it, so without the membership record
the submitting run could not see its own node in `tally query log --flow-run`,
`query jobs --flow-run`, or `query run`. A `dedup-key-conflict` admits nothing
and therefore joins the run to nothing. See [Run membership is a durable
admission
fact](../operating/observability.md#run-membership-is-a-durable-admission-fact).

## Replay from a killed runner

Suppose a runner is killed after three completed nodes while a fourth is still
running. Start the same script with the same `flowRunId`, arguments, catalog, and
configuration:

1. The runner scans existing nodes for the run and checks the recorded script,
   arguments, and catalog identity hashes.
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

## Continuation after budget exhaustion

`FlowRuntimeBudgetError`/`wall-clock-budget` (exit 10) ends one JavaScript
evaluation after 24 hours; it does not cancel or delete the child jobs that
evaluation admitted. Treat this error as a continuation point. The JavaScript
heap and pending promises are gone, but every admitted job and terminal witness
remains durable. Re-executing the same flow-run identity rebuilds the heap from
those witnessed results and continues at the first work that is not already
durable.

Wait for the runner to become terminal, then use the invocation that matches how
it was started:

- For a declaratively registered flow, retry the failed runner job in place:

  ```console
  $ tally queue retry <runner-task-uuid>
  ```

  The retry keeps the runner task UUID, which is also its `flowRunId`, increments
  the runner attempt, and preserves the original direct argv, runner pools, and
  `workloadMutex`. Re-firing the calendar producer is not a retry: it may
  deduplicate against the failed runner, and any newly admitted runner would get
  a new task UUID and therefore name a new flow run.
- For a manual run, repeat the original command with the same run ID, exact
  script bytes, arguments, catalog bytes, and effective configuration:

  ```console
  $ tally flow run /nix/store/…-campaign.js \
      --flow-run-id 4f8608e1-608f-4e04-bf47-0e49fd9801f1 \
      --args '{"repository":"mecattaf/example"}' \
      --max-nodes 1000 \
      --catalog /nix/store/…-catalog.json
  ```

  Omit `--catalog` only when the original run had no catalog. A changed script,
  argument, or catalog identity stops before new admission; a changed node
  payload stops at `replay-divergence`.

At the old frontier, the daemon resolves what happened while the evaluator was
ending:

- a witnessed pass answers `reused` (unless its evidence has drifted, in which
  case fresh work is `created`);
- an admitted job still running answers `attached` and the new evaluator awaits
  that exact task and attempt;
- a witnessed non-pass result answers `terminal`; and
- a host call that never became durable is safely admitted as `created` under
  the same deterministic key and payload.

The lifecycle stream makes the transition visible. In this abridged JSONL, the
completed prefix reuses, the live frontier attaches, and only the next node
creates work:

```json
{"type":"node-submitted","flowRunId":"4f8608e1-608f-4e04-bf47-0e49fd9801f1","ordinal":0,"disposition":"reused"}
{"type":"node-terminal","flowRunId":"4f8608e1-608f-4e04-bf47-0e49fd9801f1","ordinal":0,"disposition":"reused"}
{"type":"node-submitted","flowRunId":"4f8608e1-608f-4e04-bf47-0e49fd9801f1","ordinal":1,"disposition":"attached"}
{"type":"node-terminal","flowRunId":"4f8608e1-608f-4e04-bf47-0e49fd9801f1","ordinal":1,"disposition":"attached"}
{"type":"node-submitted","flowRunId":"4f8608e1-608f-4e04-bf47-0e49fd9801f1","ordinal":2,"disposition":"created"}
```

Node lifecycle events are never suppressed. `log()` is different: a queued log
is emitted only when its following node answers `created`. Logs before `reused`
prefix nodes, and before an `attached` frontier node, are therefore suppressed on
continuation. A log after the last node has no following disposition, so it is
flushed and may appear once per evaluation.

The 24-hour evaluator budget deliberately remains fixed; it is not configurable
through `meta` or `services.tally.flows.<name>`. Raising or removing it per
registration would let a hung awaited node or transport hold an evaluator, its
`flow` slot, and any process-scoped `workloadMutex` indefinitely. Flows that
need run-identity continuity use witnessed replay as their checkpoint mechanism.
Forge-backed spec-build campaigns instead run bounded, fresh reconcile passes;
their continuation state is the set of marked merged pull requests and
content-and-exact-base-bound automated checkpoint refs, plus authenticated
diagnosis and escalation comments on the campaign issue. Node
`runtimeMaxSec`, the runner job's registration-level `runtimeMaxSec`, and the
RPC call deadline remain separate bounds; changing one does not change the
24-hour evaluation budget.

## Daemon restart is a transport event

The runner uses one multiplexed daemon connection. On a broken connection, epoch
change, or restart-related await error, it replaces that connection and reissues
the idempotent query, submission, or `queue.await_job` call. Await includes the
attempt number. When a daemon-side automatic requeue has advanced the same task
UUID, a stale requested attempt follows the durable row's current attempt; a
future attempt remains an error rather than being silently rewritten.

The first reconnect attempt is immediate. Continued failures back off exponentially from
50 milliseconds to a 2-second cap, and the runner emits one `flow-rpc-reconnect` lifecycle
line when each call first enters reconnect mode. A live RPC call has a generous 24-hour total
deadline by default; `flow run --rpc-call-deadline-sec SECONDS` selects a shorter or longer
positive bound when an operator needs one.

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

Run-identity and payload failures deliberately stop a run before it can create a
new history.

### One `details` shape for every exit-20 refusal

The five codes below are one family: `script-changed-mid-run`,
`args-changed-mid-run`, `catalog-changed-mid-run`, `flow-run-superseded`, and
`replay-divergence`. Each is raised either by the runner's startup scan, before
the script is evaluated, or by an admission mid-run — and a driver must not have
to know which. All five therefore carry the same fourteen `details` members at
every raising site, with `null` where the code has nothing to say:

| Field | Meaning |
|---|---|
| `flowRunId` | The run whose recorded identity is in question. |
| `divergentInput` | `script`, `args`, `catalog`, or `payload`. `null` for `flow-run-superseded`, where nothing diverged. |
| `recordedHash` | The hash the ledger recorded for that input. |
| `currentHash` | The hash this runner computed for the same input, now. |
| `recordedLabel` | The node label the ledger recorded. |
| `currentLabel` | The node label this runner derived, now. |
| `taskUuid` | The durable row the refusal is about, where one is identified. |
| `successorFlowRunId` | The run that replaces a retired one. |
| `reason` | The recorded rollover reason, from `flow.supersede`'s closed set. |
| `recordedAt` | When the rollover was recorded. |
| `kernelError` | The daemon's own message, when the refusal was found through a kernel dedup-key conflict rather than by the runner's own comparison. |
| `remedy` | The `tally flow supersede` invocation that clears it, or `null` when no single command does. |
| `transient` | Always `false` for this family. |
| `resolution` | `supersede`, `run-successor`, or `investigate`. |

So `details.recordedHash` and `details.currentHash` name the two sides of the
disagreement for all five codes, whichever one fired and wherever it fired. The
[error reference](../reference/errors.md#branching-on-a-failure-without-reading-prose)
lists which members each code populates.

Two members of the family have only one raising site, because the state machine
allows only one, and that is a decision rather than a gap:

- `flow-run-superseded` is startup-only. Lineage is read once by the same
  `inspect_run` scan as the three pins; admission never re-reads it. See
  [the scope of the refusal](#the-scope-of-the-refusal).
- `replay-divergence` is mid-run only. It is a statement about one ordinal's
  payload, and at startup no ordinal has been derived yet.

`ordinal` is a top-level field of the error, not part of `details`. It is present
exactly when a node is implicated, so a startup refusal has none.

### `script-changed-mid-run` — exit 20

Before evaluating the script, the runner queries every durable node with the
`flowRunId`. If the recorded `scriptHash` differs from the current source, it exits
20. The enqueue response repeats the same check to close the race between that
scan and a concurrent runner's first submission.

Restore the exact original script bytes and replay, or use a new `flowRunId` for
an intentional new run. Do not edit a mutable script path in place and reuse the
old run ID. Declarative flows avoid that trap because the script argument is a
content-addressed Nix store path.

### `args-changed-mid-run` — exit 20

The runner hashes parsed `args` before evaluating the script. If an existing node
for the run has a different `argsHash`, it exits 20 before deriving or admitting
another node. The admission response repeats the comparison for concurrent
runners. This check does not depend on which key or payload the arguments would
have produced.

Replay with arguments that serialize to the recorded identity, or start a new
`flowRunId` for intentional argument changes.

### `catalog-changed-mid-run` — exit 20

The runner similarly pins the exact catalog bytes, including the distinction
between a catalog and no catalog. A different `catalogHash`, adding a catalog, or
removing one exits 20 before new admission. Even whitespace-only catalog edits
change this identity.

Replay with the exact original catalog bytes, or start a new `flowRunId` for a
new catalog generation.

### `flow-run-superseded` — exit 20

A durable rollover already retired this run ID. The runner checks lineage before it compares any
hash, so this answer outranks the three pins above: the run was abandoned by an explicit decision,
and repeating which input moved would not help. The error names `successorFlowRunId`, the recorded
`reason`, and `recordedAt`.

Start the successor. Do not re-key the old run.

## Superseding a terminal run

The three identity pins fail closed, which is correct — but a refusal alone is not a recovery. A
long-lived supervisor that persists one `flowRunId` per work item and retries it after every
deployment can only ever re-observe the same refusal, and three such items adjacent in a worklist
can starve everything behind them.

`tally flow supersede` is the explicit, durable transition:

```console
$ tally flow supersede \
    --flow-run-id 4f8608e1-608f-4e04-bf47-0e49fd9801f1 \
    --new-flow-run-id 9a2c1f70-3d5e-4a11-9f2b-8c6e0b7d4413 \
    --reason generation-change
```

What it does, and equally what it does not:

- The old run is **preserved unchanged** — same rows, same witnesses, same history. Superseding
  is a statement about the run, not an edit of it.
- The predecessor/successor relationship and the reason become durable in
  `<dataDir>/flow-lineage.jsonl`, together with the abandoned generation's own recorded script,
  argument, and catalog hashes, read from its rows rather than from the caller.
- Repeating the **identical** call is safe. It answers `disposition: "reused"` and writes nothing,
  so a supervisor may call it again after its own restart. Idempotency is keyed on the whole
  `(flowRunId, successorFlowRunId, reason)` triple, so mint the successor UUID once and persist it
  *before* calling; a fresh UUID per attempt is a `flow-lineage-conflict`, not a retry.
- Replaying the superseded ID is refused with `flow-run-superseded`, which names the successor.
- The successor is **not** created and inherits nothing. It starts as a fresh run, which is why a
  run that already has nodes is refused as a successor. Reusing application-level artifacts or
  checkpoints across the boundary remains the consumer's own concern, exactly as it was before.

Reasons are a closed set: `generation-change` (a declarative activation moved the script or
argument store paths), `script-changed`, `args-changed`, `catalog-changed`, and `operator`.

Read the boundary back from either end:

```console
$ tally query lineage 4f8608e1-608f-4e04-bf47-0e49fd9801f1
$ tally query run 4f8608e1-608f-4e04-bf47-0e49fd9801f1
```

`query lineage` reports `superseded`, `supersededBy`, `supersedes`, the whole `chain`
oldest-first, and `currentFlowRunId` — the run that should actually be started. `query run`
reports `state: superseded` for a retired run whatever its own node verdicts say, and names the
successor above the task board.

A contradiction is refused rather than rewritten: a second different successor, a successor that
already succeeds another run, a rollover that would close a cycle, or a predecessor whose own rows
disagree about a pinned hash all fail with `flow-lineage-conflict`. A predecessor with unfinished
nodes is refused too — cancel the run first, so that a rollover can never strand live work.

**A rollover must name a run that exists.** A predecessor with no durable node, or with no
recorded `orchestration.scriptHash`, is refused as `not_found`. Such a run can never trip a
startup identity pin, so it can never need retiring, and refusing it is what catches a typo'd or
mis-pasted run ID — the alternative is a rollover that reports success while recovering nothing
for the run the supervisor is actually replaying. It also means the recorded predecessor hashes
are never silently omitted.

**Every valid rendering of a run ID names one run.** A UUID may be written hyphenated, bare,
braced, upper case, or lower case. All of those are canonicalized to hyphenated lowercase on the
way into the ledger and on every lookup, so a rollover recorded from one spelling is found by the
runner presenting another. Records written by an earlier tally in a different rendering are
absorbed by the same canonicalization when the ledger is read; nothing needs migrating.

### The scope of the refusal

This prevention is deliberately narrow, and building automation on it means knowing where it
stops:

- It lives in the **flow runner's startup**, in the same `inspect_run` scan as the three identity
  pins. It is evaluated once, before the script is evaluated.
- It is therefore **not** an admission-time check. A rollover recorded while a run is already in
  flight does not stop that runner from admitting its remaining nodes under the retired ID; the
  refusal applies to the *next* start. Cancel the run first if you need it stopped now — which is
  also what `flow.supersede` requires of a predecessor with unfinished nodes.
- Any client that is not this runner — a direct `queue.enqueue`, an older binary — can still
  enqueue work carrying a retired `flowRunId`. `tally flow run` is the only thing that mints
  flow-run-scoped nodes in practice, which is why the runner-side check is sufficient in the
  shape the incident actually takes, but it is not a kernel-level prohibition and is not
  described as one.

### When the lineage index itself is damaged

Every flow start reads `<dataDir>/flow-lineage.jsonl`, so its integrity matters beyond the runs
it names. Two failure modes, treated differently on purpose:

- **An interrupted append** — a crash, a power loss, or a short write under ENOSPC — leaves an
  unterminated final line. That is ignored on read and truncated by the next write, exactly as the
  attestation chain repairs its own torn tail. No run is blocked.
- **A complete record that cannot be decoded or validated** — a hand edit, or bit rot — fails
  closed with `flow-lineage-unusable`, and that blocks every flow start until it is repaired. The
  alternative, skipping the bad line, could resurrect a run an operator durably retired, which is
  the one outcome this store exists to prevent. The failure carries `transient: false` and
  `resolution: "repair-lineage-ledger"` so a supervisor escalates instead of retrying it all
  night. Repair is removing one line from a plain JSONL file with the daemon stopped; it is an
  index, not a hash chain, so nothing downstream needs re-verifying.

The ledger keeps its newest 100,000 records; see the
[retention inventory](../operating/retention.md#what-still-grows).

### `replay-divergence` — exit 20

If a same-run ordinal or flow-local key re-derives a different `payloadHash`, the
runner reports both hashes, ordinal, and available labels, marks the replay error
fatal, and admits nothing past that point. Common causes are changed adapter or
pool configuration, changed resolved credentials, or deriving a spec from an
unwitnessed input. Script, argument, and catalog changes are rejected earlier by
their dedicated run-identity pins.

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
| Arguments changed after any node exists | `args-changed-mid-run`, exit 20, before new admission, regardless of the key they would derive. |
| Catalog bytes changed, added, or removed after any node exists | `catalog-changed-mid-run`, exit 20, before new admission, even when the selected work payload would be identical. |
| Run ID replayed after `tally flow supersede` retired it | `flow-run-superseded`, exit 20, before any hash comparison, naming the successor to start instead. |
| Same key, changed payload | Same-run identity: fatal `replay-divergence`, exit 20. Raw cross-run identity: `dedup-key-conflict`, exit 1. |
| `--max-nodes` increased on replay | The cap is orchestration metadata, not payload identity; a later new frontier can use the larger cap. |
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

## What silently changes your payload hash

Two payloads are the same work only if their canonical bytes match. Most of the
normalization the host performs makes *cosmetic* differences invisible — but one
thing an author reasonably assumes is normalized is not, and it is the one that
bites.

**Evidence order is not canonicalized.** The host validates and rewrites each
evidence spec individually, and keeps the array in the order the script wrote it.
Reordering an evidence array between runs of the same flow run is a different
payload and therefore `replay-divergence`. Build the array in a fixed order — a
literal, or a sorted projection of a witnessed result — not by appending in
whatever order the script happens to discover requirements.

| Input | Normalization before hashing |
|---|---|
| `evidence` array order | **None.** Order is preserved exactly as written, so reordering diverges. |
| Each `evidence` entry | `hash:sha256:<digest>` lowercases the digest; `exit:<code>` is re-rendered from the parsed integer; `artifact:` and `store:` are kept verbatim. |
| `pools` | Sorted before hashing and submission, so declaration order is free. |
| `env` | Relocated into `adapterOptions.environment` and key-sorted, so insertion order is free. A name set in both `env` and `adapterOptions.environment` is `duplicate-environment`, not a silent overwrite. |
| `approvalPolicy`, `sandboxPolicy` | Relocated into the corresponding `adapterOptions` members. Their names remain exact payload inputs because the selected adapter maps them to authorized argv. |
| Pool credentials | Resolved by walking the sorted pool list and taking the first path for each credential name, so the resolved set is a function of the pool set alone. |
| `drv` outputs | Sorted by output name, and the derived `store:` evidence is sorted and de-duplicated. |
| `prompt` | Normalized into the structured brief and hashed as `briefHash`; the prompt text is not in the payload. |

`args` and the catalog are pinned per run rather than hashed into the node
payload: they are checked once at startup, and a change is
`args-changed-mid-run` or `catalog-changed-mid-run` rather than a per-node
divergence.

## A flow cannot choose a model

There is no surface through which a script sets `model` or `effort` on a
`claude()` or `codex()` node. `adapterOptions` is not among the fields either the
`job()` or the sugar surface accepts, so writing it is
`FlowSpecError`/`unknown-spec-field`. A script may set the narrower top-level
`approvalPolicy` and `sandboxPolicy` fields; the host normalizes them into the
private envelope and the adapter still rejects names absent from its closed
policy maps. `local()` instead copies its complete launch object verbatim from
the selected catalog member — where it is operator-declared, hashed into the
catalog, and pinned for the run.

This is deliberate. A model choice that lived in the script would be a payload
input the operator never declared and the catalog hash never covered.

## Redeploying while a run is in flight

A declaratively registered flow's script is a store path, and the durable rows of
an in-flight run reference the generation that admitted them. Two things follow.

**The old script can be garbage-collected out from under a live run.** While the
old generation is still a profile generation its closure is rooted, so the script
path survives an ordinary `nix-collect-garbage`. Deleting the generation itself —
`nix-collect-garbage -d`, or `--delete-older-than` past it — drops that root. The
daemon registers GC roots for witnessed store paths and `drv` outputs, not for a
flow's script, so nothing else holds it. A run re-executed after that deletion
cannot read its script. Keep the generation until the run is finished, or accept
that the run ends and start a new run identity against the new script.

**Editing the script does not silently continue the run.** If the run is
re-executed with the same run ID against a changed script, the first thing the
runner does is compare hashes and stop with `script-changed-mid-run`, exit 20.
That is the intended outcome: the alternative is a run whose first half and
second half came from different programs.

**A supervisor recovers from that by superseding, not by retrying.** After a
declarative activation the old generation may no longer be operationally
available, so "restore the exact old inputs" can be impossible. Record the
rollover instead — `tally flow supersede --reason generation-change` — and run
the successor. The transition is durable, idempotent, and auditable from both
ends; see [Superseding a terminal run](#superseding-a-terminal-run).

**The next timer firing belongs to the new generation.** A calendar-registered
flow derives its runner's `dedupKey` from an strftime template, so the identity
of the *next* firing is a function of the clock and the template, not of the
deployed script. After a redeploy, the next firing is admitted under the new
generation and runs the new script; an unfinished run from the old generation
does not adopt it and is not resumed by it. If the template's resolution is
coarser than the redeploy interval — a monthly key redeployed twice in a month —
the second firing deduplicates against the first and does not execute again.
Change the template, not the script, when you want a redeploy to produce a
distinct run.

## The author rule

A node specification may be built only from `args`, literals, `meta`/`flowMeta`,
and prior witnessed results. Do not use a clock, random value, environment lookup,
filesystem scan, network response, promise completion race, or mutable global
process state. Put such discovery in a node, witness a compact result, and derive
later work from that result.

This rule is not stylistic advice. It is what makes a killed runner able to prove
that its next payload is the same payload—or stop honestly when it is not.
