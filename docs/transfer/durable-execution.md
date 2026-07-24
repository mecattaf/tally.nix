# Style transfer: replay validation and the determinism contract

Sources cloned/fetched 2026-07-24:
- `git clone --depth 1 https://github.com/temporalio/sdk-core` → `~/Downloads/temporal-sdk-core` (rev at clone time; shallow, no tags)
- `git clone --depth 1 https://github.com/obeli-sk/obelisk` → `~/Downloads/obelisk`
- Microsoft Learn: `durable-task-code-constraints` (canonical URL `.../azure/durable-task/common/durable-task-code-constraints`) and `durable-functions-versioning` (canonical `.../azure/durable-task/durable-functions/durable-functions-versioning`), both fetched live.

Context this feeds: tally-flow is a Rust crate embedding a JS engine to run deterministic orchestration scripts. Crash recovery = full re-execution of the script with per-call memoization against a durable witness ledger. The open question this brief informs: on replay, how do we detect that re-executed script code has diverged from the recorded ledger, and what do we do when a script is edited while a run is mid-flight.

---

## 1. Temporal sdk-core replay machinery

Crate layout: the workflow replay engine lives in `crates/sdk-core/src/worker/workflow/` inside `temporalio/sdk-core` (not a top-level `core/` — that's stale nomenclature from older Temporal docs/blog posts). Key files (line counts as cloned):

- `crates/sdk-core/src/worker/workflow/machines/workflow_machines.rs` (1802 lines) — the top-level replay driver, one `WorkflowMachines` instance per workflow run.
- `crates/sdk-core/src/worker/workflow/machines/mod.rs` — the generic `TemporalStateMachine` trait and blanket impl that every individual command-type machine plugs into.
- `crates/sdk-core/src/worker/workflow/machines/*_state_machine.rs` — one file per Temporal command type (activity, timer, child_workflow, signal_external, patch, cancel_*, nexus_operation, local_activity, update, upsert_search_attributes, modify_workflow_properties, workflow_task, complete/fail/continue_as_new/cancel_workflow).
- `crates/sdk-core/src/worker/workflow/mod.rs` — `WFMachinesError` enum and the `nondeterminism!`/`fatal!` macros.
- `crates/sdk-core/src/worker/workflow/managed_run.rs` — wraps a `WorkflowMachines`, turns machine errors into workflow-task-failure responses sent to the server.

### The matching mechanism

Replay works by re-running the workflow closure against the *same* sequence of state machines that produced the original commands, then feeding the recorded history events back into those same machine instances in order:

1. `WorkflowMachines` keeps `self.commands: VecDeque<CommandAndMachine>` — the ordered list of commands the (re-)executing workflow code has emitted so far, each tagged with the machine (`MachineKey`) that produced it. This queue is exactly the thing tally-flow's witness ledger would need an analogue of: "the calls the script made, in order, with which memoized-call slot each belongs to."
2. `handle_command_event` (`workflow_machines.rs:937-1000`) is the entry point for any history event that is itself the result of a previously-issued command (`event.is_command_event()`, dispatched from `workflow_machines.rs:896-897`). Its own doc comment (`workflow_machines.rs:928-936`) states the invariant plainly:
   > "A command event is an event which is generated from a command emitted as a result of performing a workflow task... The handling consists of verifying that the next command in the commands queue is associated with a state machine, which is then notified about the event and the command is removed from the commands queue."
3. Concretely: it pops `self.commands.front()`. If the queue is empty — i.e. history has an event but re-execution didn't (yet) produce a corresponding command — it raises `nondeterminism!("No command scheduled for event {event}")` (`workflow_machines.rs:980`). This is the **count/ordering** check: fewer (or differently-ordered) commands than history expects.
4. If a command is present, the event is fed to that command's specific machine via `submachine_handle_event` → `TemporalStateMachine::handle_event` (generic impl in `machines/mod.rs:149-175`). That generic impl does `let converted_event: Self::Event = event_dat.try_into()?;` — each machine implements `TryFrom<HistEventData> for <Machine>Events`, and that conversion fails (returns `nondeterminism!`/`fatal!`) if the history event's `EventType` isn't one the machine in this state can accept. This is the **command-type / event-type** check (e.g. `timer_state_machine.rs`'s `TryFrom` only accepts `TimerStarted`/`TimerCanceled`/`TimerFired`, anything else is `nondeterminism!("Timer machine does not handle this event")`).
5. A **third, finer-grained layer** exists but is deliberately narrow: a handful of machines compare specific *attributes* of the recorded event against the attributes of the command the current run just built, gated behind an internal SDK-version flag `CoreInternalFlags::IdAndTypeDeterminismChecks`. Example, `activity_state_machine.rs:373-395` (`ScheduleCommandCreated::on_activity_task_scheduled`):
   ```rust
   if sched_dat.act_id != dat.attrs.activity_id {
       return TransitionResult::Err(nondeterminism!(
           "Activity id of scheduled event '{}' does not match activity id of activity command '{}'",
           sched_dat.act_id, dat.attrs.activity_id));
   }
   if sched_dat.act_type != dat.attrs.activity_type {
       return TransitionResult::Err(nondeterminism!(/* type mismatch */));
   }
   ```
   The identical pattern exists for child workflows (`child_workflow_state_machine.rs:236-256`, comparing `workflow_id` and `workflow_type`).

