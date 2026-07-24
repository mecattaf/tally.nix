# Transfer brief: Inngest (inngest-js SDK) + Cloudflare Workflows

Sources:
- Clone: `github.com/inngest/inngest-js`, shallow (`--depth 1`), fetched 2026-07-24. All
  paths below are relative to `~/Downloads/inngest-js/packages/inngest/src/` unless noted.
  Head commit at clone time not pinned by this brief — re-clone will drift line numbers;
  treat line refs as "as of 2026-07-24" pointers, not permanent anchors.
- Docs: `developers.cloudflare.com/workflows/*` (Workers API, rules-of-workflows,
  sleeping-and-retrying, trigger-workflows pages) and `inngest.com/docs/*` (learn/versioning,
  guides/step-parallelism, features/inngest-functions/error-retries/retries,
  guides/singleton), fetched via WebFetch same date. Quotes below are as extracted by
  WebFetch's summarizer, not manually diffed against raw HTML — flagged where that matters.

Role in tally-flow: these are the two closest production analogs to `job()` call-site
keying and attach-vs-duplicate semantics. Neither system turns out to implement true
"attach to an in-flight duplicate" — see §4 and §5. That absence is the single most
load-bearing finding in this brief.

---

## 1. How Inngest derives step identity

**Mechanism**: user supplies a string ID (or `{id, ...opts}`) to `step.run(id, fn)` /
`step.sleep(id, ...)` / etc. The SDK hashes that string with SHA-1 to get the identifier
actually used as the memoization key on the wire and in state.

- `hashId`, `components/execution/engine.ts:3237-3239`:
  ```ts
  const hashId = (id: string): string => {
    return sha1().update(id).digest("hex");
  };
  ```
  (`sha1` destructured from `hash.js` at `engine.ts:107`; `import hashjs from "hash.js"` at
  `engine.ts:2`.)
- `hashOp` (`engine.ts:3241-3245`) rehashes an `OutgoingOp`'s `id` field the same way.
- The raw, un-hashed user string is preserved separately as `opId.userland.id` /
  `stepInfo.options.id` for display and for collision comparison; only the hash is the
  storage/wire key (`this.state.steps` is keyed by hashed id throughout `engine.ts`, e.g.
  `2277-2280`, `2624`).

**Duplicate IDs in one run**: handled by `resolveStepIdCollision`
(`engine.ts:3269-3309`). Given a `baseId`, it:
1. Hashes `baseId`. If neither `stepsMap` (steps already added to state) nor
   `expectedIndexes` (base IDs claimed by concurrent-but-not-yet-registered handlers in the
   same tick) has it, no collision — claim it and return `baseId` unchanged.
2. On collision, appends an indexing suffix (`STEP_INDEXING_SUFFIX = ":"`, defined at
   `components/InngestStepTools.ts:227`) and an incrementing integer — `baseId + ":" + i` —
   probing `i` upward from the last claimed index until an unused hash is found
   (`engine.ts:3299-3309`). Throws `UnreachableError` if no free slot is found within
   `stepsMap.size + 1` attempts (`engine.ts:3305-3308`), which the comment marks as
   theoretically unreachable given the probe bound.
3. This collision path is re-run a second time after middleware (`transformStepInput`) has
   a chance to change `stepInfo.options.id` (`engine.ts:2828-2851`, "3. If middleware
   changed the step ID, re-resolve collisions" / "Recompute hashedId with final ID").

So Inngest does **not** error on a duplicate literal ID inside one run — it silently
rewrites the effective ID to `id:2`, `id:3`, ... in call order, and only *warns* (does not
block) when it additionally detects the duplicate spans two different parallel chains: see
`maybeWarnOfParallelIndexing` (`engine.ts:2270-2295`), which fires
`ErrCode.AUTOMATIC_PARALLEL_INDEXING` with message `Duplicate step ID "<id>" detected across
parallel chains` / explanation `"Using the same ID for steps in different parallel chains
can cause unexpected behaviour. Your function is still running."` (`engine.ts:2288-2291`).
The warning is purely diagnostic; execution proceeds with the auto-indexed ID regardless.

Separately, **lazy ops** (fire-and-forget opcodes like `DeferAdd`, bypassing `state.steps`
entirely — see `components/execution/ARCHITECTURE.md:5-15`) enforce uniqueness more
strictly: a duplicate lazy-op ID within a run is *skipped* (not auto-indexed) with a logged
warning `"defer skipped: duplicate ID within run"` (`engine.ts:2374-2382`), because — per the
inline comment — "shipping two ops with identical hashed ids relies on backend dedup and
hides the mistake."

**Code changing between replays** — this is not handled in the JS SDK's engine at all; it's
a docs-level contract, not an enforced invariant in this code. From
`inngest.com/docs/learn/versioning` (WebFetch summary, quotes as extracted):
- Adding a step: "New steps are executed when discovered" in an in-progress run, with the
  caveat "New steps must not depend on data from steps that haven't executed yet."
- Modifying a step but keeping its ID: "in-progress runs that have already completed that
  step will use the memoized result" (i.e. new step body is ignored for already-completed
  calls — classic memoize-by-key-not-by-content).
