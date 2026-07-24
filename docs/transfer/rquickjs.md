# Transfer brief: rquickjs (fallback embedded JS engine)

Source: `git clone --depth 1 https://github.com/DelSkayn/rquickjs` into
`~/Downloads/rquickjs` (HEAD = tag `v0.12.1`, matches `Cargo.toml` version, so this is a
released state, not an arbitrary dev commit). The `sys/quickjs` git submodule
(`https://github.com/quickjs-ng/quickjs.git`) was additionally fetched
(`git submodule update --init --depth 1`, pinned commit `fd0a0210`, which is quickjs-ng
`0.15.1` per `sys/quickjs/quickjs.h:1410-1412`) — clone alone leaves that directory empty.
All file paths below are relative to `~/Downloads/rquickjs` unless marked `quickjs-ng:`.

## 1. Role

**Fallback engine** — used only if Boa disqualifies (a sibling brief covers Boa). rquickjs is
not a pure-Rust interpreter: it is a safe Rust binding over the C quickjs-ng engine, compiled
from bundled C sources via the `cc` crate. That C dependency is the whole reason it's fallback
rather than primary — see §5.

## 2. Lift list

All citations are `path:line` in the clone unless stated otherwise.

### Async support (futures/tokio integration, AsyncContext/AsyncRuntime)

- `core/src/runtime/async.rs:77-82` — `AsyncRuntime` wraps `Arc<Mutex<InnerRuntime>>`; all
  mutation goes through an `async_lock::Mutex`, so the engine itself stays single-threaded
  even when the `parallel` feature makes the handle `Send`/`Sync` (`:86-97`).
- `core/src/runtime/async.rs:277-309` (`execute_pending_job`) and `:312-360` (`idle`) — the
  two host-driven pumps: run one queued job vs. drain everything (jobs + spawned futures)
  until quiescent. `idle()` is the one tally-flow's runner loop would call per script tick.
- `core/src/runtime/async.rs:365-367` — `AsyncRuntime::drive()` returns a `DriveFuture` you
  `tokio::spawn`/`spawn_local` once; it keeps polling spawned futures even when no script code
  is currently running. Demonstrated end-to-end in the `drive` test at `:439-471`.
- `core/src/context/async.rs:218-224` — `AsyncContext::async_with(f: AsyncFnOnce(Ctx) -> R)`:
  the entry point for running script code that itself needs to `.await` JS promises.
- `core/src/context/ctx.rs:415-423` — `Ctx::spawn(future)` pushes a Rust future onto the
  runtime's internal executor (`Opaque` spawner); this is how a host `job()`/`parallel()`
  future gets driven without blocking the interpreter thread.
- `core/src/runtime/async.rs:643-666` (`ensure_types_are_send_sync` test) and the
  `async_test_case!` macro at `:370-408` — proves the crate's own test harness runs
  `AsyncRuntime` on both a multi-thread (`parallel` feature) and current-thread
  (`tokio::task::LocalSet`, no `parallel`) tokio runtime. **The non-`parallel` current-thread
  path is the one to prefer** — see the maturity caveat in §5 about `parallel` being
  explicitly labeled experimental by upstream.
- Real usage pattern from the crate's own tests, `core/src/runtime/async.rs:545-615`
  (`recursive_spawn_from_script`): registers a `setTimeout` host function that internally
  calls `ctx.spawn(async move { tokio::time::sleep(...).await; callback.call(()) })`, then
  evaluates a script that calls `setTimeout` recursively and awaits a `Promise` returned from
  `eval_with_options` with `EvalOptions{ promise: true, .. }` — this is close to the exact
  shape tally-flow needs for `job()`/`pipeline()` returning to script as awaitable promises.

### Registering host functions (sync + async)

- `core/src/value/function/types.rs:68-70` — `Async<T>` wrapper marks a closure as
  future-returning; `MutFn`/`OnceFn` wrap `FnMut`/`FnOnce` closures (borrow-checked at call
  time, returning `Error::FunctionBorrow` on conflict, not a panic).
- `core/src/value/function/into_func.rs:36-55` — the `IntoJsFunc` impl for `Async<Fun>` where
  `Fun: Fn(..) -> Fut`, `Fut: Future<Output = R>`: wraps the returned future in
  `Promised(fut)` (`core/src/value/promise.rs:233`) so calling the host function from script
  returns a native `Promise` immediately and resolves it when the Rust future completes.
  Combined with `MutFn`/`OnceFn` variants at `:80-100` and `:124-144` for stateful/one-shot
  async host functions.
