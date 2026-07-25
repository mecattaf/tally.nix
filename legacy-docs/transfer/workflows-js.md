# Transfer brief: workflows.js (Claude Code Workflow tool)

Source: the Workflow tool dialect built into Claude Code (no repo to clone — this brief is
written by the model that executes that dialect natively and is the authoritative digest).
Role in tally-flow: **the authoring-discipline donor**. tally-flow keeps the shape of this
format (deterministic plain JS around opaque work units) and swaps the primitive
(`agent(prompt)` → `job(spec)`) and the substrate (session journal → witness ledger).
Claude models are RL-trained on this idiom; keeping the shape is what makes flow scripts
cheap to author correctly.

## 1. The dialect surface, as shipped

```js
export const meta = { name, description, phases: [{title, detail}] }  // PURE literal, no
                                                                      // computed values
agent(prompt, {label?, phase?, schema?, model?, effort?, isolation?, agentType?}) → Promise
parallel(thunks: Array<() => Promise>) → Promise<any[]>   // BARRIER; a thrown thunk
                                                          // resolves to null, never rejects
pipeline(items, ...stages) → Promise<any[]>               // NO barrier between stages;
    // each stage gets (prevResult, originalItem, index); a throwing stage drops the item
    // to null and skips its remaining stages
log(message)                                              // narration to the operator
phase(title)                                              // progress grouping
args                                                      // injected input value, verbatim
budget: { total, spent(), remaining() }                   // shared token pool; hard ceiling
```

- `schema` option forces the worker to return validated structured output (JSON Schema,
  retry-on-mismatch at the tool-call layer). The worker's final text IS the return value —
  workers are told they report data, not prose for a human.
- Concurrency: per-workflow cap min(16, cores-2); excess queues. Lifetime agent cap 1000.
  Single fan-out call cap 4096 items — over-cap is an explicit error, never truncation.
- Nesting: one level of sub-workflow (`workflow()`), sharing budget/cap/abort.

## 2. Determinism law, as shipped

`Date.now()`, `Math.random()`, argless `new Date()` **throw** inside scripts — explicitly
"because they would break resume." Timestamps are passed in via `args` and stamped after the
workflow returns. Randomness is emulated by varying prompt/label by index. No filesystem or
Node API access. This is the Durable Functions orchestrator discipline, enforced by omission.

## 3. Resume semantics, as shipped (the load-bearing precedent)

`resumeFromRunId`: re-launch the script; the **longest unchanged prefix of `agent()` calls
(prompt, opts)** returns cached results instantly from a journal (`journal.jsonl` records
each agent's actual return value); the first edited/new call and everything after runs live.
Same script + same args → 100% cache hit. This is exactly the replay-through-memoization
model tally-flow rebuilds on the witness chain, with two deltas:
- the memo key becomes a deterministic per-call-site submission key, not (prompt, opts)
  equality against a session journal;
- the memo store becomes the daemon's durable rows + witnesses, so replay survives any
  process death, not just an in-session resume.

## 4. Lift list

1. **The authoring model**: orchestration is a disposable, reviewable, content-hashed
   artifact an LLM writes per task. Keep the JS shape, the `meta` prelude, the combinator
   names, the "workers do their own I/O and return compact structured summaries" idiom.
2. **`meta` as a validation surface**: pure-literal meta parsed before execution. tally-flow
   extends it (`meta.pools`, `meta.args` schema) for eval-time validation in the Nix module.
3. **The determinism bans**, verbatim, plus the error style (throwing with an explanation at
   the banned call site, not silently returning wrong values).
4. **`pipeline` no-barrier semantics** and the (prev, originalItem, index) stage signature —
   proven ergonomic for multi-stage fan-out.
5. **Structured-result validation with retry** at the boundary (tally analogue: schema
   validation of a job's structured result before the flow observes it — pi-appliance's
   `validate` stage, generalized).
6. **Budget accounting shape** (`total/spent()/remaining()`, hard ceiling, shared pool) —
   tally analogue: a run-scoped budget pool children draw from.

## 5. Do-NOT-copy list

1. **`agent()` as the primitive** — LLM-only by construction. tally's primitive is
   `job(spec)`; LLM harnesses are adapter sugar (`claude()`, `codex()`, `local()`, `sh()`).
2. **Error-swallowing to `null` in `parallel()`** — right for exploratory research fan-outs,
   wrong as the ONLY mode for a proof-bearing scheduler. FLOW-SPEC must rule explicitly:
   tally-flow's `parallel()` semantics (fail-loud default with per-call opt-out, or
   null-collapse compatibility) is a spec decision, not an inherited accident.
3. **Lazy/queued submission under the concurrency cap** — workflows.js may hold calls in a
   local queue; tally-flow's `parallel()` MUST submit all children to admission eagerly so
   the daemon (not the runner) arbitrates them under pools/priorities. The runner never
   implements its own scheduler.
4. **model/effort/isolation knobs** — replaced by pools/adapter/priority/workspace in
   `job(spec)`.
5. **Session-scoped journal as memo store** — replaced by the witness chain (see §3).
6. **The 5-hour in-session lifetime assumption** — a flow run may span days; nothing in the
   dialect may assume the runner process is the same process across the run.

## 6. One-paragraph mapping for authoring agents

"You are writing a tally flow script. It is workflows.js with these substitutions:
`agent(prompt, opts)` → `job({argv|adapter, pools, priority, evidence, workspace, key, …})`
or the sugar `claude(prompt, opts)` / `codex(prompt, opts)` / `local(prompt, opts)` /
`sh(argv, opts)`; results are witnessed verdicts + structured summaries; `parallel`/
`pipeline`/`log` keep their semantics; Date/Math.random/fs/network are absent; every job you
submit competes in one flat queue under pools and priorities you name explicitly."