- Changing a step's ID: "forces re-execution" for in-progress runs, even ones that had
  already run the old-ID version.
- Removing a step: "in-progress runs that have already completed it will continue normally"
  — the orphaned memoized data is simply never read again.
- Reordering steps: "triggers a warning because the execution order differs" from stored
  state, but is still graceful — "memoized steps return their stored results regardless of
  their position." The docs frame this overall posture as "graceful determinism by
  default," explicitly not strict/enforced determinism.
- ID-naming guidance: choose IDs that are "Descriptive," "Stable," and "Unique"; avoid IDs
  that "encode values that might change."

I did not find code in this clone that detects a reordering and emits the warning above —
it may live in the Go executor (server-side), not in inngest-js. Flagging as **unknown /
out of scope of this clone** rather than guessing.

---

## 2. Replay semantics

**Memoization lookup on replay** happens in `applyMiddlewareToStep`
(`engine.ts:2786-2890`), step 4, "Final memoization lookup with potentially modified
hashedId": it reads `this.state.stepState[hashedId]`. `state.stepState` is populated at
run start from the op stack the executor sends back with each invocation (i.e. results of
previously-run steps arrive as part of the *request payload*, not fetched separately) —
`hasStepState` on the `FoundStep` type (`components/InngestStepTools.ts:94-118`) tracks
"whether the step has been given some state from the Executor," distinct from `fulfilled`
(whether the in-process Promise has resolved/rejected).

- If `stepState` exists and `typeof stepState.input === "undefined"`, the step is
  `isFulfilled = true` — a completed step's result becomes the resolved handler; the user's
  step callback (`fn`) is never invoked for it (`engine.ts:2855-2869`).
- If `stepState.input` *is* defined (an array), that's a special case for how retries seed
  arguments (`engine.ts:2460-2478`) — not a "still running" signal in the JS SDK's own
  model; see below on where "in flight" is actually detected.
- Book-keeping: `this.state.remainingStepsToBeSeen` (a `Set<string>` of hashed IDs,
  seeded from `this.options.stepCompletionOrder`, `engine.ts:2125`) has the memoized step's
  hash deleted once seen (`engine.ts:2861`); `allStateUsed()` (`engine.ts:2131-2133`) is
  `remainingStepsToBeSeen.size === 0`, used to fire `onMemoizationEnd` middleware hook once
  all previously-known state has been consumed by re-execution up to the current point
  (`engine.ts:1805`, `2863`).

**Where memoized results live**: entirely in the request/response payload between the
user's server and the Inngest executor — `this.state.stepState` is populated per-invocation
from `this.options` (constructed from the incoming request), there is no local disk/DB in
the JS SDK. The SDK is stateless between HTTP invocations; the executor (not in this clone)
is the durable store of record.