**Answer to "what granularity of mismatch is caught": three layers, in order of coarseness — (a) command present/absent and ordering (queue-empty check), (b) command/event *type* (via the per-machine `TryFrom` conversion — this is the dominant check and is unconditional), (c) `activity_id`/`activity_type` (and workflow-equivalent `workflow_id`/`workflow_type`) identity comparison for activities and child workflows only, gated behind an internal versioning flag so old SDK versions that never had this check don't spuriously fail on histories predating it.** Full command *argument/payload* equality (e.g. input args, timer duration) is explicitly **not** compared — the payload the original command carried is simply discarded once the corresponding history event is matched by type (+id/type where checked); Temporal does not diff serialized inputs across replay. That's a load-bearing simplification worth noting for tally-flow's own scope decision.

### Error path and reporting

`WFMachinesError` (`worker/workflow/mod.rs:1516-1523`) has exactly two variants:
```rust
pub(crate) enum WFMachinesError {
    #[error("[TMPRL1100] Nondeterminism error: {0}")]
    Nondeterminism(String),
    #[error("Fatal error in workflow machines: {0}")]
    Fatal(String),
}
```
`Nondeterminism` carries a fixed error code `TMPRL1100`. `managed_run.rs:1040-1056` catches machine-update failures and maps `WFMachinesError::Nondeterminism` specifically to `WorkflowTaskFailedCause::NonDeterministicError`, which is sent back to the Temporal server as the workflow task completion's failure cause; `Fatal` maps instead to a generic `WorkflowWorkerUnhandledFailure`. `WFMachinesError::evict_reason()` (`mod.rs:1598-1603`) additionally maps `Nondeterminism → EvictionReason::Nondeterminism`, which drives cache eviction of the sticky workflow-execution cache — the run must be rebuilt from scratch next time (there is no partial-recovery path once nondeterminism is detected; the whole in-memory machine state for that run is thrown away).

Both macros (`nondeterminism!`/`fatal!`, `mod.rs:1528-1563`) also fire an `antithesis_assertions`-feature-gated `assert_unreachable!` — i.e. this code path is treated as a target for Antithesis-style deterministic-simulation fuzz testing, not just a runtime guard. Relevant if tally-flow ever wants a simulation-testing story for its own witness-ledger comparison.

---

## 2. The determinism contract imposed on workflow authors

### Temporal (from sdk-core + general SDK knowledge)

Not banned by a static analyzer — Temporal's Rust/other-lang SDKs rely on documentation + a few runtime traps, not compile-time enforcement. From sdk-core: no time, no random, no direct I/O, no threads inside orchestrator/workflow code; all such needs are routed through context-provided deterministic equivalents or delegated to activities (recorded in history so replay reuses the recorded result rather than re-executing the nondeterministic call).