- `core/src/value/function.rs:47-59` (`Function::new<P, F>(ctx, f)`) — the constructor;
  `f: F where F: IntoJsFunc<'js, P>`. Plain closures, `Func::from(closure)`
  (`core/src/value/function/types.rs:24-31`), or `Async(closure)` all go through this.
- `core/src/value/promise.rs:44-73` (`Promise::wrap_future`) — the lower-level primitive:
  takes any `Future<Output: IntoJs>`, creates a JS promise/resolve/reject triple via
  `ctx.promise()`, and calls `ctx.spawn(...)` to drive it. This is what `job()` would call
  directly if it needs more control than the `Async<T>` sugar gives.

### Globals control — can Date/Math.random be removed or overridden?

- **Date: yes, fully absent by construction.** `core/src/context/builder.rs:54-78` defines
  each ECMAScript builtin group as its own opt-in `Intrinsic` marker type
  (`Date`→`JS_AddIntrinsicDate`, `Json`, `Proxy`, `MapSet`, `TypedArrays`, `Promise`,
  `Performance`, `WeakRef`, `RegExp`, `Eval`). `AsyncContext::custom::<I: Intrinsic>` at
  `core/src/context/async.rs:145-156` calls only `JS_AddIntrinsicBaseObjects` (Object,
  Function, Array, String, Number, Boolean, Math, Error, Symbol, Reflect, iterator protocol)
  plus whatever `I` you name — `AsyncContext::base()` (`:138-140`) passes `intrinsic::None`
  (`= ()`, builder.rs:81), so **`Date` never exists as a global unless you explicitly add the
  `Date` intrinsic**. `intrinsic::All` (`builder.rs:84-96`) is the opposite extreme.
- **Math.random: not independently gate-able (Math is part of BaseObjects, not a separate
  intrinsic), but overridable, and it needs to be** — it is wall-clock seeded.
  `quickjs-ng: quickjs.c:47836` (`js_random_init`): `ctx->random_state =
  js__gettimeofday_us()` at context creation. `quickjs.c:47842-47850` (`js_math_random`) then
  runs a deterministic xorshift64* PRNG (`quickjs.c:47822-47831`) off that seed — so calls
  *within* one script run are reproducible relative to each other, but the seed itself differs
  run to run. The `random` property is defined with `JS_CFUNC_DEF` (`quickjs.c:47918`), i.e.
  standard writable+configurable function-property attributes, so
  `ctx.globals().get::<_, Object>("Math")?.set("random", Func::from(deterministic_rng))?`
  overrides it from Rust. There is no C-level knob to reseed or disable `Math.random`
  directly through rquickjs's Rust API — override, don't rely on absence.
- Mechanism for override in general: `Ctx::globals()` (`core/src/context/ctx.rs:235`) returns
  a plain `Object`, so any global, including ones added by an intrinsic, can be shadowed with
  `.set(name, value)` after context creation, same as JS.

### JSON/serde interop

- **No `serde` feature exists in this crate at all** (`grep -rn serde Cargo.toml
  core/Cargo.toml` → no hits). Rust↔JS conversion goes through rquickjs's own `IntoJs`/`FromJs`
  traits (manually implemented or via `#[derive]` from `rquickjs-macro`), not serde. If
  tally-flow's `job()` boundary is designed around `serde_json::Value`, that's a translation
  layer to write, not something rquickjs hands you.
- **JSON parse/stringify is available independent of the `Json` intrinsic.**
  `core/src/context/ctx.rs:279-298` (`Ctx::json_parse`) and `:300-380`
  (`json_stringify`/`_replacer`/`_replacer_space`) call `qjs::JS_ParseJSON` /
  `qjs::JS_JSONStringify` directly — the same C entry points the `JSON.parse`/`JSON.stringify`
  JS builtins use, but reachable from Rust without exposing a global `JSON` object to script.
  Doc-tested at `:655-704`. This means: script-visible `JSON` can stay absent (smaller global
  surface, one less thing to audit for determinism) while the host still marshals data to/from
  script via JSON on the Rust side.

### Promise handling driven by host futures

