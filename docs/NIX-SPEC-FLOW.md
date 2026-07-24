# NIX-SPEC-FLOW — declarative surface for the flow era

Companion to `docs/FLOW-SPEC.md`; extends `docs/NIX-SPEC.md` and the existing module layer
(`nix/modules/common.nix`, currently 2016 lines). Everything here follows the module's
established idioms: option types built beside `mkPoolType`/`mkProducerType`
(`common.nix:1312-1455`, `common.nix:1226-1276`), `_tallyAssertions` with actionable
messages, rendering through `mkRuntimeConfig` (`common.nix:1855-1867`), and eval-time
validation through the real binary via the `mkCheckedConfig` pattern
(`common.nix:1962-1980`). Style exemplars: `docs/transfer/nix-module-style.md`.

## 1. `services.tally.flows.<name>`

New `attrsOf mkFlowType` alongside pools/executors/producers/adapters in `mkOptions`
(`common.nix:1457-1627`):

| option | type | default | notes |
|---|---|---|---|
| `script` | path | — (required) | store path; the content hash of this path IS the flow's `scriptHash` identity |
| `onCalendar` | nullOr str | `"daily"`-style systemd expr or `null` | `null` = registered but not calendar-fired (gh/manual only) |
| `args` | attrs (JSON-serializable) | `{}` | validated against the script's `meta.argsSchema` at eval time |
| `priority` | enum interrupt/high/medium/low | `"low"` | the RUNNER's priority; nodes carry their own |
| `runtimeMaxSec` | nullOr positive int | `43200` | runner watchdog |
| `maxNodes` | positive int | `1000` | per-run node backstop (FLOW-SPEC §6.2); must be ≥ script's own `meta.maxNodes` if that is set — assertion otherwise |
| `catalog` | nullOr path | `null` | pooled-selector catalog JSON (§4); required if the script's meta declares selector use |
| `budgetPool` | nullOr str | `null` | run-scoped budget pool name; must exist in `pools` |
| `extraEnv` | attrsOf str | `{}` | non-`TALLY_`-prefixed (same rule as adapters, `common.nix` env assertion) |
| `credentials` | attrsOf externalPath | `{}` | LoadCredential passthrough, as everywhere |

### Rendering

A flow with `onCalendar != null` renders as a **calendar producer** named `flow-<name>`
through the existing producer machinery (`home-manager.nix:118-145` timer/service pair —
flows are home-manager-surface only, matching the existing rule that producers and meters
render only in the user-daemon module; the NixOS module gains nothing):

```
enqueue = {
  argv      = [ "tally" "flow" "run" "${script}" "--args" (toJSON args) ];
  adapter   = "shell";
  pool      = [ "flow" ];
  priority  = cfg.priority;
  dedupKey  = "flow-<name>-%Y-%m-%d";        # same strftime discipline as existing producers
  runtimeMaxSec = cfg.runtimeMaxSec;
  evidence  = [ "exit:0" ];
  noEnqueue = false;                          # flows REQUIRE enqueue capability
};
```

`noEnqueue = false` is the entire security delta between "comment runs a job" and "comment
runs a graph" — one reviewable boolean (the dotfiles monthly bot already models this,
`dotfiles/home/tally.nix:134-138`).

## 2. The `flow` pool

Auto-declared when `flows != {}` (overridable by an explicit user declaration of the same
name): `resource = "cpu-slot"`, `capacity = 8`, `enforce = "cooperative"`,
`hardPreempt = false`. A blocked runner holds only this near-free slot. Assertion: a flow
script's `meta.pools` MUST NOT list `flow` (nodes never run in the runner's pool).

## 3. Eval-time validation — the most Nix-native section

Extending `mkCheckedConfig`'s runCommand-with-the-real-binary bridge
(`common.nix:1962-1980` → `Mode::CheckConfig`, `crates/tally/src/main.rs:605-610`), each
declared flow adds to the checked-config derivation:

1. **Dialect check**: `tally flow check ${script}` — the real embedded engine parses the
   script, validates the pure-literal `meta` block, runs the determinism lint (banned
   global references detectable statically), and emits `meta` as JSON on stdout. Non-zero
   exit fails the build with the engine's own error (line/column).