**Versioning/patching**: `patch_state_machine.rs` implements `patched(patch_id)` / `deprecate_patch(patch_id)` (Rust SDK surface: `crates/workflow/src/workflow_context.rs:1016-1024` and `:1448-1456`, calling into `patch_impl`). Mechanism:
- A `patched(id)` call records a `RecordMarker` command (marker name `PATCH_MARKER_NAME`) the first time a run executes it live; on replay, the machine's `TryFrom<HistEventData>` recognizes the marker (`patch_state_machine.rs:251-263`, via `get_patch_marker_details()`), and `patched()` returns `true` if the marker is present in history, `false` if it's absent (meaning this run predates the code change and must take the *old* branch).
- The full behavior matrix is documented as a table in the file's module doc-comment (`patch_state_machine.rs:1-18`) — it is the single clearest artifact in the crate for this concern, worth reading directly. Core cases: no marker + no `patched()` call = ordinary replay, unaffected; marker present + no `patched()` call in current code = **nondeterminism** ("no matching command"); marker present + `patched()` present = returns `true`; no marker + `patched()` present = returns `false` (old-branch behavior preserved); a **deprecated** marker (`deprecate_patch`) is specifically permitted to be silently ignored/skipped even when the current code no longer calls `patched()` at all — this is the two- (or three-) stage rollout: introduce the patch, wait for all pre-patch runs to complete or transition, then deprecate (still tolerate the marker), then eventually remove the marker check entirely once no live history contains it.
- The lookahead function `patch_marker_handling` (`workflow_machines.rs:1737-1792`) is what makes deprecation replay-safe: if the current run has no patch machine for a marker but the marker is flagged `deprecated`, the event (and a following `UpsertWorkflowSearchAttributes` event if present) is skipped rather than raising nondeterminism.
- This is a **per-call-site, per-run** mechanism — each `patched()` invocation is independently resolved against whether *that specific run's* history contains the marker. It does not version the whole workflow; a single run can straddle old and new code paths at different call sites depending on when it was created relative to each patch's rollout.

### Azure Durable Functions / Durable Task (from live docs, `durable-task-code-constraints` + `durable-functions-versioning`)