- `core/src/value/promise.rs:152` / `:328` — `Promise::into_future::<T>()` /
  `into_future::<T: FromJs>()` convert a JS `Promise` *into* a Rust `Future`
  (`PromiseFuture`/`MaybePromiseFuture`), for the reverse direction (host awaiting a
  script-produced promise, e.g. `eval_with_options(..., EvalOptions{promise:true,..})?
  .into_future::<Value>().await` as used at `core/src/runtime/async.rs:598-607`).
- `core/src/value/promise.rs:233-` (`Promised<T>`) / `:44-73` (`wrap_future`) — the forward
  direction (Rust future → JS promise), covered above.
- Everything is glued together by the runtime's job queue: `execute_pending_job`/`idle`
  (`core/src/runtime/async.rs:277-360`) run both quickjs's internal promise-reaction jobs and
  the `Opaque` spawner's polled futures in one loop, so promise resolution and Rust future
  completion interleave through a single pump rather than two separate event loops.

## 3. Determinism audit

QuickJS-ng behaviors that can differ across runs of the *same* script, gathered from
`sys/quickjs/quickjs.c` and the intrinsic-gating mechanism above:

| Source | Evidence | Mitigation available via rquickjs |
|---|---|---|
| `Math.random()` | `quickjs.c:47836` seeds from `js__gettimeofday_us()` (wall clock) at context creation | Override `Math.random` post-creation (writable/configurable, confirmed `quickjs.c:47918`); no reseed API |
| `Date` (`Date.now()`, `new Date()`) | Wall clock, standard JS | Simply don't add the `Date` intrinsic (`builder.rs:57`) — global doesn't exist |
| `performance.now()` | `Performance` is a separate opt-in intrinsic (`builder.rs:75`, `JS_AddPerformance`), wall-clock backed | Don't add the `Performance` intrinsic |
| `WeakRef` / `FinalizationRegistry` | Finalizer timing is tied to GC cycle timing, which is heuristic/threshold-driven (`set_gc_threshold`, `core/src/runtime/async.rs:239-243`), not script-observable-deterministic | Don't add the `WeakRef` intrinsic (`builder.rs:77`) if scripts must never observe GC timing |
| Real host-future completion order (`Promise.race`/`Promise.any` over two host-driven promises) | Not a quickjs defect — `ctx.spawn` futures are polled by whatever OS-thread-scheduled order tokio delivers them in (`core/src/runtime/opaque.rs`, `SchedularPoll` in `runtime/async.rs:297-301`) | This is on tally-flow's own `job()`/`parallel()` host implementation to make deterministic (e.g. resolve in call order, not completion order), not something rquickjs provides |
| Property enumeration order (`Object.keys`, `for..in`, `JSON.stringify` key order) | **Not** a nondeterminism risk — quickjs stores properties in an ordered shape (integer keys ascending, then string keys in insertion order, then symbols), matching the ECMAScript spec's mandated deterministic order | n/a, already deterministic |
| `Array.prototype.sort` | Spec-mandated stable sort since ES2019; quickjs-ng targets that spec | n/a, already deterministic |
| Symbol identity (`Symbol()`) | Each call produces a unique, unforgeable value — deterministic *shape* (same script always creates the same number/order of symbols) but the underlying identity can't be serialized/compared across runs | Only relevant if a script tries to persist a raw `Symbol` across replay boundaries — avoid that at the `job()` boundary |
| `parallel` feature scheduling | Upstream README, "Development status" section: *"Some experimental features like `parallel` may not works as expected. Use it for your own risk."* (verbatim) | Prefer the non-`parallel`, current-thread (`tokio::task::LocalSet`) configuration demonstrated in the crate's own tests (`core/src/runtime/async.rs:396-406`) |

Net: rquickjs/quickjs-ng's *interpreter* is deterministic (refcount GC is explicitly called
out upstream as chosen partly for deterministic behavior — root README.md, "Main features of
QuickJS" list, `sys/quickjs/README.md` is just the fork blurb, the determinism claim is in
rquickjs's own `README.md`: *"Garbage collection using reference counting (to reduce memory
usage and have deterministic behavior) with cycle removal."*). The nondeterminism surface for
tally-flow is exactly the two wall-clock-backed globals (`Date`, `Math.random`, optionally
`performance.now()`) plus GC-timing-observable APIs (`WeakRef`/`FinalizationRegistry`) — all
of which are either absent-by-default (opt-in intrinsics) or overridable from Rust.

## 4. Error-reporting quality

