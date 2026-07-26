# tally-flow

`tally-flow` is the deterministic JavaScript runner described by
`docs/FLOW-SPEC.md` sections 9–12. It embeds Boa 0.21.1, validates the flow
dialect before evaluation, and keeps daemon access behind `FlowClient`.

## Script contract

A script starts with a pure-literal `export const meta = { ... }`. The checker
parses that declaration as a module, validates its JSON-compatible literal
shape, applies the determinism lint, blanks only the `export` token to preserve
source positions, and reparses the result as a Script. `args`, `meta`, and
witnessed node results are the script-visible inputs.

The globals are:

- `job(spec, { settle })`
- `claude(prompt, opts)`, `codex(prompt, opts)`, `local(prompt, opts)`, and
  `sh(argv, opts)`
- `parallel(thunks, { settle })` and `pipeline(items, ...stages, { settle })`
- `members(selector, opts)`, `quorum(declaration)`, `attributed(member, value)`,
  `dissent(declaration)`, and `repairKey(member)`
- `log(value)`

`job()` rejects with stable `.code` values for admission, terminal, schema,
key, loop, and replay failures. Settle mode returns the failed `NodeResult`
instead. Every report carries a source position; uncaught exceptions also
carry Boa's available shadow-stack frames and the current ordinal frontier.

The runner assigns ordinals synchronously, submits in ordinal order, and makes
terminal facts observable in ascending `witnessSeq` order. Replayed payload
hashes and the run's pinned script hash are checked before execution can pass a
divergence point. No runner journal or state file exists.

Every `claude()`, `codex()`, and `local()` node carries a host-stamped
`orchestration.promptRevision` equal to `sha256:<hex>` over the exact UTF-8 bytes
of its resolved prompt. When the selected adapter configuration declares
`skillBundle`, the host hashes that configured content with the same construction;
when it declares `skillRevision`, the stable identifier is copied verbatim.
The fields are absent when unavailable and cannot be supplied by script options.
Re-execution derives them from the same prompt and adapter configuration; a changed
prompt also changes the structured brief and therefore reaches the existing
`replay-divergence` check through `payloadHash`.

The executable exposes:

```text
tally flow check SCRIPT [--args JSON] [--catalog PATH]
tally flow run SCRIPT --args JSON --max-nodes N [--catalog PATH] [--flow-run-id ID]
```

The executable binds `FlowClient` to one multiplexed daemon connection. Every
node uses full-mode admission; live rows attach and await, terminal rows replay
their witnessed result, and a daemon restart replaces the connection and
reissues the idempotent operation. When run as a tally job, the runner derives
`flowRunId` and child ancestry from its job identity and submits children as
`source=orchestrator`.

## Engine boundary

Boa was retained over the rquickjs contingency because its host hooks expose
compile-string rejection and promise rejection tracking, and its custom
`JobExecutor` makes witness-ordered observation an explicit host policy.
`Date`, `WeakRef`, and `FinalizationRegistry` are deleted; `Math.random`
throws; timers are never registered; eval/Function compilation is denied; and
loop and recursion limits are set. Boa does not preserve the original thrown
frame when an ordinary exception crosses an async-promise rejection boundary,
so that case reports the deterministic promise rejection call site captured by
the host hook. Synchronous exceptions retain the full Boa shadow stack.

## Data plane

**Tally moves no bytes between hosts.** Cross-host handoff is via the workspace
repo (commits/branches) or the deployed artifact store (attic push /
substitute; R2 for public artifacts). Flow scripts MUST NOT assume a shared
filesystem across pools on different hosts. Evidence records reference
artifacts; they never carry them.