Banned APIs are explicit and documented per-language (`durable-task-code-constraints`, fetched 2026-07-24): `DateTime.Now`/`DateTime.UtcNow`/`Stopwatch` (use `context.CurrentUtcDateTime`); `Guid.NewGuid()`/`crypto.randomUUID()`/language-native UUID gen (use `context.NewGuid()`, which is documented to produce Type-5 UUIDs deterministically); raw random numbers (must come from an activity, or a seeded PRNG reused identically each replay); direct I/O/bindings/HTTP in-line (must go through an activity, or the SDK's own durable-HTTP wrapper); static/module-level mutable state; environment variables read at orchestration time (must be passed in as input or via an activity); blocking sleep (`Thread.Sleep`, `time.sleep`) — must use `context.CreateTimer()`; any async operation not issued through the context object (`Task.Run`, `setTimeout`, `HttpClient.SendAsync` are called out by name as forbidden); language-model constraints — JS orchestrators must be plain generator functions (not `async`), Python orchestrators must be generators using `yield` not coroutines using `await`, because "coroutine semantics don't align with the... replay model."

There is a documented **runtime detector** for one class of violation only: ".NET threading APIs" — the doc states "The Durable Task Framework attempts to detect accidental use of nonorchestrator threads... If it finds a violation, the framework throws a `NonDeterministicOrchestrationException`... However, this detection behavior won't catch all violations, and you shouldn't depend on it." This is a narrower and more honestly-hedged guarantee than Temporal's replay-matching, which structurally *must* run on every history event, not just opportunistically.

**Versioning/patching equivalent**: Durable Functions/Durable Task has no Temporal-style per-call-site `patched()` primitive. Its recommended mechanism (`durable-functions-versioning`, fetched live) is **"Orchestration versioning"** — a *whole-instance* version tag, not a per-call marker:
- Each orchestration instance is permanently assigned a version string at creation time.
- Orchestrator code can branch on its own instance's version ("Orchestrator functions can examine their version and branch execution accordingly, keeping old and new code paths in the same codebase").
- The runtime enforces that "workers running older orchestrator function versions" cannot execute "orchestrations of newer versions" — version compatibility is checked at the worker/instance level before any code runs, not detected after the fact via a replay mismatch.
- Documented fallback strategies if you don't use orchestration versioning: side-by-side deployment (new storage account or new task-hub name — full isolation, old instances drain on old infrastructure untouched) or "stop all in-flight instances" (prototyping only, explicitly discouraged for production).
- The doc is explicit about what happens with **no mitigation**: "Deploying breaking changes without a mitigation strategy... can cause orchestrations to fail with *nondeterministic orchestration* errors, get stuck indefinitely in a `Running` status, or trigger low-level runtime failures." It also gives the exact mechanism of detection for logic changes (adding/removing/reordering calls): "During replay, if the original call to `Foo` returned `true`, then the orchestrator replay calls into `SendNotification`, which isn't in its execution history. The runtime detects this inconsistency and raises a *non-deterministic orchestration* error because it encountered a call to `SendNotification` when it expected to see a call to `Bar`." — i.e. the same "next-expected-command-vs-actual" ordering check as Temporal's `handle_command_event`, just without Temporal's finer per-call patch primitive to route around it; DF's answer to "the script changed mid-flight" is coarser-grained (whole-instance version pinning) rather than Temporal's fine-grained (per-call-site marker) approach.

**This is the crux fact for tally-flow's own ruling**: the two strongest prior-art systems chose *different granularities* for the same problem — Temporal: per-call-site opt-in marker, resolved independently for each call each run. Durable Functions: whole-instance version tag, resolved once at instance creation. Tally-flow's spec needs to pick one of these shapes (or explicitly reject both) for "what happens when a flow script is edited while a run is mid-flight" — this is exactly the decision this brief was requested to surface.

---

## 3. Obelisk (found: `github.com/obeli-sk/obelisk`, shallow-cloned to `~/Downloads/obelisk`)

Obelisk is a pre-release, single-binary durable/deterministic workflow engine built on the WASM Component Model (WIT-defined interfaces, WASI 0.2), persisting an execution log to SQLite or Postgres, and — notably for tally-flow's own architecture — it embeds **Boa** (the Rust JS engine) to run JS-authored workflows and activities directly, alongside a native WASM-component path (`crates/boa-common/`, `crates/workflow-js-runtime/`, `crates/activity-js-runtime/`). Determinism-by-construction there is enforced at the *host-import* boundary rather than by post-hoc replay diffing: `crates/workflow-js-runtime/src/deterministic_executor.rs` supplies a custom Boa `JobExecutor` (`DeterministicJobExecutor`) that runs promise/async/generic jobs from fixed, explicitly-drained queues and outright `panic!`s if the engine ever tries to schedule a `Job::TimeoutJob` ("Workflow must be deterministic, timeout jobs are not supported") — i.e. nondeterministic primitives (timers, wall-clock, ambient randomness) are refused at the JS-engine-integration layer before they can ever produce a divergent value, rather than being allowed to run and then checked against history afterward. The replay/mismatch-detection analogue lives in `crates/wasm-workers/src/workflow/event_history.rs` (4517 lines) — it defines an `EventHistory` type, an `ApplyError`/`DbErrorWriteOrReplayInterrupt` error split (roughly Temporal's `Nondeterminism`/`Fatal` split), and, notably, an `AwaitNextExtensionError::FunctionMismatch` variant (`event_history.rs:930-955`) reporting mismatch by fully-qualified function name (ffqn) when a child/join-next call on replay doesn't match what history recorded — the direct structural analogue of Temporal's activity `act_id`/`act_type` check, but keyed on the WASM component function signature instead of a Temporal activity id.

Given that Obelisk already solves "embed a JS engine, make it deterministic, persist an execution log, detect replay divergence" as its whole reason for existing, it is worth a deeper follow-up read (not done in this pass — `event_history.rs` at 4517 lines was skimmed for structure/vocabulary only, not read end-to-end) before tally-flow finalizes its own witness-ledger comparison logic. `crates/boa-common/` in particular (`imports.rs`, `wasi_job_executor.rs`, `crypto.rs`) is the closest available prior art for "how do you sandbox Boa's ambient nondeterminism (Math.random, Date, WASI imports) at the binding layer" — a problem tally-flow has to solve regardless of what it does about replay validation.

---

## 4. Lift list for tally-flow's runner

The one idea most worth mirroring, stated precisely:

**Before attaching a re-derived call to an existing memoized ledger slot, verify identity+type match before trusting/reusing the memoized result — don't just match by position/count.** Temporal's structure for this (`activity_state_machine.rs:373-395`, `child_workflow_state_machine.rs:236-256`) is:
1. Match by **position in the ordered command queue** first (`workflow_machines.rs:965-992`, `handle_command_event`) — this is what catches "the script now calls fewer/more/reordered things than the ledger has."
2. Within a matched position, match by **command/event type** (per-machine `TryFrom<HistEventData>`, `machines/mod.rs:149-175`) — catches "this call-site now does a different *kind* of operation."
3. Within a matched type, for the two composite-identity command kinds (activity, child workflow), compare a **small, explicit identity tuple** — `(id, type)` for activities, `(workflow_id, workflow_type)` for child workflows — not the full argument payload. This check is itself versioned/optional (`CoreInternalFlags::IdAndTypeDeterminismChecks`), which is a second lift-worthy idea: **ship the stricter check behind a flag that's only turned on for runs created after the flag existed**, so tightening the determinism check later doesn't retroactively break in-flight runs that predate the tightening. This is exactly the kind of forward-compatible-strictness mechanism a witness-ledger validator needs if tally-flow ever wants to add finer-grained checks after v1 ships.
4. Explicitly **do not** diff full argument/payload equality — Temporal doesn't, and Obelisk's ffqn-mismatch check similarly doesn't appear to deep-compare arguments (per the skim above, worth confirming with a deeper read if tally-flow considers going further than Temporal here).
5. On any of the above mismatches, fail the *entire* run's resumption with a single tagged error class (`WFMachinesError::Nondeterminism`, code `TMPRL1100`) distinct from other-fatal-errors, and **evict/discard all in-memory state for that run** rather than attempting partial repair (`managed_run.rs:1040-1056`, `EvictionReason::Nondeterminism`). Cheap and safe; the run gets rebuilt fresh next time it's picked up. tally-flow's witness ledger comparison should probably adopt the same "detect → tag → discard-and-rebuild, never patch in place" posture rather than trying to reconcile a partially-diverged run.

---

## 5. Do-NOT-copy

- **Temporal's server/history-service architecture** (the durable-task-framework side: history shards, task queues, matching service, multi-tenant server deployment model). sdk-core is a *client-side replay library* talking to that server over gRPC; none of the server-side persistence/sharding/queueing design is relevant to tally-flow, which is a single embedded crate with its own local witness ledger and no separate server process. Copying any of that would be solving a distributed-systems problem tally-flow doesn't have.
- **Temporal's internal-flags versioning-of-the-checker-itself machinery** (`CoreInternalFlags`, SDK-capability negotiation with the server) beyond the *idea* noted in §4.3 above — the concrete implementation is bound up with Temporal's SDK/server capability-negotiation protocol (worker sends its supported flag set, server acks) which has no counterpart in a single-process embedded crate. Take the principle, not the mechanism.
- **Durable Functions' whole-instance version-and-branch model as the *only* answer** — it's simpler than Temporal's per-call-site markers but pushes all the "what does old vs new mean" logic into hand-written `if version >= X` branches scattered through orchestrator code, which the docs themselves flag as something you maintain indefinitely ("keeping old and new code paths in the same codebase"). Given tally-flow's flows are short JS scripts rather than long-lived .NET/Java codebases, this may be the wrong ergonomic trade — worth an explicit ruling rather than defaulting to it.
- **Obelisk's WASM Component Model / WIT-interface boundary as an architecture to imitate wholesale.** Obelisk's determinism-by-construction depends on treating the workflow as an isolated WASM component with a narrow, statically-typed host-import surface (WIT). Tally-flow embeds Boa directly in-process with a JS-object-shaped host API, which is a different (looser) sandboxing model; adopting Obelisk's WASM-component isolation would be a rewrite, not a transfer. The *narrower* lift — how Obelisk's `DeterministicJobExecutor` refuses nondeterministic job kinds outright — is fair game (§3/§4); the surrounding component-model plumbing is not.

---

## Open / unknown

- Obelisk's `event_history.rs` (4517 lines) was skimmed for vocabulary and error-type names only, not read for its full replay algorithm — if the lift in §4 needs more than the outline given here, that file needs a dedicated follow-up pass.
- Did not locate a Temporal doc/blog section discussing whether `patched()` checks are ever retroactively tightened the way `IdAndTypeDeterminismChecks` implies (i.e. the *history* of that flag's introduction) — the flag's existence is confirmed in code; its changelog/rationale was not looked up.
- Obelisk's actual on-disk execution-log schema (souce: `assets/schemas/`, not opened) was not inspected — relevant if tally-flow wants a concrete witness-ledger schema comparison beyond Temporal's in-memory command queue.