- `core/src/value/exception.rs:70-88` — `Exception::message()` / `Exception::stack()` read
  `error.message` / `error.stack` off the thrown object (same as script-side `err.stack`).
  `Debug`/`Display` impls at `:15-22` and `:217-230` include the stack when present.
- Stack traces are built by quickjs-ng's `build_backtrace` (`quickjs-ng:
  sys/quickjs/quickjs.c:7766-7930`) in V8-style text form: one line per frame,
  `` `    at <funcName> (<filename>:<line>:<col>)` `` for JS frames
  (`quickjs.c:7877-7891`), `` `    at <funcName> (native)` `` for native/host frames
  (`quickjs.c:7896`), `<anonymous>` when the function has no name (`quickjs.c:7873-7875`).
  Line/column come from `find_line_num` walking the bytecode's line-number table
  (`quickjs.c:7883-7884`) — real per-statement source position, not just per-function.
- `filename` in that trace is whatever name the script was evaluated with —
  `core/src/context/ctx.rs:29-40` (`EvalOptions.filename: Option<String>`, "Filename. Ignored
  when calling eval_file_*"). For a multi-script orchestration runner this matters: giving
  each loaded flow-script file its real path as the eval filename makes stack traces point at
  actual source files instead of a generic `<input>`/`<eval>` tag.
  `EvalOptions.backtrace_barrier` (`ctx.rs:32-33`, flag `JS_EVAL_FLAG_BACKTRACE_BARRIER`) lets
  the host stop a trace from unwinding into frames *before* a given eval — useful to keep
  runner-internal frames out of a script author's error output.
- Real example, doc-tested at `core/src/result.rs:600-615`
  (`CatchResultExt`/`CaughtError`):
  ```rust
  use rquickjs::CatchResultExt;
  if let Err(CaughtError::Value(err)) = ctx.eval::<(),_>("throw 3").catch(&ctx) {
      assert_eq!(err.as_int(), Some(3));
  }
  ```
  For an actual `Error` instance (not a bare thrown value), the same `.catch(&ctx)` yields
  `CaughtError::Exception(ex)`, and `ex.stack()` gives the `at ... (file:line:col)` text above.
  *Caveat: this environment has no Rust toolchain available (`cargo`/`rustc` not on `$PATH`),
  so this is a source-verified example, not one I additionally ran and captured live output
  for.* The spec author should run a throwing script through `eval_with_options` with a real
  `filename` set and paste the live `.stack()` output before finalizing this section.

## 5. Packaging cost

- **C dependency, and which fork.** `sys/quickjs` is a git submodule pointing at
  `https://github.com/quickjs-ng/quickjs.git` (`.gitmodules:1-3`), pinned at commit
  `fd0a0210` = **quickjs-ng 0.15.1** (`sys/quickjs/quickjs.h:1410-1412`). This is the
  actively-maintained **quickjs-ng** fork (by Ben Noordhuis and Saghul, per
  `sys/quickjs/README.md`), not Bellard's original dormant quickjs.
- **Build mechanism.** `sys/build.rs:143,205-245` copies 4 `.c` files + headers into
  `$OUT_DIR` and compiles them with the `cc` crate into `libquickjs.a` — a bundled static-lib
  build, no `pkg-config`/system-quickjs lookup path exists. Requires a working C compiler at
  build time (any `nixpkgs` `stdenv` provides one) — no Nix-specific patching needed for that
  part.
- **Bindgen is *not* required for the common Nix target.** `sys/build.rs:256-293` (the
  non-bindgen path) copies pre-generated bindings from `sys/src/bindings/<target>.rs`;
  `x86_64-unknown-linux-gnu.rs` (and `aarch64-unknown-linux-gnu.rs`,
  `x86_64-unknown-linux-musl.rs`, etc.) already exist in-tree (`sys/src/bindings/` listing).
  The root `README.md`'s platform table confirms `x86_64-unknown-linux-gnu`: shipped bindings
  ✅, tested ✅, supported by quickjs ✅. So a crane flake targeting typical
  x86_64-linux/aarch64-linux does **not** need `libclang`/`bindgen`/`clang` in
  `nativeBuildInputs` — just a C compiler. The `bindgen` Cargo feature exists only as a
  fallback for unlisted platforms.
