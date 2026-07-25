# Style-transfer brief: boa-dev/boa as the embedded JS engine for `tally-flow`

Source: `git clone --depth 1 https://github.com/boa-dev/boa` cloned to `~/Downloads/boa` on 2026-07-24.
Repo state at clone time: workspace version `1.0.0-dev` (unreleased, ahead of the last published
crate). All paths below are relative to the clone root unless stated otherwise.

## 1. Role in tally-flow

Boa (`boa_engine`) is the candidate host engine for `tally-flow`'s deterministic script runner: a
pure-Rust, no-C-dependency ECMAScript implementation that exposes the entire event-loop machinery
(job queue, module loader, clock, host hooks) as pluggable traits rather than baking in any OS
timers, filesystem, or network access. That is precisely the shape tally-flow needs — a script
environment whose only I/O is the host-registered `job()`/`parallel()`/`pipeline()`/`log()`
surface, with `Date`, `Math.random`, filesystem and network absent by construction (Boa ships none
of these by default; a companion crate, `boa_runtime`, adds them opt-in) and with promise/microtask
scheduling fully owned by the embedder's `JobExecutor` implementation. The core design question for
tally-flow is not "does Boa support async" (it does, natively) but "who decides the order in which
concurrently-completing daemon jobs become observable to the script" — and Boa's architecture puts
that decision entirely in the host's hands (see §3), which is what makes deterministic replay
possible at all.

## 2. Lift list

### Context creation and configuration
- `Context::default()` for a batteries-included context; `Context::builder()` /
  `ContextBuilder` for full control — `core/engine/src/context/mod.rs:1144` (`.clock(...)`),
  `:1158` (`.module_loader(...)`), builder's `.job_executor(...)`, `.host_hooks(...)`.
- `examples/src/bin/tokio_event_loop.rs:258-261` and `smol_event_loop.rs:251-254` show the
  canonical builder assembly: `ContextBuilder::new().job_executor(Rc::new(queue)).build()`.
- Registering globals: `Context::register_global_property` (`core/engine/src/context/mod.rs:252`,
  doc example inline) and `register_global_callable`/`register_global_builtin_callable`
  (used in `examples/src/bin/tokio_event_loop.rs:199-223` to install `console`, a sync builtin,
  and an async builtin in one function).

### Registering global native functions — sync and async
- Sync: `NativeFunction::from_fn_ptr` (plain `fn` pointer) and `NativeFunction::from_copy_closure`
  / `from_copy_closure_with_captures` (`Copy` closures, safe) or the `unsafe` `from_closure` /
  `from_closure_with_captures` for non-`Copy` captures — all in
  `core/engine/src/native_function/mod.rs:159-260`. Full worked example with captured state:
  `examples/src/bin/closures.rs` (plain closures at line 27, closures with GC-traced captures at
  line 73, and an `unsafe` closure over `Cell`/`RefCell` at line 151).
- **Async — this is the load-bearing mechanism for `job()`.**
  `NativeFunction::from_async_fn` at `core/engine/src/native_function/mod.rs:196-224`:
  ```rust
  pub fn from_async_fn<F>(f: F) -> Self
  where
      F: AsyncFn(&JsValue, &[JsValue], &RefCell<&mut Context>) -> JsResult<JsValue> + 'static,
      F: Copy,
  ```
  Internally it (a) creates a pending `JsPromise` via `JsPromise::new_pending`, (b) enqueues a
  `NativeAsyncJob::new(async move |context| { ... })` that awaits the user's future and then calls
  `resolvers.resolve`/`resolvers.reject` on completion, and (c) returns the promise synchronously
  to the calling script. This is exactly the `job(spec)` shape: the native fn returns a `Promise`
  immediately; the Rust future (talking to the daemon over the Unix socket) resolves it later from
  inside the job executor. Worked full example returning a real future:
  `examples/src/bin/tokio_event_loop.rs:148-163` (`fn delay(...) -> impl Future<Output=JsResult<JsValue>>`,
  registered at line 211 with `NativeFunction::from_async_fn(delay)`).
  Note the `F: Copy` bound — a native async fn cannot directly close over non-`Copy` state (e.g. an
  `Rc<Socket>`); pass shared state via `Copy` handles (indices/ids into a table) or via
  `context.realm().host_defined()` (see `examples/src/bin/host_defined.rs`, `insert`/`get` at
  lines 45 and 51) rather than closure capture.