**In-flight steps** (a step dispatched to the executor whose result hasn't come back yet) —
the JS SDK's own concept of "duplicate/racing invocation" is **not** an attach mechanism; it
is a **fencing** mechanism at the transport layer:
- `StaleDispatchError` (`api/api.ts:32-38`) is thrown when the checkpoint-async endpoint
  returns HTTP 409 (`api/api.ts:588-593`): `"Stale dispatch: checkpoint returned 409 (run
  <runId>)"`. The inline comment at `api.ts:585-587` explains why: *"409 means the executor
  has already requeued. Halt rather than returning buffered ops, which would let the
  executor memoize them as canonical and chain the next dispatch off this dead
  invocation."* (references internal ticket `EXE-1552`).
- On catching a stale dispatch in `attemptCheckpointAndResume` (`engine.ts:948-960`), the
  SDK does not retry or attach — it returns `{ type: "function-rejected", ..., retriable:
  false }`, i.e. this invocation gives up entirely and lets whichever invocation the
  executor considers canonical continue.
- `shouldRetry: (err) => !isStaleDispatchError(err)` (`engine.ts:520`) — the SDK's own retry
  wrapper explicitly excludes stale-dispatch from its retry policy, because retrying it
  would just re-race the same losing invocation.

So: Inngest's answer to "two processes racing on the same run" is *server-assigned
canonical winner via a generation/queue-item fence, loser self-aborts* — never "second
caller attaches to the first caller's in-flight promise" within the JS SDK. The attach-like
behavior, if any exists, is implemented in the (closed-source, not in this clone) Go
executor's queueing layer, not in inngest-js. Marking that as **unknown**.

**Error/retry semantics per step**:
- `NonRetriableError` (`components/NonRetriableError.ts:10-35`) is a plain `Error` subclass
  with `name = "NonRetriableError"` and an optional `cause`; thrown inside a step body to
  signal "cease all execution and not retry" (class doc comment, lines 1-8).
- Docs (`inngest.com/docs/features/inngest-functions/error-retries/retries`, WebFetch
  extraction): "Each `step.run()` has its own independent retry counter" — default 4 retries
  (5 attempts total); throwing `NonRetriableError` from a step or function "bypass[es] any
  remaining retries and fail[s]" that step/function; the `attempt` argument passed into
  function context is "zero-indexed... incremented every time the function throws an error
  and is retried, and is reset when steps complete."
- A step's failure is itself memoized: `StepError` (`components/StepError.ts:13-36`) wraps
  a failed step's `stepId` plus a `jsonErrorSchema`-parsed error (name/message/stack/cause)
  so that a *previously failed and now-exhausted* step replays as a rejection without
  re-running — confirmed by the `handling-step-errors` test fixture
  (`test/functions/handling-step-errors/`), whose expected timeline output for a
  permanently-failed step ("a") is the structured `{ error: { name, message, stack, cause }
  }` shape, i.e. the failure itself is the durable, replayable artifact.

**Parallelism via `Promise.all`, out-of-order completion**: the intended pattern (per docs
`guides/step-parallelism` and the `promise-all` test fixture,
`test/functions/promise-all/handler.ts`) is:
```ts
const [one, two] = await Promise.all([
  step.run("Step 1", () => 1),
  step.run("Step 2", () => 2),
]);
return step.run("Step 3", () => one + two);
```
Mechanically, each `step.run` call in the same synchronous tick is registered via
`pushStepToReport` (`engine.ts:2348-2352`), batched into `foundStepsToReport` /
`unhandledFoundStepsToReport` maps keyed by `hashedId`, and flushed together by
`reportNextTick` (`engine.ts:2298-2345`) as a single `steps-found` batch — so steps declared
in the same `Promise.all` are discovered and dispatched together regardless of the order
their handlers eventually settle in.

Completion order is reconciled against a server-supplied ordering,
`this.options.stepCompletionOrder` (seeds `remainingStepCompletionOrder`,
`remainingStepsToBeSeen`, `engine.ts:2125`). `reportNextTick`'s inner loop
(`engine.ts:2320-2331`) walks `remainingStepCompletionOrder` and, for each ID in that
server-declared order, calls `.handle()` on the matching unhandled step *if already found*
— i.e. the SDK replays found-step reporting in the order the server says steps actually
completed, not the order `Promise.all`'s callbacks fire in JS's microtask queue. Docs
summary: "When each step is finished, Inngest will aggregate each step's state and
re-invoke the function with all state available," and "Sequential steps in parallel groups
may not execute in the order you expect."

---

## 3. Cloudflare Workflows

All quotes below are WebFetch extractions from `developers.cloudflare.com/workflows/build/`
{`rules-of-workflows`, `sleeping-and-retrying`, `workers-api`, `trigger-workflows`} pages,
fetched 2026-07-24. I did not fetch raw HTML to hand-verify wording; treat quotes as
faithful-but-summarized.

**Step naming rules**: "Steps should be named deterministically (that is, not using the
current date/time, randomness, etc.)." Dynamic names are allowed only "constructed in a
deterministic way" — derived from stable prior step outputs traversed predictably, not from
random shuffling. `step.do(name, config?, callback, rollbackOptions?)` — `name` is capped at
"up to 256 characters." Within a step's context, `ctx.step.count` (as accessed inside
`WorkflowStepContext`, per the workers-api page) gives "how many times `step.do` has been
called with this name in the current Workflow run (1-indexed)" — i.e. Cloudflare's
duplicate-name handling is an explicit visible counter the user can read, not a hidden
auto-suffix the way Inngest's `:2`/`:3` indexing is.