- **Submodule content is already flattened into the published crate — verified, not assumed.**
  Downloaded the actual `rquickjs-sys-0.12.1.crate` tarball from
  `static.crates.io/crates/rquickjs-sys/rquickjs-sys-0.12.1.crate` and listed it: it contains
  `rquickjs-sys-0.12.1/quickjs/quickjs.c`, `quickjs.h`, `libregexp.c`, `dtoa.c`,
  `quickjs-c-atomics.h`, `cutils.h`, etc. directly — the submodule's C sources are bundled as
  regular files in the crates.io package (cargo's publish step folds submodule content in,
  and CI's publish workflow explicitly checks out `submodules: true` before running
  `cargo publish`, `.github/workflows/*.yml:569-588`). **Practical consequence: if tally.nix
  depends on `rquickjs`/`rquickjs-sys` from crates.io (the normal case for a crane
  `cargoVendorDir` built from the registry), no submodule-fetching logic is needed in the
  flake at all** — the vendored crate already has the C sources. Submodule fetching would
  only become a concern if tally.nix pinned rquickjs via a git/path source instead of
  crates.io.
- **WASM/WASI path downloads a toolchain at build time** (`sys/build.rs:13-92`,
  `download_wasi_sdk` via `curl`) — irrelevant unless targeting `wasm32-wasip1`/`wasip2`;
  flag it only so nobody is surprised by network access during a `wasm32-wasi` build.
- **Maturity/version.** Cloned HEAD = tag `v0.12.1`, and that matches the `Cargo.toml`
  version — this is a tagged release, not a mid-development commit. Release history goes back
  through `v0.4.0-beta.2`; `CHANGELOG.md` shows `[0.12.1] - 2026-06-29` and `[0.12.0] -
  2026-05-26`, i.e. active, roughly-monthly maintenance. Upstream's own characterization
  (root `README.md`, "Development status"): *"This bindings is feature complete, mostly
  stable and ready to use. The error handling is only thing which may change in the future.
  Some experimental features like `parallel` may not works as expected."* Both rquickjs
  (`Cargo.toml:9`, `license = "MIT"`) and quickjs-ng (`sys/quickjs/LICENSE`) are MIT.

## 6. Boa-vs-rquickjs decision inputs

Factual axes only; rquickjs cells filled from this brief, Boa cells left for the spec author
(sibling brief covers Boa in the primary-candidate spot).

| Axis | rquickjs (fallback) | Boa (primary) |
|---|---|---|
| Implementation | Rust binding over bundled C (quickjs-ng 0.15.1) | *(fill in)* |
| Async/tokio integration | Native: `AsyncRuntime`/`AsyncContext`, `ctx.spawn`, `Promised<T>`, `Promise::into_future` — all first-class in the `futures` feature | *(fill in)* |
| Host function registration (sync) | `Function::new`/`Func::from(closure)` via `IntoJsFunc` | *(fill in)* |
| Host function registration (async) | `Async(closure)` wrapper, closure returns `Future`, auto-wrapped in a JS `Promise` | *(fill in)* |
| Can `Date` be fully absent | Yes — opt-in `Intrinsic` marker type, not added by `base()`/`custom()` unless named | *(fill in)* |
| Can `Math.random` be fully absent | No — `Math` is part of non-optional `BaseObjects`; must override the property post-creation instead | *(fill in)* |
| `Math.random` determinism risk if untouched | Wall-clock seeded PRNG (`js__gettimeofday_us`), deterministic *within* a run, not *across* runs | *(fill in)* |
| JSON interop | No serde feature; native `IntoJs`/`FromJs`; `Ctx::json_parse`/`json_stringify` work without exposing global `JSON` to script | *(fill in)* |
| Stack trace quality | V8-style `at fn (file:line:col)`, real per-statement line/col via bytecode line table; `EvalOptions.filename` controls the file name shown; `backtrace_barrier` fences host frames out | *(fill in)* |
| C toolchain dependency | Yes — must compile bundled C via `cc` crate; needs a C compiler in the Nix build closure | *(fill in, presumably: none — pure Rust)* |
| Bindgen/libclang needed for Nix (x86_64/aarch64-linux) | No — pre-generated bindings shipped for those targets | *(fill in, presumably: n/a)* |
| Submodule/vendoring concern for crane | None if consumed from crates.io — C sources are flattened into the published tarball (verified) | *(fill in, presumably: n/a)* |
| Upstream maturity signal | Tagged release (v0.12.1), ~monthly cadence, upstream self-describes as "feature complete, mostly stable"; explicitly flags `parallel` feature as experimental | *(fill in)* |
| License | MIT (both rquickjs and quickjs-ng) | *(fill in)* |