2. **Pool closure**: `jq` cross-check that `meta.pools ⊆ config.pools` (plus the §2
   assertion). An undeclared pool is a **build failure at `nixos-rebuild`/`home-manager
   switch` time**, never a 1 a.m. `unknown pool` runtime failure.
3. **Args validation**: rendered `args` validated against `meta.argsSchema` (the binary
   does this in `flow check --args`; JSON Schema evaluation lives in Rust, not in Nix).
4. **Catalog check**: when `catalog` is set, `tally flow check --catalog` validates it
   against the catalog schema (§4) and confirms every selector class the script's meta
   declares resolves to ≥1 member.

The flake adds fixture checks in the style of `adapter-presets`/`producer-registry`
(`flake.nix:858-953`): a valid flow script accepted; scripts violating each lint rule
(nonliteral meta, banned global, undeclared pool, bad argsSchema) each rejected with the
exact expected message — the schema-rejection idiom already used seven times in the flake.

## 4. Catalog contract (schema owned here, instances owned by dotfiles)

`tally.nix` owns the JSON Schema for the selector catalog; `dotfiles` renders instances
from `lib/local-models.nix`. Shape (normative fields; full schema ships in-repo and is
enforced by `flow check --catalog`):

```json
{ "version": 1,
  "members": [ { "id": "qwen3-coder-next", "family": "qwen", "maker": "alibaba",
                  "classes": ["pooled-fast", "pooled-strongest", "coding"],
                  "adapter": "pi", "pools": ["worker-gpu"],
                  "launch": { "model": "…" } } ] }
```

Selector resolution (`members('pooled-strongest', {count, diversity})`) is deterministic:
filter by class → order by the catalog's array order → apply the diversity key
(`family` | `maker`) as a round-robin partition → take `count`. The resolved list is
stamped into provenance before any member node is submitted (pi-appliance
witness-before-inference rule — `docs/transfer/dotfiles-prior-art.md`). **Capacity note
(explicit, from the prior-art review):** selectors resolve *membership*, not concurrency;
with today's capacity-1 `worker-gpu` pool, members drain sequentially and correctly. A
`budgetGb`-partitioned co-resident vram pool is the declarative way to buy real
concurrency later; nothing in the selector contract assumes it.

## 5. GitHub command → flow wiring

No new producer machinery: a gh producer's `commandComments` trigger maps a command to an
enqueue template (`common.nix:983-1124`) whose argv is the same `tally flow run <store
path>` invocation. GitHub chooses *which* flow, never *what* code: scripts are store paths
baked into config; comment text supplies only bounded validated scalars into `--args`
(existing scalar-interpolation validation). The flow parent carries `ghOrigin`; children
carry ancestry + `relatedTrigger` receipt references (FLOW-SPEC §4). Posting policies
(receipt/evidence/gate-summary/close-on-acceptance) attach to the parent runner job's
witness — the issue sees one command in, one proof out.

## 6. Hardening presets

New `adapters.<name>.hardening` option: `nullOr (enum ["strict" "workspace" "none"])`,
default `null` (= `none`, current behavior — no silent tightening of existing deploys).
Rendered into the transient unit properties the executor stamps (the Rust executor owns
`systemd-run` invocation; the preset name travels through config). Directive bundles are
adapted from the srvos github-runners worked example
(`docs/transfer/nix-module-style.md`): `strict` = `ProtectHome=read-only`, `PrivateTmp`,
`ProtectSystem=strict`, `NoNewPrivileges`, `RestrictAddressFamilies=AF_UNIX AF_INET
AF_INET6`, explicit `ReadWritePaths` = workspace + state; `workspace` = `PrivateTmp` +
`ReadWritePaths` only. Presets are per-adapter configuration — trust the agent's intent,
bound its blast radius mechanically; tool-side output policing stays rejected.

## 7. Checks summary added to `nix flake check`

`flow-dialect-accept`, `flow-dialect-reject-*` (one per lint rule), `flow-pool-closure`,
`flow-catalog-schema`, plus the multi-host `runNixOSTest` (`nodes.coordinator` +
`nodes.worker`, baked SSH keypair fixtures — new work, slot documented in
`facts-nix-module.md` §5) exercising: one flow end-to-end over the SSH executor, daemon
kill mid-run, replay-through-attach, and one cross-host artifact handoff through the
sanctioned data plane (FLOW-SPEC §15).