**Determinism rules / forbidden outside a step**:
- "do not store state outside of a step" — "Workflows may hibernate and lose all in-memory
  state."
- "It is not recommended to write code with any side effects outside of steps... the
  Workflow engine may restart while an instance is running."
- Non-deterministic branching outside a step (`Math.random()`, `Date.now()` used in
  conditionals outside `step.do`) "could behave differently if the Workflow restarts."
- Non-idempotent API/binding calls "are always done after checking if the operation is
  still needed" (i.e. must be step-wrapped so the check-then-act is atomic w.r.t. replay).
- Persisted state must be "exclusively comprised of `step.do` returns."
- Every `step.do()` call must be `await`ed — omitting `await` creates "a dangling Promise"
  causing "exceptions being swallowed (and not terminating the Workflow)."

**What's forbidden inside a step vs outside**: side effects and non-deterministic value
generation are required to happen *inside* `step.do`'s callback (its return value is the
only thing that gets durably cached); state/logic *outside* steps must be pure/derivable
from step outputs only, since it isn't persisted across hibernation/restart.

**Return value serialization**: primitives, `Array`/`Object` composites (if recursively
serializable), and "any structured-cloneable type" up to "no longer than 1 MB"; `Function`,
`Symbol`, and circular references throw.

**Sleep primitives**:
- `step.sleep(name, duration)` — relative, e.g. `"1 hour"`; accepts ms number or a
  human string with units `second|minute|hour|day|week|month|year`.
- `step.sleepUntil(name, date)` — absolute, `Date` or unix-ms timestamp.
- "A Workflow instance that is resuming from sleep will take priority over newly scheduled
  (queued) instances" — an explicit scheduling-priority guarantee not present in anything
  found in the Inngest clone.

**Retry primitives**: default config `{ retries: { limit: 5, delay: 10000, backoff:
"exponential" }, timeout: "10 minutes" }`; per-step override via `step.do(name, { retries:
{ limit, delay, backoff: "constant"|"linear"|"exponential" }, timeout }, callback)`; `delay`
may be a function of `({ ctx, error }) => ...` returning a duration (string/number/promise),
letting retry backoff read the error (e.g. bump delay on a rate-limit message). Throwing
`NonRetryableError` inside a step: "The Workflow instance itself will fail immediately, no
further steps will be invoked" — note this is instance-fatal, unlike Inngest's
`NonRetriableError` which (per its docs) can fail just the enclosing step while the rest of
the function's already-completed steps stay memoized and other independent branches are
unaffected. This is a materially different blast radius between the two systems and is
called out explicitly here because it's easy to conflate the two identically-named error
classes.

**Instance ID / duplicate-create handling** (`trigger-workflows` / `workers-api` pages):
`Workflow.create({id, ...})` "Throws an error if the provided ID is already used by an
existing instance that has not yet passed its retention limit" — a hard error, not an
attach and not a silent skip. `createBatch()` differs: "Unlike create, this operation is
idempotent and will not fail if an ID is already in use" — a duplicate-ID entry in a batch
is silently *excluded from the returned array* (skipped), not attached to. To force a
same-ID instance to run again, the documented path is `restart()` on the existing instance
(replace-in-place), not re-`create()`.

---

## 4. Lessons for tally-flow keying — factual failure-mode inventory (no recommendations)

