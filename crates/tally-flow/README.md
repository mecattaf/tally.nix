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
- `drv(spec, { settle })`
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

`drv()` is the store-native primitive:

```js
await drv({
  drvPath: "/nix/store/00000000000000000000000000000000-package.drv",
  outputs: [
    {
      name: "out",
      path: "/nix/store/11111111111111111111111111111111-package"
    }
  ]
});
```

The host sorts outputs by name, rejects malformed or duplicate store paths, and
maps the node to `nix build --no-link <drvPath>^*`. Its pool is always the
reserved, automatically declared `build` pool. Its dedup key is
`drv:<drvPath>`, and its task UUID is derived deterministically from that same
seed. A missing output admits an ordinary build row and leases one build slot.
When every output is already valid in the Nix store, the daemon skips the row
and lease, then appends a cheap witness with `substituted` disposition, the
derivation, and `store:<path>` evidence for every output.

> If a node is hermetic and replay-stable, it should be a derivation and Nix
> memoizes it. `job()` exists only for the impure — and everything impure gets
> witnessed.

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