- For scheduling a raw async job manually (not through a promise-returning native fn), use
  `NativeAsyncJob::with_realm(async move |context| {...}, context.realm().clone())` and
  `context.enqueue_job(job.into())` — used for the `interval()` builtin in
  `examples/src/bin/tokio_event_loop.rs:167-196`.

### The `JobExecutor` trait and custom job queues (critical section)
- Trait definition: `core/engine/src/job.rs:795` (approx, see `pub trait JobExecutor: Any`)
  with two methods: `fn enqueue_job(self: Rc<Self>, job: Job, context: &mut Context)` and
  `fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()>`, plus a default-forwarding
  `async fn run_jobs_async(...)` that real async executors override.
- `Job` enum variants (all in `core/engine/src/job.rs`): `PromiseJob` (microtask, from `.then()`
  handlers and promise reactions), `AsyncJob`/`NativeAsyncJob` (a job that itself is a `Future`,
  used by `from_async_fn` and by user code scheduling raw futures), `TimeoutJob`/`IntervalJob`
  (time-keyed, driven by `context.clock().now()`), `GenericJob` (`job.rs:443`, a one-shot realm-bound
  closure with no ordering guarantees from the spec — "there is no strict specification as to
  priority and ordering" per its doc comment), and `FinalizationRegistryCleanupJob`.
- **Spec-compliance note that is the crux of the determinism problem**, verbatim from
  `core/engine/src/job.rs` (`PromiseJob` doc comment, ~line 583): promise jobs must (1) run with
  the right realm active, (2) have the right active script/module, and (3) "run in the same order
  as the `HostEnqueuePromiseJob` invocations that scheduled them." Boa's own `NativeJob` guarantees
  (1) and (2) internally; **the doc explicitly states "implementations of `JobExecutor` must only
  guarantee that jobs are run in the same order as they're enqueued."** In other words: Boa
  guarantees FIFO delivery of whatever you enqueue, but it is entirely up to the host's
  `JobExecutor` to decide *when* a given `NativeAsyncJob`'s future resolves and thus in what order
  the corresponding `resolve()`/`reject()` calls (and hence `PromiseJob`s) get enqueued in the
  first place. This is exactly the seam tally-flow needs to control: the runner's `JobExecutor`
  impl is where "N jobs submitted to the daemon complete in wall-clock-arbitrary order" gets turned
  into "a single, reproducible enqueue order" — e.g. by buffering all futures that become ready in
  a scheduling tick and flushing them in a canonical order (submission id) rather than true
  completion order, or by recording completion order during a live run and replaying it verbatim.
- Reference (non-deterministic-by-design) executor implementations to study, not copy verbatim:
  - `core/engine/src/job.rs` `SimpleJobExecutor` (Boa's own default-ish FIFO executor; look at
    `enqueue_job`/`run_jobs_async` for the general shape of draining four separate queues:
    `promise_jobs`, `async_jobs`, `clock_jobs`, `generic_jobs`).
  - `examples/src/bin/tokio_event_loop.rs:38-145` and `examples/src/bin/smol_event_loop.rs:36-138`
    — nearly identical hand-rolled `Queue: JobExecutor` using `futures_concurrency::FutureGroup` +
    `futures_lite::future::poll_once(group.next())` to race pending async jobs, then
    `drain_jobs()` to flush the promise-job FIFO queue, then yield to the outer runtime. **This
    `poll_once(group.next())` pattern is precisely "whichever future finishes first, in real
    time" — the pattern to avoid (or to wrap with a deterministic reordering buffer) in
    tally-flow.**
  - `examples/src/bin/module_fetch_async.rs` contains, in the code itself, the confession:
    "Adding some prints to show the non-deterministic nature of the async fetches. Try to run the
    example several times to see how sometimes the fetches start in order but finish in disorder."
    (lines 26-28). This is Boa's own maintainers documenting the exact hazard tally-flow must
    design around.
- `IdleJobExecutor` (`core/engine/src/job.rs`, near the trait def) — a no-op executor useful if you
  want to fully disable promise progression (not directly useful for tally-flow since promises are
  needed for `job()`, but worth knowing it exists as the "opposite extreme").
- Host-side raw promise construction/inspection for testing/driving: `JsPromise::new`,
  `JsPromise::new_pending`, `.then()`, `.finally()`, `PromiseState::{Pending,Fulfilled,Rejected}` —
  `examples/src/bin/jspromise.rs` (whole file; e.g. lines 26-35 create-and-resolve, 95-108
  out-of-order resolution of two pending promises via `new_pending`, demonstrating that resolve
  order — not creation order — drives `.then()` firing order, which is again the crux for
  tally-flow's replay guarantee).

### Passing structured JSON both directions (`JsValue <-> serde_json`)
- `JsValue::from_json(&serde_json::Value, &mut Context) -> JsResult<JsValue>` and
  `JsValue::to_json(&mut Context) -> JsResult<Option<serde_json::Value>>` —
  `core/engine/src/value/conversions/serde_json.rs:40` and `:115`. Full round-trip doctest inline
  at lines 20-38 and 93-113; extra edge-case tests in the same file: cyclic-object detection throws
  `TypeError: cyclic object value` (`:306-319`), `undefined` maps to `None`/is dropped from
  objects but becomes `null` inside arrays (`:328-373`), `BigInt` and `Symbol` are rejected with a
  `TypeError` (`:132-134`, `:200-202`) since JSON has no representation for them — tally-flow's
  `job(spec)` marshalling must treat those as hard errors, not silently coerce.
- Object key iteration for `to_json` walks `index_property_keys()` (integer keys, ascending) then
  `shape.keys()` (string keys, insertion order) — i.e. serialization order is deterministic and
  spec-shaped (see §3).
- Alternative for typed, schema-shaped payloads (as opposed to arbitrary JSON blobs): the
  `#[derive(TryFromJs, TryIntoJs)]` macros with `#[boa(rename/rename_all/skip/into_js_with)]`
  attributes — full worked example in `examples/src/bin/try_into_js_derive.rs` (struct defs at
  lines 7-34, usage below). Useful if `job(spec)`'s spec/result shapes are fixed Rust structs;
  `serde_json` conversion remains the right tool for genuinely dynamic payloads.

### Module vs script evaluation
- Script path: `Context::eval(Source)` (`core/engine/src/context/mod.rs:205`, thin wrapper over
  `Script::parse(src, None, self)?.evaluate(self)`), or explicit `Script::parse` /
  `.evaluate(&mut Context)` / `.evaluate_async(&mut Context)` (`core/engine/src/script.rs:176,192`)
  when you need to interleave evaluation with an external async runtime without blocking
  (`examples/src/bin/tokio_event_loop.rs:283-330` shows both the blocking and the
  `evaluate_async`-inside-`LocalSet` flavors).
- Module path: `Module::parse` + `.load(context)` → `.link(context)` → `.evaluate(context)`
  (each step returns a `JsPromise`), or the one-shot `Module::load_link_evaluate` convenience —
  `examples/src/bin/modules.rs:1-70` for the manual four-step version,
  `examples/src/bin/module_fetch_async.rs:81-105` for the one-shot version.
  Modules require a `ModuleLoader` (see below); scripts do not (no `import` = no loader needed) —
  for tally-flow's flat `job()`-only scripts, plain `Script`/`Context::eval` is almost certainly
  sufficient and avoids having to reason about a module loader at all.
- Synthetic (host-authored, no JS source) modules: `SyntheticModuleInitializer`,
  `examples/src/bin/synthetic.rs` (whole file) — a way to expose host functionality as an
  `import`-able module rather than a global, if tally-flow ever wants `job`/`parallel`/`pipeline`
  namespaced instead of global.

### Controlling / removing intrinsics
- **`Math.random` cannot be hooked** — it calls `rand::random::<f64>()` directly with no
  indirection through `HostHooks` or `Clock`:
  `core/engine/src/builtins/math/mod.rs:784-786`
  (`pub(crate) fn random(...) -> JsResult<JsValue> { Ok(rand::random::<f64>().into()) }`).
  The only way to neutralize it for determinism is host-side removal/override after context
  creation (below); there is no engine-level configuration switch.
- **`Date` *is* hookable**, via the `Clock` trait: `core/engine/src/context/time.rs:147`
  (`pub trait Clock { fn now(&self) -> JsInstant; fn system_time_millis(&self) -> i64; }`) and
  `ContextBuilder::clock<C: Clock + 'static>(Rc<C>)` at `core/engine/src/context/mod.rs:1144`.
  `Date.now()`/`new Date()` read `context.clock().system_time_millis()`
  (`core/engine/src/builtins/date/mod.rs:52-53, 358-359`); `setTimeout`/interval scheduling reads
  `context.clock().now()` (monotonic). Supplying a fixed/deterministic `Clock` impl instead of the
  default `StdClock` (`core/engine/src/context/time.rs:161-193`) is the correct, supported way to
  pin wall-clock time for replay — no property deletion needed for `Date` itself, though the
  runner may still want to delete the `Date` global entirely if "no wall-clock access at all" is a
  harder requirement than "wall-clock access is pinned."
- **Deleting/overriding a global outright**: `context.global_object()` returns the realm's
  `JsObject` (`core/engine/src/context/mod.rs:440`); call
  `.delete_property_or_throw(js_string!("Date"), context)` on it. This is not a hypothetical —
  Boa's own `Context::unregister_global_class` does exactly this at
  `core/engine/src/context/mod.rs:407` (`self.global_object().delete_property_or_throw(js_string!(C::NAME), self)?`).
  The same pattern removes `Math.random`'s carrier (either delete all of `Math`, or fetch the
  `Math` object and delete/redefine its `random` property) or replace it with a deterministic
  host-controlled PRNG exposed under a different, intentional name if tally-flow ever wants a
  seedable random primitive.
- Module loading is host-controlled by default to the point of being off: `IdleModuleLoader`
  (`core/engine/src/module/loader/mod.rs`, ~line 202) "throws when trying to load any modules...
  useful to disable the module system on platforms that don't have a filesystem" — and it **is**
  the default (`core/engine/src/context/mod.rs:1220`, `Rc::new(IdleModuleLoader)` when no loader is
  supplied to the builder). So out of the box, `import` already goes nowhere; tally-flow doesn't
  need to do anything extra here unless it wants `import` to work against an in-memory registry
  (`MapModuleLoader`, same file, ~line 218) rather than disabled outright.
- Runtime guardrails worth adopting even though they are not intrinsics removal per se:
  `context.runtime_limits_mut().set_loop_iteration_limit(n)` and `.set_recursion_limit(n)` —
  `examples/src/bin/runtime_limits.rs` (whole file). These throw a Rust-only `RuntimeLimit` error
  that JS `try/catch` cannot intercept (comment at line 24), which is a useful backstop against
  runaway scripts independent of the `job()`/`parallel()` cooperative model.

## 3. Determinism audit

- **Property/key iteration order is deterministic and insertion-ordered** — object shapes store
  keys in a `Vec<(PropertyKey, Slot)>` (`core/engine/src/object/shape/property_table.rs:11-26`,
  `keys()`/`keys_cloned_n()` "Returns all the keys, in insertion order"); the `FxHashMap` alongside
  it is used only for O(1) key→slot lookup, never for iteration. `to_json` itself walks integer
  keys ascending then string keys in insertion order (`core/engine/src/value/conversions/serde_json.rs:153-197`).
  No HashMap-iteration-order hazard here.
- **`Math.random` is a direct, unhookable entropy source** — `rand::random::<f64>()`,
  `core/engine/src/builtins/math/mod.rs:786`. Must be deleted/overridden by the host (§2) before
  running any script tally-flow intends to replay; there is no engine flag.
- **`Date`/wall-clock is fully engine-mediated through the `Clock` trait** (§2) and is therefore
  safe *if* the host supplies a deterministic `Clock`; the default `StdClock`
  (`core/engine/src/context/time.rs:161`) is real wall-clock and must not be used for replay.
- **GC is allocation-threshold-triggered, not time-triggered**: `core/gc/src/lib.rs:55-56,189-197`
  — collection fires when `bytes_allocated > threshold`, a pure function of the allocation
  sequence the script (plus host-registered natives) produces. Given deterministic script inputs
  and deterministic native-fn behavior, GC trigger points are themselves deterministic — this is
  *not* a hazard by itself.
- **`WeakRef`/`FinalizationRegistry` are the one place GC becomes observable**, and they are
  registered unconditionally as globals with no feature gate
  (`core/engine/src/builtins/mod.rs:36,70,111,314,318,442,447` — `weak::WeakRef`,
  `finalization_registry::FinalizationRegistry`, both always `global_binding`'d). Per spec these
  callbacks are inherently implementation-defined/non-observable-in-principle (engines are never
  required to collect anything), so this is a hazard independent of whether Boa's specific GC
  heuristic is itself deterministic: **tally-flow should delete `WeakRef` and
  `FinalizationRegistry` from the global object alongside `Date`/`Math.random`**, same mechanism
  as §2.
- **The real nondeterminism risk is at the `JobExecutor` boundary, not inside the engine**: Boa
  only guarantees FIFO delivery of whatever the host enqueues (§2, `job.rs` `PromiseJob` doc
  comment); the *order in which concurrent `NativeAsyncJob`/daemon futures resolve* is entirely a
  host-executor decision, and the reference `Queue` implementations in
  `examples/src/bin/tokio_event_loop.rs`, `smol_event_loop.rs`, and `module_fetch_async.rs` all use
  `futures_concurrency::FutureGroup` + `poll_once(group.next())`, i.e. "first future to finish in
  real wall-clock time wins." `module_fetch_async.rs:26-28`'s own comment says this out loud. A
  deterministic tally-flow `JobExecutor` must impose its own ordering policy over raw future
  completion (canonical-by-submission-id draining per tick, or completion-order recording +
  replay) rather than reusing these examples' race-to-first pattern verbatim.
- **Unhandled-rejection tracking is a no-op by default**: `HostHooks::promise_rejection_tracker`
  (`core/engine/src/context/hooks.rs:106-113`) has an empty default body ("The default
  implementation... is to return unused"). If tally-flow wants deterministic, observable behavior
  on an unhandled rejection (e.g. abort the workflow run with a specific error) it must override
  this hook explicitly; left at default, unhandled rejections are silently swallowed by the engine
  itself (the host executor may still separately log/`eprintln!` on job errors, as the examples
  do, but that's app-level, not spec-level, tracking).
- **`eval()` / dynamic string execution and `Function` constructor** are reachable from script by
  default and are a determinism/sandboxing concern only insofar as they can construct new code at
  runtime — not inherently nondeterministic themselves, but worth disabling via
  `HostHooks::ensure_can_compile_strings` returning an error (worked example inline in
  `core/engine/src/context/hooks.rs:20-53`) if tally-flow wants to forbid runtime code generation
  entirely as a determinism/auditability hardening measure (not required for determinism per se,
  but a reasonable belt-and-suspenders item since `eval`/`Function` could be used to probe for
  environment differences).
- **No engine-level source of thread/OS timing jitter leaks into script-visible state** other than
  the two items above (`Date`, `Math.random`) plus whatever the host's own native functions choose
  to expose (e.g., a job's result payload containing a real wall-clock timestamp from the daemon
  — that's a tally-flow application-level determinism concern, not a Boa one).

## 4. Error-reporting quality

- Parse/compile errors carry line and column and read as plain English, no ANSI/no multi-line
  caret diagram. Real examples pulled directly from Boa's own test suite (`assert_native_error`
  calls, i.e. these are the literal strings `JsError::to_string()` produces):
  - `core/engine/src/tests/control_flow/mod.rs:12`: `"illegal break statement at line 1, col 1"`
  - `core/engine/src/tests/function.rs:275`: `"Duplicate parameter name not allowed in this context at line 2, col 12"`
  - `core/engine/src/tests/operators.rs:151`: `"Invalid left-hand side in assignment at line 1, col 1"`
  - `core/engine/src/tests/mod.rs:385`: ``"expected token 'identifier', got '=' in identifier parsing at line 1, col 7"``
  - `core/engine/src/builtins/json/tests.rs:315`: `"expected value at line 1 column 1"` (note:
    `JSON.parse` errors use "column" spelled out; syntax errors elsewhere use "col" — a real,
    minor inconsistency to be aware of if tally-flow parses these strings rather than treating them
    as opaque diagnostics).
- Runtime (post-parse) errors: `JsError`/`JsNativeError` `Display` impl
  (`core/engine/src/error/mod.rs:856-871, 929-940`) appends `"\n    at {entry}"` per shadow-stack
  frame when a backtrace was captured, where each frame renders as either
  `"<function> (native at file:line:col)"` or `"<function> (path:line:col)"`
  (`core/engine/src/vm/shadow_stack.rs:80-134`, `DisplayShadowEntry`). Native-fn frames need the
  `native-backtrace` Cargo feature (`core/engine/Cargo.toml:92`) to carry real Rust source
  locations; bytecode frames always carry JS-source line/col via the `SourceInfo`/position map.
- Native functions can pull a full call stack on demand: `context.stack_trace()` returns
  `Vec<&CallFrame>`, each with `.position() -> CallFrameLocation { function_name, path, position:
  Option<Position(line, col)> }` — worked, asserted example in `core/engine/src/vm/tests.rs:45-107`
  (`fn position()`), which registers a native `check_stack()` callable and calls it from four
  nested contexts (arrow fn → `eval` → named fn → top level `<main>`), asserting the exact
  `(function_name, path, line, col)` tuple at every depth. This is the mechanism to use if
  tally-flow wants to attach a JS call-stack snapshot to a `job()` failure for user-facing
  diagnostics.
- `RuntimeLimit` errors (loop/recursion caps, §2) are explicitly documented as **not catchable by
  JS `try/catch`** (`examples/src/bin/runtime_limits.rs:24`) — they only ever surface as a Rust
  `Err` from `context.eval`/`Script::evaluate`, which is useful for tally-flow (a runaway script
  can't swallow its own kill signal) but means these are a distinct error channel from ordinary
  thrown `Error` objects and need separate handling in the runner's error-to-daemon-response
  mapping.

## 5. Do-NOT-copy list

- **`examples/src/bin/module_fetch_async.rs`'s `HttpModuleLoader`** — fetches modules over the
  network with `reqwest::get` inside `load_imported_module`. This is the literal opposite of
  tally-flow's "network deliberately absent" requirement; do not adapt this pattern for any
  loader, even an ostensibly "safe" one — module loading should stay on `IdleModuleLoader` (the
  default) or a fully in-memory `MapModuleLoader`.
- **`boa_runtime`'s default feature set** (`core/runtime/Cargo.toml:41-46`): `default = [...,
  "fetch", "url"]` — the companion "example runtime" crate pulls in real network fetch
  (`reqwest`) by default. Do not depend on `boa_runtime` with default features for tally-flow;
  either depend on `boa_engine` alone (recommended — nothing in the lift list above requires
  `boa_runtime`) or depend on `boa_runtime` with `default-features = false` and hand-pick only
  genuinely safe pieces (e.g. `Console`, used purely for `examples/src/bin/tokio_event_loop.rs:201-204`-style
  `console.log`) — and even then, prefer implementing `log()` directly against the four-fn host
  API rather than pulling in a whole borrowed console implementation.
- **`boa_wintertc`'s `timers`/`fetch` modules** (`core/wintertc/src/timers`, `core/wintertc/src/fetch`)
  — same reasoning: this crate exists specifically to bolt on `setTimeout`/`fetch`-shaped WinterTC
  web APIs, exactly what tally-flow must not expose. Do not adopt wholesale.
- **The `poll_once(group.next())` "first-future-wins" `JobExecutor` pattern** in
  `tokio_event_loop.rs`, `smol_event_loop.rs`, and `module_fetch_async.rs` — copy the *shape*
  (four job-type queues, `run_jobs_async` loop structure) but not the ordering semantics; as
  documented in §3, this pattern resolves whichever daemon future happens to finish first in real
  time, which is exactly the non-determinism tally-flow must eliminate. Treat these examples as
  "here is the plumbing to route four kinds of jobs," not "here is how to keep replay
  deterministic."
- **`cli/`'s `fast-allocator` feature (jemalloc/mimalloc)** — a benchmarking/CLI convenience
  (`cli/Cargo.toml:45-49`), irrelevant to an embedded library use and pulls in a C allocator; don't
  carry it into a workspace crate whose whole pitch is "pure Rust, no C deps."
- **Relying on `HostHooks::promise_rejection_tracker`'s default no-op** for anything — as noted in
  §3, silently doing nothing on unhandled rejections is Boa's default, not a safe default for a
  workflow runner; don't leave it unoverridden and assume rejections are being surfaced.
- **Assuming `NativeFunction::from_async_fn` can capture arbitrary state via closure** — the `F:
  Copy` bound (`core/engine/src/native_function/mod.rs:199`) rules out capturing e.g. an `Rc<Socket>`
  or `Rc<RefCell<..>>` directly in the async fn; don't fight this with `unsafe` closure captures
  (`from_closure`/`from_closure_with_captures` exist for the sync case and carry real UB risk
  if misused per their own doc caveat, `core/engine/src/native_function/mod.rs:122-128`) — instead
  thread shared state through `context.realm().host_defined()` (§2) or a `Copy` handle/index.

## 6. Maturity / packaging

- **Version at clone**: workspace `version = "1.0.0-dev"` (`Cargo.toml:35`, unreleased, main
  branch HEAD as of 2026-07-24).
- **Last published, stable crate**: `boa_engine` **0.21.1** on crates.io (`max_version` /
  `max_stable_version` = `0.21.1` per the crates.io API, checked live). The `CHANGELOG.md` confirms
  the JobExecutor/NativeAsyncJob/Clock-trait machinery cited throughout this brief already shipped
  in **v0.21.0 (2025-10-21)** — see `CHANGELOG.md:15` ("Revamp `JobQueue` into `JobExecutor` and
  introduce `NativeAsyncJob`", PR #4118), `:26` (async-fn context-capture signature change, #4215),
  `:34` (`run_jobs_async` plain-async, #4331), `:36` (`AsyncFnOnce` ctors, #4333), `:127`
  ("Implement an internal time type and Clock trait", #4149). **Practically: tally.nix can pin to
  the published `boa_engine = "0.21.1"` crate rather than a git dependency on main** — none of the
  APIs in this brief are dev-branch-only, though it's worth re-verifying exact line numbers against
  0.21.1's tag rather than this main-branch clone before implementation.
- **MSRV**: `rust-version = "1.91.0"` (`Cargo.toml:36`).
- **Pure Rust / no C deps confirmed for the runtime path**: `boa_engine`'s only
  x86_64-linux-gnu-conditional native dependency is `jemallocator`, and it's scoped to
  `[target.x86_64-unknown-linux-gnu.dev-dependencies]` (`core/engine/Cargo.toml:207-208`) — i.e. a
  benchmark-only dev-dependency, not part of the published library's dependency graph. No
  `openssl`/`native-tls` anywhere in any workspace `Cargo.toml` (checked via grep across the whole
  clone). ICU4X (`intl`/`intl_bundled` features) is pure Rust. The repo's own `flake.nix` dev shell
  lists `openssl` in `buildInputs`, but that's a dev-shell convenience (likely for `reqwest`'s
  default TLS backend used by dev-only examples/tests, e.g. `module_fetch_async.rs`'s
  `reqwest::get`), not a build requirement of `boa_engine` itself.
- **test262 conformance**: README claims "more than 90% of the latest ECMAScript specification"
  (`README.md:20`) and points to a live dashboard at `https://boajs.dev/conformance`
  (`README.md:103-106`); that dashboard is a client-rendered SPA and did not yield a scrapable
  number via `curl` — **exact current pass percentage: unknown from this clone**, only the ">90%"
  README claim is verifiable offline.
- **Notable open upstream issues relevant to embedding tally-flow**:
  - [#3442](https://github.com/boa-dev/boa/issues/3442) "Cancel/Interrupt active evaluation" (open
    since 2023-10-30) — there is **no external interrupt mechanism** for a synchronous
    `while(true){}`-style busy loop other than the pre-configured `runtime_limits` counters (§2);
    tally-flow cannot assume it can cancel a stuck script from another thread mid-evaluation.
  - [#5442](https://github.com/boa-dev/boa/issues/5442) "Parser rejects async arrow function in
    export default position" — a live parser gap around async syntax, worth a smoke-test against
    tally-flow's actual script dialect.
  - [#4524](https://github.com/boa-dev/boa/issues/4524) "Audit public APIs" — upstream is
    actively reworking/stabilizing embedder-facing surface toward a 1.0; expect some API drift if
    tracking main instead of pinning to 0.21.1.
  - [#5420](https://github.com/boa-dev/boa/issues/5420) "No public weak reference API for
    `JsObject`" — tangential, but confirms `WeakRef`'s host-side story is still in flux, reinforcing
    the §3/§5 recommendation to just delete the global rather than try to use it.
- **nixpkgs packaging**: nixpkgs ships a `boa` package (`nix eval nixpkgs#boa.{pname,version,meta.homepage}`
  → `pname="boa"`, `version="0.21.1"`, `homepage="https://github.com/boa-dev/boa"`,
  description "Embeddable and experimental Javascript engine written in Rust") — this is the
  `boa_cli` binary at the same 0.21.1 release as the current crates.io publish. There is no
  separate nixpkgs attribute for the `boa_engine` *library* specifically (unsurprising — Rust
  libraries are normally consumed via Cargo/crates2nix/naersk-style vendoring rather than as a
  nixpkgs derivation), so tally.nix should expect to pull `boa_engine` through its Rust
  dependency-vendoring path (e.g. `crane`/`naersk`/`buildRustPackage`'s `cargoLock`), not through a
  nixpkgs top-level attribute.