| Failure mode | Inngest (inngest-js) | Cloudflare Workflows |
|---|---|---|
| Duplicate literal step ID within one run | Auto-suffixed `id:2`, `id:3`, ... via probing (`resolveStepIdCollision`, `engine.ts:3269-3309`); executes as if a new step. Cross-parallel-chain duplicates additionally emit a runtime warning (`AUTOMATIC_PARALLEL_INDEXING`) but still proceed. | Exposed as a *visible* counter (`ctx.step.count`, 1-indexed) the user can branch on; no evidence of an auto-suffix — the same `name` re-run is a legitimately supported "call this step N times" idiom, not flagged as an error. |
| Duplicate ID for a *lazy/fire-and-forget op* (`defer()`) within one run | Detected and the duplicate is **skipped** with a warning ("defer skipped: duplicate ID within run", `engine.ts:2374-2382`) — different (stricter) policy than regular steps, because these ops have no memoized-result identity to fall back on. | Not applicable — no fire-and-forget op class documented for Workflows. |
| Function/workflow code changed between the run's start and a replay (steps added/removed/reordered) | Documented (not code-enforced in this clone) "graceful determinism": added steps just run when reached (must not depend on unexecuted step data); same-ID step whose *body* changed still returns the *old* memoized result; changed *ID* forces re-execution; removed steps' stale memoized data is simply never read again; reordering only *warns*, execution stays correct because lookup is by ID not by position. | `rules-of-workflows` documents *how to avoid* nondeterminism (don't branch outside steps on `Date.now()`/`Math.random()`, don't hold state outside steps) but the fetched pages contain no explicit statement of what happens if step names/order change between a deploy and a running instance's resume — not found, flagged **unknown**, not guessed. |
| Two racing invocations of "the same run" (executor requeued while a stale worker is still executing) | Server returns HTTP 409 on checkpoint; SDK reifies this as `StaleDispatchError` and the *losing* invocation halts immediately (`function-rejected`, non-retriable) rather than attaching to or resuming the winner (`api/api.ts:585-593`, `engine.ts:948-960`). Explicit rationale in-code: returning buffered ops from the loser would let the executor "memoize them as canonical and chain the next dispatch off this dead invocation" — i.e. the failure mode being defended against is exactly "stale writer clobbers/forks canonical state," not "how do I attach." | `create()` on a live/retained duplicate ID throws (hard reject); `createBatch()` silently skips the duplicate entry; `restart()` is the only documented way to reuse an ID, and it explicitly replaces rather than joins. No attach concept found. |
| Singleton / "only one run of this function" (Inngest-specific feature, not step-level but is the closest first-class "what do do with a second concurrent invocation" policy) | Two explicit modes, both **not** attach: "skip" — "Skips the new run if another run is already executing," existing run continues untouched; "cancel" — "Cancels the existing run and starts the new one." Documented caveat: rapid repeated triggers "may result in some runs being skipped rather than cancelled, similar to a debounce effect." | Not found in fetched pages; Workflows' per-instance-ID model (§3) is the nearest analog and behaves like Inngest's "skip" (hard-reject/soft-skip a duplicate ID) with no cancel-mode found. |
| Parallel steps completing out of order | Reconciled via server-supplied `stepCompletionOrder`; the SDK replays *reporting* of newly-found steps in that server order regardless of which `Promise.all` callback's microtask actually resolves first (`engine.ts:2298-2345`). Memoization itself is still keyed by hashed ID, so out-of-order completion never risks writing a result under the wrong key — only the *reporting/batching* order is reconciled. | Not directly addressed in fetched pages beyond the general one-step-per-name model; Workflows examples shown were sequential, not parallel `Promise.all` over `step.do`. Flagged **unknown** whether Workflows supports/encourages parallel steps at all in the fetched material. |

**Headline finding**: neither system implements "a second call-site hit while the first
call's job is still in flight attaches to the first call's promise/result." Both systems'
answer to concurrent duplication is *fencing*: reject, skip, or cancel-and-replace the
second comer, never join it to the first. Attach-to-in-flight (as tally-flow's spec
requires) is not prior art these two systems provide — it would be a genuine design gap to
fill, not a pattern to transplant.

---

## 5. Lift list / Do-NOT-copy

**Lift**
- **Hash the user-supplied ID, don't trust it as the storage key directly** (Inngest,
  `engine.ts:3237-3239`). Keeps the wire/storage key fixed-length and collision-resistant
  independent of what characters the user puts in a job label; the raw string is still kept
  alongside for display/diagnostics (`userland.id`). Low-risk, mechanical thing worth
  copying into call-site key derivation.
- **Memoize the failure, not just the success** (Inngest `StepError`,
  `components/StepError.ts`; confirmed behaviorally by the `handling-step-errors` fixture).
  A step that permanently failed and had its retries exhausted must replay as the *same
  rejection* on re-run, not silently vanish or re-attempt. Directly load-bearing for
  tally-flow's memoized-result collapse paragraph — the memoized witness needs an
  error-shaped variant, not just a success-shaped one.
- **Keep persisted state to step/job outputs only** (Cloudflare `rules-of-workflows`: "state
  exclusively comprised of `step.do` returns"; "do not store state outside of a step").
  Matches tally-flow's own constraint that the deterministic script's *variables* must be
  reconstructible purely from replayed `job()` results — this is independent validation of
  that shape from a second production system.
- **Treat "duplicate ID" and "still in-flight" as two different problems with two different
  detections**, per Inngest's split between `resolveStepIdCollision` (same-tick, static,
  local-state check) and `StaleDispatchError` (cross-invocation, server-fenced, HTTP-level
  check). Don't conflate call-site key collision (a same-run authoring bug) with dispatch
  racing (a crash-recovery/liveness problem) — they need different remedies in both these
  systems and worth keeping separate in tally-flow's spec language too.
- **Sleep/retry as durable primitives with explicit, inspectable config** (Cloudflare
  `step.do(name, {retries, timeout}, fn)`, dynamic delay as a function of `(ctx, error)`).
  The shape (declarative retry policy attached at the call site, not ambient/global) is
  a clean reference for whatever tally-flow's job() retry options end up looking like.

**Do NOT copy**
- **Inngest's silent auto-suffix on duplicate step IDs** (`id:2`, `id:3`, ...,
  `engine.ts:3269-3309`). This trades a real authoring bug (accidentally reusing a
  call-site key) for silent behavior-change instead of a loud error; the only user-visible
  signal is an *optional* warning that itself only fires in the cross-parallel-chain case.
  For tally-flow, where call-site key IS the crash-recovery attach key, a silently-renamed
  key on the *n*th occurrence is exactly the kind of nondeterminism that would break
  witnessed-result collapse on replay if the collision detector's internal counters (a
  `Map`, rebuilt fresh each run from scratch) ever diverge between runs — e.g. because a
  middleware conditionally added a colliding call on one run and not another. Worth noting:
  this in-memory-counter-based scheme is itself only deterministic given deterministic
  control flow, i.e. it inherits the exact assumption tally-flow is trying to make
  explicit and rigid instead of implicit and best-effort.
- **Cloudflare's `NonRetryableError` killing the *entire instance*, not just the step**
  (§3). If tally-flow adopts a similarly-named "don't retry this" primitive, it must decide
  explicitly which blast radius it means — Inngest's version (step-scoped, sibling
  steps/branches keep their memoized results) and Cloudflare's (instance-fatal) are not
  interchangeable, and copying the name without copying the semantics (or vice versa) would
  silently pick one of two very different behaviors.
- **Relying on a hidden/global generation-id + HTTP-409 fence as the *only* answer to
  racing invocations** (Inngest, `api/api.ts:585-593`). This works because Inngest has a
  centralized executor that both invocations must round-trip through; tally-flow's stated
  model (local scheduler daemon, attach rather than fence) is explicitly a different
  contract — a fence-and-reject strategy transplanted wholesale would contradict the
  attach-semantics requirement in the spec, not merely under-implement it.
- **Treating "graceful determinism" (Inngest's stated posture) as a target to imitate.**
  Inngest explicitly chose warn-and-continue over enforce-and-reject for reordered/duplicate
  steps (§1, §2). That is a deliberate leniency trade-off for a multi-tenant SaaS serving
  arbitrary user code it can't fully control; it is evidence of *a* choice, not evidence
  that leniency is correct for tally-flow's own (single-operator, local, spec-controlled)
  environment.

---

## Gaps / explicitly unknown

- Whether/how the Go executor (server-side, not in this JS clone) implements anything
  attach-like for a duplicate dispatch is unknown — out of scope of `inngest-js`.
- Whether Cloudflare Workflows supports `Promise.all`-style parallel steps at all, and if
  so how out-of-order completion is reconciled, was not found in the fetched pages.
- Whether Cloudflare detects/warns on reordered step names between a code deploy and a
  resuming instance (the `rules-of-workflows` page tells you not to rely on order changing
  silently but does not state what detection, if any, exists) is unknown.
- WebFetch's summaries were used as the docs source rather than raw HTML diffing; exact
  wording nuances (e.g. whether the 256-character step-name limit is enforced or merely
  advised) should be re-verified against raw markdown if this brief is used to settle a
  wording-sensitive spec paragraph.
