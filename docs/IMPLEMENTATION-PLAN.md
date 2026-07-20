# tally — v0 implementation plan (layered module map for the Bun build)

> **2026-07-09.** The build order for THE V0 BUILD: comprehensive, fully-featured, explicitly
> NOT stubbed — no TODO bodies, no "not implemented" paths for anything the SPEC marks as v0
> scope. Definition of done: **working code pushed to GitHub — not a release** (V0.1-PATH.md
> preamble). The first real drive is Tom's academic-PDF OCR pass (~4.7k sidecars) run as a
> genuine tally job, so the job → pls lease → worker → witness → artifact path must work
> end-to-end (V0.1-PATH §3; BUILD-SEQUENCE steps 3+5).
>
> Governing docs, in authority order: `docs/DECISIONS.md` (ruling ledger) · `docs/SPEC.md`
> (product spec) · `docs/CLI-SURFACE.md` (**§2 is the FROZEN wire contract — honor it
> byte-for-byte**) · `docs/BUILD-SEQUENCE.md` (the ordered spine this plan parallelizes) ·
> `docs/REFERENCES.md` (lineage; `vendor/` is clean-room inspiration ONLY — herdr is AGPL,
> cmux-manaflow is GPL; **never lift code**, CLI-SURFACE §4). The gh-intake query surface is
> the octo.nvim surface scan (notes `july26-fable-second/tally-shape/octo-nvim-surface-scan.md`);
> the ownership table is the boundary investigation (notes
> `july26-fable-second/tally-boundary/BOUNDARY-INVESTIGATION.md`), both crystallized into
> SPEC "The tally / dotfiles boundary" and DECISIONS "dotfiles boundary contract".

---

## 0. Stack & machine facts

- **TypeScript on Bun**, one Bun-compiled binary = daemon + CLI (DECISIONS jul9 Bun flip;
  SPEC "Flake outputs"). `bun build --compile` + `autoPatchelfHook`, packaged via **bun2nix**
  (Backlog.md's first-party flake is the live reference).
- **Never Rust-inside-Bun FFI** (DECISIONS jul9: FFI hybrid ruled OUT). TaskChampion access is
  `task export`/`task import` **shell-out only**, version-pinned (DECISIONS jul9).
- **journald emission is `StandardOutput=journal` stdout capture** with `SyslogIdentifier=tally`
  in the unit — not a native journal-socket client (SPEC "Emission path"; DECISIONS jul9).
- bun is NOT installed on this NixOS box: every bun command runs as
  `nix shell nixpkgs#bun --command bun …` (verified: bun 1.3.13). nix 2.34; gh authenticated
  as mecattaf; remote `https://github.com/mecattaf/tally.git` (main).

### Build / test / compile commands (this machine, exact)

```sh
cd /home/tom/mecattaf/tally
nix shell nixpkgs#bun --command bun install                 # deps (typescript, @iarna/toml or smol-toml, bun-types)
nix shell nixpkgs#bun --command bun run typecheck           # = bunx tsc --noEmit (typescript is a devDependency)
nix shell nixpkgs#bun --command bun test                    # full bun test suite
nix shell nixpkgs#bun --command bun run build               # = bun build --compile --outfile=dist/tally src/main.ts
nix run .#bun2nix                                           # regenerate bun.nix from bun.lock (bun2nix is a Nix-distributed
                                                            #   codegen binary, NOT an npm dep — run via the apps.bun2nix output;
                                                            #   the package.json `bun2nix` script just execs `nix run .#bun2nix`)
nix build .#tally                                           # the packaged binary (bun2nix + autoPatchelfHook)
nix flake check                                             # module eval + package build sanity
nix run .#dev                                               # process-compose dev rig, mock jobs (SPEC "Inputs & dev rig")
```

`dist/` and `node_modules/` join `.gitignore`. Every module below must keep
`bun run typecheck` and `bun test` green at merge.

---

## 1. Explicitly OUT of v0 (docs-deferred — do not build)

Listed so no implementer "helpfully" adds them:

1. **`tally enqueue --ephemeral`** — deferred behind first-need (CLI-SURFACE §5 "Deferred").
2. **`tally pane annotate`** (`custom_status` + TTL) — deferred until the CUBS consumer exists
   (CLI-SURFACE §5 "Deferred"). The `custom_status` *field* stays in the schema (§2.2).
3. **`tally query standup --stale-hours N`** — RECOMMENDED-ADOPT but **ruling PENDING; "do not
   build as ruled"** (CLI-SURFACE §1.5 note).
4. **Approval/escalate gating (Q2 relaxation)** — recorded doctrine, **DORMANT**: "No tally
   component implements this" (SPEC "What tally is NOT"; DECISIONS agentctl entry).
5. **Front-end substrate / `tally bridge` / web renderer** — "a given build pass builds the
   **CLI only**; the non-interactive front-end substrate comes at the END" (SPEC front-end
   doctrine; BUILD-SEQUENCE step 10). No bridge subcommand, no bundle.
6. **Multi-pool subscription/API substrate** (cc-usage meter daemon, GO/SLOW/STOP gate,
   round-robin pool-assigner) — OUV-CM r5: generalized **after** a second real pool exists;
   BUILD-SEQUENCE step 9 lands after the single-GPU meter+witness. v0 ships the two **GPU**
   pools fully + the DS4 co-allocation; the witness `pool`/`charge` fields are populated
   GPU-only so step 9 is purely additive (SPEC "Meter unification"). `queue pause/resume`
   (the admission gate those pools will use) IS built now — it's in the frozen verb set.
7. **RSS/mail intake, gcal view, VTODO export** — deferred/off-critical-path (PS#8, PS#4;
   BUILD-SEQUENCE step 8 marks gcal "optionally"). gh intake IS v0 (step 8), OFF by default.
8. **`nixosModules.tally`** — ships as the ruled **unbuilt thin wrapper stub** (PS#17). A stub
   here is compliance, not a shortcut.
9. **Session lifecycle** — tally never creates/names/attaches/reattaches zmx sessions and never
   calls `kitty @ launch` (CLI-SURFACE §1.2 boundary, §3.2). The **sole** boundary exception is
   the `claude -p` contingency, **kept non-default behind the `--via-terminal` flag** (DECISIONS
   Q6; CLI-SURFACE §3.1 sidenote) — build it, gated, documented as outside the zmx substrate.
   Mechanism recorded (M2.2): the flag path uses `kitty @ launch` confined to one named function
   in `src/agents/claude-p-contingency.ts` — the single carve-out in the M1.6 boundary grep-test.
10. **NPU titling, sandboxing, model-tier routing, email bridges, Baikal** — permanently out
    (SPEC "What tally is NOT"; DECISIONS Q7, PS#appendix, PS#2, PS#8, PS#21).

Provisional defaults for the two genuinely-open detector flags (CLI-SURFACE §5 flags 4/5),
chosen minimal + easily changed, recorded here for Tom to re-rule:

- **Flag 4 (scrape cadence, un-hooked panes):** watcher-edge-triggered scrape plus a fixed
  fallback poll — 2 s while a pane's agent is `working`, 10 s otherwise; config-overridable.
- **Flag 5 (shell panes):** NOT grid-classified. `agent.kind=shell` derives status purely from
  job/process state (`working` while the dispatched process runs, `done`/`idle` on exit); no
  TOML manifest for shell.

---

## 2. Layered module map

Rules of engagement: modules within a layer own **disjoint** file sets and can be built in
parallel; a module may only import from `src/contracts/` (layer 0) and from modules it names
in `dependsOn`. All subprocess access (kitty, zmx, task, pls, gh, journalctl, git, systemd-run)
goes through the injectable `Exec` seam defined in contracts, so every module is testable
against the layer-0 fakes without the real substrate.

### Layer 0 — scaffold, shared contracts, test kit

**M0.1 `scaffold`** — files: `package.json`, `tsconfig.json`, `bunfig.toml`, `.gitignore`
(edit), `flake.nix`, `nix/package.nix`, `nix/pls.nix`, `bun.nix` (generated), `src/main.ts`.
Repo bootstrap per BUILD-SEQUENCE step 1 under the Bun ruling. `package.json` scripts:
`typecheck` (`bunx tsc --noEmit`), `test`, `build` (`bun build --compile --outfile=dist/tally
src/main.ts`), and `bun2nix` — **the `bun2nix` script execs `nix run .#bun2nix`** (bun2nix is a
Nix-distributed codegen binary from the `bun2nix` flake input, not on PATH inside `nix shell
nixpkgs#bun`), so both entry points converge on the one apps output. Dependencies: `typescript`,
`bun-types`, one TOML parser
(`smol-toml`), nothing else heavyweight — the binary must stay single-file-compilable.
`flake.nix` on **flake-parts** with inputs: `nixpkgs`, `flake-parts`, `bun2nix`,
`process-compose-flake`, plus the tally-pinned substrate inputs **pls**
(github:sniarchos/pls — promoted to the named list, DECISIONS Q3, **`flake = false`**:
verified the live tree ships NO flake.nix — `bin/pls`, `server/app.py`, `client/pls.py` are a
plain dependency-free Python project — so a bare input without `flake = false` fails
`nix flake lock` on the spot), **pi** (badlogic/pi,
`flake = false`; ⚠ the pin is stale per CLI-SURFACE §3.4/§5 flag 3 — the adapter binds to the
documented interface, never this clone), **llama-swap** (mostlygeek/llama-swap, **`flake = false`**:
verified it also ships no flake.nix; it is pinned-only in v0), and **taskwarrior** (taskwarrior 3.x;
**upstream ships no root flake — use `pkgs.taskwarrior3`** (do not burn a lock cycle discovering
the "upstream flake" does not exist) — the pin discipline is
"each on its own named trigger, never bundled", SPEC "Inputs & dev rig"; `gh` resolves from
nixpkgs, auth is ambient per DECISIONS Q8). **Packaging ownership (scaffold owns, M3.3 consumes):**
because pls is a `flake = false` source with no derivation of its own, scaffold's `nix/pls.nix`
wraps the pinned pls source in a trivial `python3` application derivation (upstream is
dependency-free, so a `writeShellApplication`/`buildPythonApplication`-style wrap with `python3`
on PATH suffices), exported as **`packages.pls`** so M3.3's pls broker units get an ExecStart
store path and the M3.3 `pls-wrap` PATH wiring references it rather than an implementer guessing
who packages a Python app from a `flake = false` source; **llama-swap** resolves from
`pkgs.llama-swap` (or, if absent, the same `flake = false` wrap) — pinned-only in v0, so the
cheapest compliant form wins. Outputs: `packages.tally`, **`packages.pls`** (the wrap above),
`homeManagerModules.tally`, `nixosModules.tally`, `apps.dev`, **`apps.bun2nix`** (wraps the
`bun2nix` input's codegen package so `nix run .#bun2nix` regenerates `bun.nix` from `bun.lock`
— BUILD-SEQUENCE step 1; the `package.json` `bun2nix` script execs this), and `checks`.
**Scaffold CREATES minimal valid placeholder files** at `nix/hm-module.nix`,
`nix/nixos-module.nix`, and `nix/dev.nix` — empty-module bodies (`{ ... }: { }`) for the two
modules and a trivial no-op `apps.dev` for the dev rig — so `flake.nix` references files that
exist and are git-tracked, and **`nix flake show` / `nix flake check` go green at layer 0**
(the layer-0 acceptance "nix flake shows the three outputs" is otherwise unmeetable, since Nix
evaluates outputs from files that must already exist). **Layer-0 acceptance (added):
`nix flake lock` succeeds with all named inputs resolved** — i.e. `pls`, `pi`, and `llama-swap`
each carry `flake = false` so the lock does not choke on a missing flake.nix, and this gate
runs green before the scaffold acceptance can go green (it is the same failure class the prior
review caught for bun2nix and the placeholder nix files). **Handoff (explicit disjoint-ownership,
"scaffold creates, layer-3 modules overwrite"):** `nix-module` (M3.3) OVERWRITES
`nix/hm-module.nix` + `nix/nixos-module.nix` with the real modules; `dev-rig` (M3.4)
OVERWRITES `nix/dev.nix` with the real process-compose rig — so there is exactly one final
writer per file, not two silent ones. `src/main.ts` is the one entry: argv[1] === "daemon" boots the
daemon (layer 1), anything else dispatches to the CLI (layer 3) — both wired via a tiny
registry so layer-0 compiles standalone.

**M0.2 `contracts`** — files: `src/contracts/*.ts`
(`wire.ts`, `events.ts`, `snapshot.ts`, `witness.ts`, `journal.ts`, `task.ts`, `job.ts`,
`agent.ts`, `selectors.ts`, `config.ts`, `paths.ts`, `constants.ts`, `errors.ts`, `exec.ts`,
`bus.ts`, `index.ts`). **The shared-contracts module every parallel implementer reads.**
Encodes, verbatim from the frozen docs (see §3 "Shared contracts" below for the full
inventory): the §2 wire frames and every event payload, the §2.2 snapshot shape, the PS#9
witness record (17 fields incl. `seq`/`prev_hash`/`hash`), the FS§4+PS#11 journald field
matrix, the Seam-A enqueue params/result, the four-state status enum, the three keys
(`persistence_session_id` / `kitty_window_id` / `session_ref` — never conflated,
CLI-SURFACE §0), the TW UDA names incl. `trust`, all numeric bounds as named constants, the
`Exec` subprocess seam, and the in-daemon event-bus interface. Ships exhaustive
compile-time-shape tests plus runtime validators (hand-rolled narrowing functions, no zod —
keep the compile small) used by the daemon on ingress.

**M0.3 `testkit`** — files: `test/helpers/*.ts`, `test/fixtures/**`.
Fake `Exec` implementations: scripted `kitty @` (ls JSON trees, get-text grids, send-text/
focus-window recorders), fake `zmx list --short`, fake `task` (export/import/config over a
tmp JSON store, faithful to taskwarrior JSON), fake `pls` (lease grant/queue/release with
generations), fake `gh` (canned notifications + graphql search pages + rate_limit), fake
`journalctl`, fake `systemd-run` (falls through to direct spawn). Tmp-dir/ledger scaffolding,
an NDJSON socket test client speaking §2 framing, and grid fixtures for the detector
(claude-code spinner frames, pi prompt-box frames, blocked-prompt frames). Every layer ≥1
module writes its bun tests against this kit.

### Layer 1 — foundation services (each depends only on `contracts` + `testkit`)

**M1.1 `daemon-core`** — files: `src/daemon/*.ts`
(`server.ts`, `framing.ts`, `rpc.ts`, `subscriptions.ts`, `replay-ring.ts`, `wait.ts`,
`heartbeat.ts`, `epoch.ts`, `state.ts`, `supervise.ts`, `index.ts`).
The Bun unix-socket server implementing **CLI-SURFACE §2 byte-for-byte**: socket at
`$XDG_RUNTIME_DIR/tally/tally.sock`, mode 0600, local-only (§2.1); NDJSON framing — one UTF-8
JSON object per line, LF, request `{id,method,params}` / response `{id,result|error}` /
event `{seq,event,…}` interleaved on one connection; 64 KiB per-frame cap; monotonic `seq`
per `lease_epoch` + stable event uuid `id`; bounded replay ring of exactly **4096** events;
subscribe ACK with `resume:{after_seq,oldest_seq,latest_seq,next_seq,gap}`; slow-subscriber
disconnect at **1024** unacked frames via a final `stream.overflow`; `heartbeat{ts,latest_seq}`
~15 s, suppressible, not replayable, no own seq; RPC methods `session.snapshot`,
`session.subscribe` (names/categories filters, `min_protocol`/`max_protocol` negotiation with
`unsupported_protocol` error; first response is the ACK carrying the literal `type:"subscription"`
discriminator, §2.4), `session.wait` (predicate subjects `job`/`agent`/`pane_output`
with count/timeout semantics; `pane_output` **rejects `is_viewer` panes** — anti-loop
invariant #4, and `wait.ts` satisfies a `pane_output` read through the **`WaitScrapeProvider`
seam** the detector registers, never importing sensors/detector; **the provider's match is
emitted as `pane.output_matched` by the detector — `wait.ts` only consumes it, returning that
same event as the `session.wait` result**), `session.ack`,
`session.unsubscribe` (§2.4), plus the internal-additive carriers (`queue.*`, `pane.*`,
`agent.*`, `query.*`, `session.list`, `session.register_viewer`, `kitty.watcher_event`,
`agent.hook_event` — full inventory in §3); every replayable event frame is stamped with both
the monotonic `seq` and the stable event uuid `id` (both first-class wire fields, §2.1);
`protocol_version = 1`, bumped
only on breaking change; `lease_epoch` changes ONLY on daemon (re)start (§2.5) — sourced from
the pls lease generation, backstopped by the daemon-incremented **counter file** so it stays
monotone across unclean reboot (PS#21 lease-epoch source as amended 2026-07-12, issue #9;
`epoch.ts` owns the file under `$XDG_STATE_HOME/tally/epoch` and is its sole incrementer — the
unit's ExecStartPre bump was removed). `supervise.ts` is the restart-isolation harness for in-daemon
threads (the detector rides it, PS#15a). Snapshot *assembly* delegates to a provider
interface registered by the session-model (layer 2) — daemon-core owns the transport, never
the model. Tests: framing round-trips, ring/gap/overflow/ack, epoch-change voiding cursors,
protocol negotiation, wait predicates incl. viewer rejection, two subscribers one stream.

**M1.2 `witness`** — files: `src/witness/*.ts`
(`ledger.ts`, `chain.ts`, `record.ts`, `verify.ts`, `model-id.ts`, `index.ts`).
The append-only witness JSONL — **ledger-as-truth** (PS#9). Physical append: plain `O_APPEND`
+ `fsync` per line, each line a complete JSON object, no checksum prefix, no temp-then-rename
(PS#10a); ledger path `$XDG_DATA_HOME/tally/witness.jsonl`. Full canonical record per SPEC
"Record schema": `task_uuid, transition_timestamp, verdict, exit_code, artifact_content_hash,
gpu_seconds, wall_clock, attempt, lease_epoch, dedup_key, labor_class, trace_ref?, pool,
charge, model, seq, prev_hash, hash` — the 5-field form is a projection only. **Per-line hash
chain (jul9):** `seq` monotonic, `prev_hash = sha256:<hex>` of the prior line, `hash` =
`"sha256:" + hex(sha256(line's JSON with hash field cleared))`; **restart-surviving** — on
boot scan the ledger forward, discard a torn trailing line by JSON-parse failure (PS#10a rule),
recover `(last_seq, last_hash)` so one unbroken chain spans daemon restarts. Implementation
choice recorded: **one ledger-wide chain** (the SPEC leaves ledger-wide vs per-job open; the
ledger-wide chain matches "one unbroken chain per ledger" and agentctl precedent — flip to
per-job later is additive). `verify.ts` backs `tally witness verify`: walks `seq` order,
recomputes each `hash`, checks each `prev_hash`, reports the exact breaking `seq` + reason;
sequence-gap completeness is a separate pass; **runs on any copy of the ledger, no daemon**.
`model-id.ts`: models.dev `provider/model-name` normalization — ids containing `/` pass
through, bare harness names are prefix-normalized (`claude-*` → `anthropic/…`, `gpt-*` →
`openai/…`, `gemini-*` → `google/…`); `model` absent on shell runs. GPU-seconds derive from
the **witness span** (job start/end recorded natively — DECISIONS timewarrior-removed row);
`labor_class != fresh` and `verdict=clean-exit-no-artifact` lines are excluded from canonical
GPU-seconds aggregation helpers. Tests: chain across simulated restart, torn-line discard,
tamper/truncate/reorder detection with exact breaking seq, model-id table, fsync-per-line
append semantics, projection shape.

**M1.3 `taskchampion`** — files: `src/tw/*.ts`
(`client.ts`, `udas.ts`, `rows.ts`, `oplog.ts`, `index.ts`).
The **thin durable veneer** (DECISIONS 2026-07-09 evening): TaskChampion via `task export` /
`task import` **shell-out, never in-process** (jul9 ruling; 30-80 ms is fine — never called in
a hot loop). `udas.ts` bootstraps the UDA vocabulary idempotently via `task config` (`rc.confirmation=off`):
`agent`, `labor_class`, `pool`, `session_ref`, `model_class`, `cwd`, `worktree`, `trust`
(values `unreviewed|reviewed|recalled` — written `unreviewed` at completion, flipped only by
review/recall, **never blocks future work**; SPEC "The trust review UDA") plus `dedup_key` and
`lease_epoch`-adjacent job metadata as UDAs where the row needs them. `rows.ts` enforces the
**durable-row admission test** (appendix; CLI-SURFACE §1.1a): a row only for
autonomous/batch/queued units needing cross-source urgency or crash-survival — one row per
durable job, one standing row per drain, and **no high-frequency machine state (heartbeats,
leases, evidence) ever written to TW**; live-orchestrator-spawned units get `task_uuid: null`.
Priority maps to TW `priority` (urgency engine, PS#1a). `oplog.ts` derives the **`prev_*`
shadow fields** (CLI-SURFACE §2.3 note): under the shell-out constraint the attribute-level
delta is captured by exporting the row immediately before mutation (the op-log's computed
delta, re-derived at the only legal access altitude) — consumers read `prev_state`/`prev_status`
off the wire, additive-optional, never a protocol bump. Store is **single and authoritative on
the conductor**; no sync, no replication (SPEC "Conductor-receiver"). Tests against the fake
`task` binary: UDA bootstrap idempotence, admission-test matrix, import/export round-trip,
prev_* derivation, veneer discipline (assert no heartbeat-shaped writes).

**M1.4 `journal`** — files: `src/journal/*.ts` (`emit.ts`, `reader.ts`, `index.ts`).
The journald TALLY_* emission + read path (SPEC "journald TALLY_* event schema", FS§4 + PS#11).
`emit.ts` writes exactly one structured line per event to **stdout** (captured by
`StandardOutput=journal`, `SyslogIdentifier=tally` in the unit — the jul9 ruled mechanism):
a single-line JSON object carrying `SYSLOG_IDENTIFIER`-adjacent fields `TALLY_EVENT`
(`enqueued|dispatched|started|heartbeat|preempted|resumed|completed|failed|evidence_pass|
evidence_fail|witness_emitted`), `TALLY_TASK_UUID`, `TALLY_CLASS`, `TALLY_SOURCE`,
`TALLY_AGENT`, `TALLY_SESSION_REF`, `TALLY_UNIT`, `TALLY_EXIT_CODE`, `TALLY_GPU_SECONDS`,
`TALLY_ARTIFACT_HASH`, `TALLY_EVIDENCE`, `TALLY_ATTEMPT`, `TALLY_LEASE_EPOCH`,
`TALLY_LABOR_CLASS`, and a human-readable `MESSAGE`, honoring the required-at matrix
(always / at-dispatch+ / at-completed…). journald is **observability, not load-bearing
memory**; the witness is emitted from these fields but is a separate artifact. `reader.ts`
shells `journalctl -t tally -o json [--since …] [-f]` and re-hydrates TALLY_* fields (parsing
the JSON MESSAGE payload when fields ride stdout capture) — the read half of `query log` and
the standup join. Tests: field-matrix completeness per event, reader round-trip via fake
journalctl, single-line no-embedded-newline guarantee.

**M1.5 `pls`** — files: `src/pls/*.ts`
(`pools.ts`, `broker.ts`, `lease.ts`, `coalloc.ts`, `wrap.ts`, `index.ts`).
pls as the box governor (PS#5): tally **owns the pool configuration** — `pools.ts` declares the
two GPU pools (`worker-gpu` prioritized, `controller-gpu`), single-lease-per-pool
(`PLS_CAPACITY=1`), `--cost` = estimated VRAM-GB budget math; pool config is rendered for the
module's broker units (layer 3 consumes it). `broker.ts` is the client to **both** boxes'
brokers (worker reachable over TB3/tailnet — address from config, never a hardcoded hostname,
DECISIONS Q9). `lease.ts`: acquire-before-GPU at declared priority, **RAII/process-exit as the
single release path** (never a second release), lease **generation** surfaced as the primary
`lease_epoch` source, holderless-lease reclaim hook for recover(). The lease is
**non-preemptible**; preemption lives one layer up (jobs). `coalloc.ts`: the DS4 cross-box
atomic co-allocation — heavy worker hold + light controller spill, both-or-queue (PS#5).
`wrap.ts`: the ambient **pls-lease-wrap** helper (`tally pls-wrap -- <cmd>` internal verb +
the script the module installs) so any heavy invocation is lease-gated without knowing tally
exists (SPEC "The ambient default"). Tests against fake pls: serialization of two competitors,
process-death release, generation monotonicity, co-alloc both-or-queue, direct non-tally
tenant acquisition.

**M1.6 `sensors`** — files: `src/kitty/*.ts` (`rc.ts`, `throttle.ts`, `watcher-ingest.ts`),
`src/zmx/client.ts`, `hooks/kitty/tally-watcher.py`.
The kitty sensor/actuator surface (CLI-SURFACE §3.1 — **the out-of-band law: never interpose
on the pty byte stream**) and the zmx read surface (§3.2). `rc.ts` shells the four sanctioned
verbs, keyed on `kitty_window_id`: `kitty @ ls` (inventory: windows, cwd, foreground_processes
incl. `title` for the OSC regions, user-vars, focus), `kitty @ get-text --match id:<id>` with
extent flags (the throttled grid read), `kitty @ send-text` (+ key escapes for `send-key`),
`kitty @ focus-window --match id:<id>`. **No `kitty @ launch`** anywhere (boundary). At most
`kitty @ set-user-vars` writes one opaque identity back-reference — never status (§5 flag 1).
`throttle.ts` centralizes read throttling so detector + `pane capture` share one budget.
`tally-watcher.py` is the kitty watcher payload (on_close / on_focus_change / on_cmd_startstop
/ on_title_change / on_set_user_var): a tiny stdlib-only script that connects to the tally
socket and posts an internal additive RPC (`kitty.watcher_event`) — the event edge that
replaces existence-polling; the **registration line lives in dotfiles kitty.conf**, the module
exports this script's store path read-only (DECISIONS Q4). `watcher-ingest.ts` validates and
re-emits those edges onto the bus. `src/zmx/client.ts`: **enumerate-only** — `zmx list --short`
for the session universe; `persistence_session_id` = the zmx name; tally never
creates/names/attaches/kills (CLI-SURFACE §3.2 MUST-NOT list). `rc.ts` declares
`kitty @ launch` FORBIDDEN and absent from the sensors surface. Tests: rc arg construction,
watcher NDJSON ingestion, throttle coalescing, zmx list parse, and the boundary grep-test
asserting the forbidden verbs (`kitty @ launch`, `zmx attach/kill`) appear nowhere in `src/`
**with exactly ONE carve-out: `src/agents/claude-p-contingency.ts`** (the gated `--via-terminal`
contingency, M2.2/§1 item 9) — the test excludes that single file path and additionally asserts
its `kitty @ launch` is reachable only behind the `--via-terminal` flag, so the boundary law and
the contingency co-exist without contradiction (`zmx attach/kill` remain forbidden everywhere,
no carve-out).

### Layer 2 — engines (compose layer 1)

**M2.1 `session-model`** — depends: daemon-core, sensors. Files: `src/model/*.ts`
(`store.ts`, `discovery.ts`, `workspace.ts`, `rollup.ts`, `snapshot.ts`, `index.ts`).
The in-memory session data model and its reconcile-from-disk discipline:
`{session, pane} → (persistence_session_id, kitty_window_id, agent{kind,status})` (FS§5),
grouping tier **Workspace → Session → Pane** with status dots aggregated up (PS#6b), pane
`id = "<session>:<pane>"` (CLI-SURFACE §0). `discovery.ts` joins `zmx list --short` ×
`kitty @ ls` × watcher edges into pane/session records (`observed_at` = first seen, never
creation), maintains `is_viewer` marking for `tally session watch` panes (anti-loop invariant
#4 — the flag every detector/capture path honors). **`store.ts` is the ONE authoritative
session store** (the single-store ruling, issue-3 resolution): detector and jobs do NOT own
snapshot legs — they **write into this store via the `Bus`** (detector writes `agents[]` from
its records, jobs writes `jobs[]` from lifecycle), each registering through the
**`SnapshotSectionProvider`** seam so the store knows which section it feeds; `detector/records.ts`
is demoted to detector-internal explain-data (not a second agent store). `discovery.ts` also
handles **`session.register_viewer {kitty_window_id}`** — a `session watch` client posts it
(reading `$KITTY_WINDOW_ID` from its env) to mark its own pane `is_viewer=true`. `workspace.ts`
populates tier 1 best-effort from `niri msg -j workspaces` when present, else one default
workspace named by `conductorHost` config (tally observes; niri owns layout). `snapshot.ts`
registers the daemon-core `SnapshotProvider` and assembles the **§2.2 bootstrap frame
verbatim from the single store** —
`protocol/protocol_version/daemon_version/lease_epoch/seq/ts/focus/workspaces/sessions/panes/
agents/jobs` (it reads the `agents[]`/`jobs[]` legs from the store the siblings wrote, never
importing detector or jobs). Emits `session.observed`, `session.ended`, `workspace.focused`,
`pane.created`, `pane.closed`, `pane.focused` per §2.3. Tests: discovery joins from fake
ls/zmx fixtures, rollup aggregation, snapshot shape golden-file against §2.2 (incl. the
detector-written `agents[]` + jobs-written `jobs[]` legs composed via the store), viewer
marking + `register_viewer`, pane id encoding.

**M2.2 `jobs`** — depends: taskchampion, pls, witness, journal, daemon-core. Files:
`src/jobs/*.ts` (`engine.ts`, `enqueue.ts`, `dedup.ts`, `dispatch.ts`, `lifecycle.ts`,
`evidence.ts`, `recover.ts`, `preempt.ts`, `barrier.ts`, `index.ts`) and `src/agents/*.ts`
(`kinds.ts`, `pi.ts`, `claude-code.ts`, `claude-p-contingency.ts`, `shell.ts`).
**The spawn-tracked-agent-job — the one execution primitive** (SPEC "Three planes"). Seam A in
full (CLI-SURFACE §1.1a): params `{priority, source, kind, invocation|argv, cwd|worktree,
evidence[], pool, model_class, dedup_key, session?, barrier/wait-group/wait-count/wait/
timeout/detach}`; result `{task_uuid, lease_epoch, pool, status, session_ref, dedup_key,
witness_lsn, verdict}`. Durable-row admission via the TW veneer (row or `task_uuid: null`);
**every heavy unit emits a witness line, row or no row** (appendix). `dedup.ts`:
dedup-by-existence — pre-run `stat` of the artifact + grep of the success witness for
`dedup_key`; re-hash **only on mtime/size mismatch**; hit ⇒ skip the GPU run, `labor_class=
reused`, `status:"reused"`, excluded from canonical GPU-seconds (SPEC "Dedup-by-existence").
`dispatch.ts`: priority queue → pls lease (worker-gpu default for heavy work; declared `--pool`
hint honored — **no model re-picking ever**, PS#2) → execution as a transient systemd user
unit via `systemd-run --user --unit tally-job-<id> …` (the `TALLY_UNIT`/`unit` field), with a
direct `Bun.spawn` fallback when systemd is absent (dev rig/tests); env carries
`TALLY_TASK_UUID`, `TALLY_SESSION_REF`, `TALLY_YIELD_FD` conventions. `src/agents/*`: the three
adapters build the leaf invocation and extract `session_ref` + `model` — `pi` (`pi --session
<id>` resume; extensions dir per §3.4 — bind to the documented interface, the vendor pin is
stale), `claude-code` (`claude --resume <id>`; `claude -p` is the **non-default contingency
flag** `--via-terminal` implemented per the §3.1 sidenote: launch in kitty + delayed
`send-text` kickoff — the sole boundary exception, off by default), `shell` (no model, no
session_ref).
**Contingency launch mechanism (recorded so no implementer guesses):** the `--via-terminal`
path uses **`kitty @ launch`** to open `claude` in a kitty window, waits ~10 s, then
`kitty @ send-text` the autonomous-mode kickoff (per the §3.1 sidenote). This `kitty @ launch`
call is **confined to exactly one named function `launchViaTerminal()` in a dedicated file
`src/agents/claude-p-contingency.ts`** (the ONLY place under `src/` where `kitty @ launch`
appears), reachable **only** behind the `--via-terminal` flag. Its windows are **outside the
zmx substrate** per DECISIONS Q6 (though the run still becomes a recoverable zmx-backed session
per the §3.1 sidenote side benefit). The sensors grep-test (M1.6) carves out this one file (see
there). This satisfies §1 item 9 "build it, gated" without the grep test failing. `lifecycle.ts` drives the job state machine and emits the `job.*` events
**mirroring journald TALLY_EVENT verbatim** (one vocabulary) to bus + journal + witness:
enqueued → dispatched → started → heartbeat (throttled gpu-seconds tick) → evidence_* →
completed|failed (+ preempted/resumed/witness_emitted). `evidence.ts`: terminal commit gates
on **artifact-exists ∧ content-hash ∧ exit-code-ok ∧ witness-span**, never self-report; clean
exit without a gate-passing artifact ⇒ `verdict=clean-exit-no-artifact`, excluded from
canonical GPU-seconds, `TALLY_EVENT=evidence_fail` (PS#21). `recover.ts`: **re-present, never
replay** — witness_lsn reconciliation on boot (ledger tail-hash vs max applied lsn,
full replay only on mismatch), ACK-gated retry, zombie fencing by lease-epoch, undeleted-row
⇒ re-dispatch (`pi --resume`, `labor_class=recovered`), bounded attempt-capped requeue (PS#9's
five invariants). `preempt.ts`: preemption-as-policy — the cooperative yield-at-checkpoint
signal (SIGUSR1 to the holder's unit) above the non-preemptible lease; holder records
session_ref, releases via process-exit, batch re-dispatched via resume (SPEC "The inner
fold"). `barrier.ts`: `--wait` blocks on terminal `job.completed|job.failed|job.evidence_fail`
deltas with exit code mirroring the verdict; barrier = enqueue-N-await-N over `session.wait`
(subsumes `wait_for_subagents`); `--timeout` never cancels. `queue cancel/pause/resume`
semantics: cancel with `--force` fencing by lease_epoch; pause/resume as the pool admission
drain gate (running holders keep their lease) — CLI-SURFACE §1.1. Tests: the full happy path
against fakes, dedup skip + re-hash-on-mismatch, evidence-fail forensics, recover() torn/ACK/
fence/bounded matrices, preempt-yield-resume, barrier N-of-M, row-vs-no-row admission, OCR-
shaped batch (many enqueues, one lease, artifacts + witness lines verifiable end-to-end).

**M2.3 `detector`** — depends: sensors, session-model, daemon-core. Files:
`src/detector/*.ts` (`loop.ts`, `manifest.ts`, `regions.ts`, `osc.ts`, `hooks.ts`,
`classify.ts`, `records.ts`, `index.ts`), `manifests/claude-code.toml`, `manifests/pi.toml`.
The one genuinely-new piece (PS#15a): an in-daemon **supervised** thread (daemon-core
`supervise.ts`; restart-isolation, not a split binary) classifying **exactly**
`blocked|working|done|idle`; internal `unknown` collapses to last-known (or `idle` at first
sight) and **never reaches the wire** (CLI-SURFACE §0). Two precedence-ordered strategies,
every record/event carrying `detector:"hook"|"scrape"` (§3.3): **Strategy 1 (hook,
AUTHORITATIVE)** — cooperative harness events posted by the installed hooks (layer 3) via the
internal `agent.hook_event` RPC; lifecycle map `running→working, idle→idle,
needsInput→blocked, unknown→scrape-fallback`; `UserPromptSubmit`/`Stop` gate the scraper to
active turns; hooks carry the resume/session ref for recover(). **Strategy 2 (scrape,
UNIVERSAL FALLBACK)** — throttled `kitty @ get-text` over the clean-room TOML manifest format
(herdr *format* reference, zero code lifted): `[[rules]]` with `id`, target `state`, integer
`priority` (highest wins), named `region`, predicates `contains/regex/line_regex/any/all/not`,
flags `visible_working/visible_blocker/visible_idle/skip_state_update`. **Region split by
mechanism (deep-pass A1):** grid regions (`whole_recent`, `after_last_horizontal_rule`,
`prompt_box_body`, `bottom_non_empty_lines(N)`) scope `get-text`; OSC regions (`osc_title`,
`osc_progress`) bind to `kitty @ ls foreground_processes[].title` + OSC progress — **never
get-text** — and serve as the zero-latency fast path checked before the grid read (Claude
Code's braille spinner). Two inherited laws: match invariant visible controls with explicit
AND/OR gates; never key off the user-scrollable viewport. **Scope: agent panes only** —
`is_viewer` panes are never in the manifest set, enforced here AND at `pane capture --source
detection` / `session.wait pane_output` (anti-loop #4). Emits `agent.detected`,
`agent.status_changed` (the SPINE), the convenience frames `agent.blocked`/`agent.done`
immediately after their status_changed, `agent.released`, **and `pane.output_matched` (§2.3)** —
**the detector is the SOLE emitter of `pane.output_matched`** (frozen-wire ownership ruling): it
fires the event both for its own region+regex scrape matches AND for every `WaitScrapeProvider`-
fulfilled `session.wait pane_output` read, because the component performing the matched read owns
the event and the detector holds the `kitty @ get-text` path. Payload is the §2.3 shape
`{pane_id, session_id, matched_line, read:{source, format, text, revision, truncated}}`; because
the detector performs the read it knows the 64 KiB `FRAME_CAP` framing, so it sets
`read.truncated=true` when the matched read hits the frame cap. **The detector writes the
`agents[]` snapshot leg into the single `model/store.ts` via the `Bus`** (through
`SnapshotSectionProvider` — issue-3 single-store ruling; it never becomes a second store), and
**registers the `WaitScrapeProvider`** that `daemon-core/wait.ts` calls to satisfy a
`session.wait pane_output` regex read (throttled `kitty @ get-text`, `is_viewer` rejected at the
seam) — the provider's match is emitted as `pane.output_matched` by the detector, and `wait.ts`
returns that same event as the `session.wait` RPC result (it consumes, never emits). Provisional cadence + shell policy per §1 of this plan (flags 4/5 — recorded,
re-rulable). `records.ts` holds only **detector-internal explain-data** (matched rule, manifest
source/version, strategy) retained per agent and surfaced via `agent.explain` — NOT an agent
store. Detector logs are TTL-prunable, never proof (PS#21 retention). Tests: manifest parsing + rule
precedence, fixture-grid classification per state for claude-code and pi, OSC fast path,
hook-over-scrape precedence + turn gating, unknown collapse, viewer exclusion, supervised
restart isolation (crash the loop, daemon lives), **`pane.output_matched` emission (fixture
grid + regex ⇒ the event on the stream with `read{source,format,text,revision,truncated}`,
including `truncated=true` at the 64 KiB `FRAME_CAP`, for both a scrape match and a
`WaitScrapeProvider`-fulfilled `session.wait` read)**.

**M2.4 `intake-gh`** — depends: taskchampion, journal, daemon-core. Files: `src/intake/*.ts`
(`gh.ts`, `poller.ts`, `signals.ts`, `map.ts`, `ratelimit.ts`, `index.ts`).
Direct `gh` CLI intake (PS#21, bugwarrior replaced) — **wired but OFF by default, opt-in
per-source**; auth is the ambient authenticated `gh`, tally never manages credentials
(DECISIONS Q8). Query surface **per the octo.nvim surface scan** (§5–6 of the scan): poll, in
priority order, (1) `gh api /notifications` — reasons `mention`, `review_requested`, `assign`
first-class; (2) `gh api graphql` search qualifiers `review-requested:@me is:open`,
`assignee:@me is:open`, `mentions:@me`, `author:@me` follow-ups; (3) the cheap per-tracked-item
`updatedAt` probe **before any full hydration** (octo's own two-phase polling pattern);
hydrate changed items with octo's core field set (reviewDecision, statusCheckRollup, isDraft,
labels, assignees, reviewRequests, subscription state). **Respect mute**: skip
UNSUBSCRIBED/IGNORED subjects and thread-level unsubscribes. Check `/rate_limit` headroom like
octo does; `--paginate` semantics on lists. `map.ts` turns a qualifying signal into a
TaskChampion row (`source=gh`, priority from signal class — review_requested > mention >
assign by default, config-tunable; dedup on node id so re-polls never duplicate rows) —
proving **cross-source urgency ranking** over the one store (a gh signal out-ranks the OCR
firehose, BUILD-SEQUENCE step 8). Read/poll half only — the mutation surface is future work
(scan §6). **Daemon mount (mechanism named, not guessed):** the daemon is the only persistent
process (the nix module ships no intake timer unit), so `intake-gh` depends on daemon-core and
**registers `poller.ts` on daemon-core's `supervise.ts` cadence host** at boot — the composition
root (`main.ts`) mounts it via the `DaemonMount` seam (§3 Seams); it runs supervised (restart-
isolated) like the detector loop and is a no-op while OFF by default. Tests against fake gh:
reason filtering, mute respect, two-phase probe (no hydration without delta), row dedup,
rate-limit backoff, OFF-by-default, poller mounts on the supervise host.

**M2.5 `triggers`** — depends: jobs, daemon-core. Files: `src/triggers/*.ts`
(`events-dir.ts`, `drain.ts`, `index.ts`).
The three-ingress trigger surface, **one queue, no path privileged** (PS#16b): (1) the
`events/` drop directory (`$XDG_STATE_HOME/tally/events/`) — watched + swept: each JSON file
is one Seam-A enqueue payload, validated, enqueued, then archived to `events/done/` (malformed
→ `events/rejected/` + journald entry); (2) systemd timers — `drain.ts` is the oneshot
entrypoint (`tally daemon drain`, invoked by the module's `Persistent=true` timer). **The drain
oneshot is a THIN SOCKET CLIENT, never a second engine:** it connects to the tally socket and
issues the internal-additive **`queue.drain` RPC** — which makes the **DAEMON** sweep `events/`
and re-present pending durable TW rows into the live queue. The oneshot process instantiates NO
jobs engine, NO queue, and NO lease client, so the one-queue invariant (PS#16b) and the single
`lease_epoch` source are preserved. If the socket is absent the oneshot **fails** (exit
non-zero); systemd retries at the next timer tick. `drain.ts`'s sweep/re-present logic
therefore runs **in-daemon** (invoked by the `queue.drain` handler); the `tally daemon drain`
verb only triggers it over the socket. (3) live socket-enqueue is Seam A itself (jobs module).
**No filesystem-drain codepath ever runs outside the daemon** — the events dir is an ingress
that produces ordinary in-daemon enqueues, never a queue (PS#1). **Daemon mount (mechanism
named, not guessed):** `triggers` depends on daemon-core and, at boot via the `DaemonMount`
seam wired by the composition root (`main.ts`, §3 Seams), **registers the `queue.drain` RPC
handler and its `events/` directory watcher on the daemon runtime**; the handler calls
`drain.ts`'s in-daemon sweep/re-present (which reaches the jobs engine through the ordinary
enqueue path), so `triggers` neither is imported by jobs nor re-implements a queue. Tests:
drop-file → job with
correct provenance `source`, malformed quarantine, `queue.drain` idempotence (double-drain
enqueues once), socket-absent oneshot fails cleanly, all three paths landing in one queue.

### Layer 3 — surface (CLI, hooks, nix)

**M3.1 `cli`** — depends: daemon-core (client side), jobs, witness, journal, taskchampion.
Files: `src/cli/*.ts` (`index.ts`, `client.ts`, `output.ts`, `queue.ts`, `session.ts`,
`pane.ts`, `agent.ts`, `query.ts`, `standup.ts`, `witness-cmd.ts`, `internal.ts`).
The **frozen §1 verb set, complete** — every verb a thin socket request, `--json` everywhere
(JSON is the contract, text a convenience), verb-prefix `tally <noun> <verb>` with the single
top-level alias `tally enqueue` (§0): `queue enqueue|cancel|pause|resume` with every §1.1a
flag incl. `--wait`/`--barrier`/`--wait-group`/`--wait-count`/`--timeout`/`--detach` and exit
code mirroring the verdict; `session list` (zmx-delegated enumeration join, `--short`,
`--workspace`) and `session watch` (snapshot-then-events, `--all/--snapshot-only/--since/
--format jsonl|tree` — the watch connection marks its pane `is_viewer` by reading
`$KITTY_WINDOW_ID` from its environment and posting `session.register_viewer {kitty_window_id}`
before subscribing); `pane
send|send-key|focus|capture` (capture `--source visible|recent|detection`, detection refusing
viewer panes; `--lines/--format`); `agent list|get|read|explain|wait|send|focus` (wait = the
exposed barrier primitive; **no `agent start`** — starting an agent IS enqueue, §4
divergence 1); `query status` (per-pool lease/queue depth + `protocol_version` — the ping),
`query log` (witness + journald merged feed, `--task/--session/--event/--since/--follow`),
`query render` (`--format text|json|jsonl|tree|jcal`, `--scope sessions|queue|witness`,
`--collapse`), `query standup` — `standup.ts` implements the **four-log read-time join**
`task export × journalctl -t tally -o json × git log × harness JSONL`, keyed on `session_ref`
/ `TALLY_TASK_UUID` (SPEC "The four-log read-time join"; git scoped to public proof-of-work;
the harness JSONL is *pointed at* — path + existence + ids only, **never copied**), output
`{window, completed[], in_flight[], reused, gate_fails}` in `text|json|md`. Plus `tally
witness verify [--ledger <path>]` (SPEC hash-chain: daemonless, any copy) and the internal
verbs `tally daemon run|drain` (`drain` is a **thin socket client** posting the `queue.drain`
RPC — the daemon does the sweep; it fails if the socket is absent, per M2.5), `tally pls-wrap`,
`tally hooks install`. Selector grammar
(`<session>:<pane>`, agent_id, bare pane) in one resolver. `--help` exposes the §1 verb tree
(BUILD-SEQUENCE step 1 acceptance). Tests: arg-parse table for every verb, JSON output golden
shapes per the §1 tables, standup join against fixture logs, verdict-mirroring exit codes,
witness verify CLI on a tampered fixture ledger.

**M3.2 `hooks`** — depends: contracts (payloads talk to the socket directly). Files:
`src/hooks/installer.ts`, `hooks/claude-code/tally-hook.ts`, `hooks/pi/tally-session.ts`.
The **module-owned cooperative-hook installer** (CLI-SURFACE §5 flag 2 CLOSED; SPEC boundary
"tally SHIPS"; DECISIONS Q5): Strategy-1 authoritative detector input. `tally hooks install
[--kind claude-code|pi] [--dry-run]` — idempotent, cooperative (merges, never clobbers
foreign hooks), and invoked by home-manager activation from the nix module. Claude Code:
register `UserPromptSubmit`/`Stop`/`SessionStart`/`Notification` hooks in the CC settings
hooks schema, each invoking `hooks/claude-code/tally-hook.ts` (compiled alongside; posts
`agent.hook_event {kind, lifecycle, session_ref, cwd}` NDJSON to the tally socket; exits 0
fast and silent when the socket is absent — the harness must never block on tally). pi:
install `tally-session.ts` into `~/.pi/agent/extensions/` / `$PI_CODING_AGENT_DIR/extensions/`
(the pi extension mechanism, §3.3/§3.4 — interface per docs, the vendor pin is stale), posting
the same lifecycle events + the `pi --session <id>` resume ref. Tests: installer idempotence
+ merge-not-clobber on fixture settings, payload NDJSON shape, socket-absent fast-exit.

**M3.3 `nix-module`** — depends: scaffold (incl. `packages.pls` — the `flake = false` pls wrap
scaffold's `nix/pls.nix` exports, M0.1), pls (pool config rendering), hooks, sensors
(watcher script path). Files: `nix/hm-module.nix`, `nix/nixos-module.nix`, `nix/units.nix`.
**`nix/hm-module.nix` and `nix/nixos-module.nix` OVERWRITE the layer-0 scaffold placeholders**
(the empty-module bodies scaffold created so the flake evaluates at layer 0 — the "scaffold
creates, nix-module overwrites" handoff in M0.1); `nix/units.nix` is net-new.
`homeManagerModules.tally` — the **primary**, load-bearing module (typed options in →
generated artifacts out; microvm.nix-*shape*, never a dependency; FS§5). Option surface
**minimal** (PS#17): `enable` · `role = "conductor"|"receiver"` (daemon runs only on
conductor) · `conductorHost` (pure configuration — **no hostname frozen anywhere**, DECISIONS
Q9) · `sessions` · `package` — plus the two options concrete needs already pull on:
`watcherScript` (**read-only** export of the kitty watcher store path so the dotfiles-owned
kitty.conf line never rots, DECISIONS Q4) and `intake.gh = { enable (default false),
sources }` (PS#21: wired in the module, shipped OFF). **`sessions` — provisional minimal
semantics (no type/semantics is defined anywhere in the docs, so this plan rules one, flagged
for Tom's re-ruling exactly like the detector flags 4/5 in §1):** typed as
`types.listOf types.str`, **default `[]`**, meaning a list of zmx session-name globs the
daemon scopes discovery to; **`[]` = observe ALL enumerated sessions** (no scoping). It is
rendered into the daemon config JSON (`config.sessions`) and read by `model/discovery.ts` to
filter the `zmx list --short` universe; a **typed no-op is NOT acceptable here** — the option
carries real (if minimal) discovery-scoping semantics so the module surface matches PS#17's
enumerated surface. RE-RULE FLAG: Tom may narrow/redefine `sessions` (e.g. per-workspace maps)
without a protocol bump — this is pure daemon-config. Generated systemd **user** units
(linger-compatible, `default.target`): `tally-daemon.service` (`ExecStart=tally daemon run`,
**`StandardOutput=journal`**, `SyslogIdentifier=tally`, `Restart=always`, runtime dir for the
socket), `tally-drain.timer` (`Persistent=true`) + `tally-drain.service` oneshot, the pls
broker unit(s) (`ExecStart` = the scaffold-owned **`packages.pls`** store path, M0.1 — never a
guessed derivation) + rendered pool config (tally owns pls config, PS#5/OUV-CM), an epoch-counter
`ExecStartPre` increment (PS#21 lease-epoch backstop), and the ambient `pls-lease-wrap`
helper on PATH (SPEC "The ambient default"). PATH wiring for runtime deps: `task` (pinned
taskwarrior 3.x via `pkgs.taskwarrior3`), `pls` (the scaffold `packages.pls` wrap), `gh`, `zmx`
assumed ambient. Ships **no** zmx/receiver/kitty config
(boundary). `nixosModules.tally` = the ruled unbuilt thin stub. Tests: `nix flake check`
module eval with both roles + assertions (receiver ⇒ no daemon unit; conductorHost required
when enabled).

**M3.4 `dev-rig`** — depends: scaffold. Files: `nix/dev.nix`, `dev/process-compose.yaml`,
`dev/mock/*` (mock job scripts, sample events/ payloads, fixture kitty/zmx/task/pls shims for
a live local boot). **`nix/dev.nix` OVERWRITES the layer-0 scaffold placeholder** (the trivial
no-op `apps.dev` scaffold created so the flake evaluates at layer 0 — the "scaffold creates,
dev-rig overwrites" handoff in M0.1) with the real process-compose rig. `nix run .#dev` boots
the daemon against mock jobs via process-compose
(SPEC "Inputs & dev rig"; BUILD-SEQUENCE step 1 acceptance) — daemon + fake brokers + a
scripted enqueue exercising the full job lifecycle on a laptop with no GPU/worker. Production
stays systemd user units. Tests: a smoke script asserting the rig's daemon answers
`session.snapshot` and completes one mock job.

### Layer 4 — integration

**M4.1 `e2e`** — depends: everything. Files: `test/e2e/*.test.ts`, `test/e2e/helpers.ts`.
Full-path integration under `bun test`, real compiled binary where practical: (1) **the
OCR-drain rehearsal** — enqueue N shell-kind batch jobs with `--evidence artifact:… hash:…
exit:0` + `--dedup-key`, one fake worker-gpu lease serializing them, artifacts written,
witness lines chained, TW rows completed with `trust:unreviewed`; re-run skips all N as
`reused` and excludes them from canonical GPU-seconds (BUILD-SEQUENCE steps 3+5 acceptance —
this is the shape Tom's first test-drive replays on live hardware); (2) evidence-fail
forensics (clean exit, no artifact ⇒ `clean-exit-no-artifact` + `evidence_fail`); (3) kill
-9 the daemon mid-flight ⇒ recover() re-presents, chain head survives, cursor-voiding epoch
bump observed by a reconnecting subscriber; (4) `enqueue --wait` + a 3-job barrier via
`--wait-group`; (5) detector fixture pass: fake grids drive
blocked/working/done transitions onto one stream a `session watch` client and an `agent wait`
both consume; (6) `tally witness verify` detects a byte-flipped line by exact seq; (7)
`query standup` reconstructs the run from `session_ref` alone against fixture logs; (8)
`tally --help` verb-tree golden test; (9) compile smoke: `bun build --compile` output runs
`query status` against the test daemon.

---

## 3. Shared contracts (layer 0, `src/contracts/` — the agreement surface)

Everything parallel implementers must agree on, defined ONCE in M0.2:

- **Wire (CLI-SURFACE §2, FROZEN):** `Frame = Request {id, method, params} | Response {id,
  result|error} | Event {seq, id, event, …payload}`. **The `id` on Event frames is the stable
  event uuid (§2.1) carried alongside `seq` on every replayable event** — encode it in
  `src/contracts/wire.ts` as a first-class field of the Event type, NOT prose (the daemon-core
  ring stamps both). Method names `session.snapshot|subscribe|wait|ack|unsubscribe` + internal
  additive `kitty.watcher_event`, `agent.hook_event`, `queue.*`, `pane.*`, `agent.*`,
  `query.*`, `session.list`, `session.register_viewer` RPC carriers (full inventory below);
  **`SubscribeAck` carries the literal discriminator field `type:"subscription"` (§2.4,
  FROZEN — do not drop it):** `SubscribeAck {type:"subscription", subscription_id,
  protocol_version, epoch, resume:{after_seq, oldest_seq, latest_seq, next_seq, gap}}`;
  `Snapshot` exactly per §2.2 (workspaces/sessions/panes/agents/jobs + focus + protocol
  header); full event-name union + payload types per §2.3 (`agent.detected/status_changed/
  blocked/done/released`, `pane.created/closed/focused/output_matched`, `session.observed/
  ended`, `workspace.focused`, `job.enqueued/dispatched/started/heartbeat/preempted/resumed/
  evidence_pass/evidence_fail/completed/failed/witness_emitted`, `heartbeat`,
  `stream.overflow`) with optional `prev_*` shadow fields; consumers MUST ignore unknowns.
  **Byte-for-byte golden tests (a constants/shape table in `testkit` asserted by contracts'
  own suite) MUST pin: the `SubscribeAck` `type:"subscription"` discriminator, the Event
  `{seq, id, event, …}` field order/names, the full §2.2 snapshot shape, and every RPC method
  name below** — so no omission of the discriminator, the event `id`, or a method name can
  survive a green build (this operationalizes the plan's rule "any deviation from §2 field
  names is a build failure").
  - **`session.wait` agent predicate accepts `until_status` over all four `AgentStatus` values**
    — the frozen §2.4 example shows `done|blocked`, but accepting `idle|working` too is an
    additive widening of an accepted param per §2.5, **required** so the equally-frozen §1.4
    `tally agent wait --status <done|blocked|idle|working>` surface is fully servable (`agent
    wait` routes through `session.wait`, no separate method). Encode the predicate against the
    full four-value `AgentStatus` enum in `wire.ts`, and golden-test the four-value acceptance
    so a narrowing to the §2.4 two-value literal is a build failure.
- **RPC method inventory (the full internal-additive set, param/result types defined in
  `src/contracts/wire.ts` and golden-tested by name).** The FROZEN §2.4 public five —
  `session.snapshot`, `session.subscribe`, `session.wait`, `session.ack`, `session.unsubscribe`
  — plus these **internal-additive** carriers (adding one is NEVER a protocol bump, §2.5;
  each is the socket carrier for a CLI verb or a sensor edge, so the parallel cli / daemon-side
  implementers share one name/param contract instead of guessing):
  - **queue.** — `queue.enqueue` (Seam-A params/result), `queue.cancel {task_uuid, force?}`,
    `queue.pause {pool?}`, `queue.resume {pool?}`, **`queue.drain {}`** (triggers the in-daemon
    events/ sweep + TW re-present — see triggers module; the drain oneshot is a thin client of
    exactly this method).
  - **pane.** — `pane.send {pane, text}`, `pane.send_key {pane, keys}`, `pane.focus {pane}`,
    `pane.capture {pane, source, lines?, format?}` (source `detection` rejects `is_viewer`).
  - **agent.** — `agent.list {…filters}`, `agent.get {agent_id}`, `agent.read {agent_id, …}`,
    `agent.explain {agent_id}` (returns the retained matched-rule / manifest-source / strategy
    explain-data). `agent wait|send|focus` route through `session.wait` / `pane.send` /
    `pane.focus` (no separate method).
  - **query.** — `query.status {}` (per-pool lease/queue depth + `protocol_version` — the ping),
    `query.render {format, scope, collapse?}`. `query log` / `query standup` read journald +
    ledger + the four logs directly (no daemon RPC).
  - **session.** — `session.list {workspace?, short?}` (zmx-delegated enumeration join),
    **`session.register_viewer {kitty_window_id}`** — the mechanism by which a `session watch`
    client marks its OWN pane `is_viewer=true`: the client reads `$KITTY_WINDOW_ID` from its
    environment and posts this RPC before/at subscribe, so the detector and `pane capture
    --source detection` / `session.wait pane_output` exclude it (anti-loop invariant #4). No
    such method existed in the frozen §2.4 set; it is internal-additive, added here.
  - **sensor edges** — `kitty.watcher_event` (from `tally-watcher.py`) and `agent.hook_event`
    (from the cooperative hooks), both internal-additive as already recorded.
- **Constants:** `PROTOCOL_VERSION=1`, `REPLAY_RING=4096`, `FRAME_CAP=65536`,
  `MAX_UNACKED=1024`, `HEARTBEAT_MS≈15000`, socket path
  `$XDG_RUNTIME_DIR/tally/tally.sock`, ledger/events/epoch paths.
- **Enums:** `AgentStatus = blocked|working|done|idle` (exactly four — a fifth is a protocol
  bump); `AgentKind = pi|claude-code|shell`; `Priority/TALLY_CLASS = high|medium|low`;
  `Source = r2|gh|calendar|manual|orchestrator`; `Verdict` incl. `pass` and
  `clean-exit-no-artifact`; `LaborClass = fresh|recovered|reused`; `Trust =
  unreviewed|reviewed|recalled`; `DetectorStrategy = hook|scrape`; `TallyEvent` (the journald
  vocabulary the `job.*` events mirror verbatim).
- **Witness:** `WitnessRecord` (all 17+ fields incl. `pool`, `charge {unit, amount, class}`,
  `model`, `seq`, `prev_hash`, `hash`) + the 5-field projection type + hash-input
  canonicalization rule (line's own JSON with `hash` cleared).
- **Journald:** `TallyFields` matrix (field → required-at stage). **`TALLY_AGENT` uses the
  SPEC's short vocabulary `pi | cc | shell | <worker>` (SPEC journald table), NOT the
  `AgentKind` spelling** — so `src/contracts/journal.ts` defines an explicit
  `AgentKind → TALLY_AGENT` mapping in one golden-tested function: `claude-code → "cc"`,
  `pi → "pi"`, `shell → "shell"` (and a raw worker label passes through as `<worker>`). Both
  the `emit.ts` writer and the `query log`/standup reader/join go through this function, so the
  journald table and the reader never disagree on whichever spelling an implementer would
  otherwise guess.
- **Seam A:** `EnqueueParams` / `EnqueueResult {task_uuid|null, lease_epoch, pool, status,
  session_ref, dedup_key, witness_lsn, verdict}`; `EvidenceCheck = artifact:<path> |
  hash:<algo> | exit:<code>` (witness-span implicit).
- **Keys:** the three never-conflated keys (`persistence_session_id`, `kitty_window_id`,
  `session_ref`); pane composite id `"<session>:<pane>"`; selector grammar.
- **TW:** UDA name/type table; the durable-row admission predicate signature.
- **Seams:** `Exec` (injectable subprocess runner — the only way any module shells out),
  `Bus` (typed in-daemon pub/sub), `SnapshotProvider`, `Clock`, plus the
  **`DaemonMount`/`RpcRegistrar`** seam and two composition seams
  that resolve the snapshot/wait cross-layer joins **without** session-model importing its
  layer-2 siblings:
  - **`DaemonMount` (`RpcRegistrar`)** — the boot-time registration seam by which the
    composition root (`main.ts`) wires each in-daemon module's exported handlers and loops into
    daemon-core, so a module never has to import daemon-core's internals nor be imported by a
    sibling to get mounted. It exposes `registerRpc(method, handler)` (additive RPC carriers),
    `registerWatcher(path, handler)` (directory watchers), and `registerSupervised(loop)` (the
    `supervise.ts` cadence/restart host). **Who mounts what:** `triggers` (M2.5) registers the
    `queue.drain` RPC handler + its `events/` watcher; `intake-gh` (M2.4) registers its
    `poller.ts` as a supervised loop; the detector (M2.3) already rides `supervise.ts` through
    this host; jobs/session-model register their own carriers here too. `main.ts` is the single
    place that calls each module's `mount(daemon)` at boot — one composition root, no
    daemon-core deps leaking into the mounted modules beyond this typed seam.
  - **`SnapshotSectionProvider`** — a registration seam alongside `SnapshotProvider`. The
    **single store ruling:** `model/store.ts` is the ONE authoritative session store;
    **detector and jobs do NOT own snapshot sections — they WRITE into the store via the
    `Bus`** (detector writes the `agents[]` leg from its records, jobs writes the `jobs[]` leg
    from lifecycle), and `session-model/snapshot.ts` composes the §2.2 frame from the single
    store it owns. `detector/records.ts` is therefore **demoted to detector-internal
    explain-data** (the retained matched-rule/manifest/strategy behind `agent.explain`), NOT a
    second agent store. `SnapshotSectionProvider` is the typed interface a section-writer
    registers so the store knows which leg it feeds; the frame assembly reads only the store.
  - **`WaitScrapeProvider`** — the seam `daemon-core/wait.ts` uses to satisfy a
    `session.wait pane_output` predicate: wait.ts requests an on-demand pane_output regex read
    (a typed request, or a `bus` request event) that the **detector loop** fulfills via the
    throttled `kitty @ get-text` path, honoring the `is_viewer` rejection at the seam. wait.ts
    never imports sensors/detector; it depends only on this provider being registered.
- **Config:** daemon/CLI config shape (`conductorHost`, broker addresses, pool table, intake
  toggles, detector cadence overrides) — rendered by the nix module, readable from
  `$XDG_CONFIG_HOME/tally/config.json`.

---

## 4. Risks & recorded implementation choices

1. **journald structured fields under stdout capture:** `StandardOutput=journal` records
   stdout as `MESSAGE` under `SYSLOG_IDENTIFIER=tally`; the TALLY_* fields therefore ride as
   a single-line JSON `MESSAGE` payload and `reader.ts`/the join parse them back out. This is
   the two-line workaround of record (SPEC "Emission path"); native emission is the recorded
   flip-back pull, not v0's problem.
2. **prev_* from the op-log:** with in-process TaskChampion ruled out, the attribute delta is
   captured by pre-mutation `task export` (see M1.3) — semantically the op-log's computed
   delta at the only legal access altitude.
3. **Detector open flags 4/5:** provisional defaults recorded in §1; flagged for Tom, cheap to
   change (config values + one manifest rule).
4. **pi vendor pin stale:** adapters bind to the documented interface (`pi --session`,
   extensions dir); re-pin is CLI-SURFACE §5 flag 3, not a build blocker.
5. **bun2nix offline build:** bun2nix is a **Nix-distributed codegen binary** (the `bun2nix`
   flake input), NOT an npm dependency — it is not on PATH inside `nix shell nixpkgs#bun`, so
   `bun run bun2nix` cannot invoke it directly. Wired via the `apps.bun2nix` output: regenerate
   with **`nix run .#bun2nix`** (the `package.json` `bun2nix` script execs the same), commit the
   resulting `bun.nix` whenever `bun.lock` changes or `nix build .#tally` fails; keep runtime
   deps near-zero to minimize churn.
6. **Chain scope:** ledger-wide chain chosen (M1.2) where SPEC left ledger-wide vs per-job
   open; recorded so a future per-job flip is a deliberate change.
7. **No code from `vendor/`:** herdr (AGPL) and cmux (GPL) are interface references only;
   e2e includes no lifted text, and the plan's manifests are authored fresh against the
   documented format.
8. **`claude -p` contingency mechanism (recorded, not guessed):** `--via-terminal` uses
   `kitty @ launch` confined to one function in `src/agents/claude-p-contingency.ts` — the sole
   carve-out in the M1.6 boundary grep-test; windows sit outside the zmx substrate (DECISIONS
   Q6). Gated off by default per §1 item 9.
9. **Single-store composition (issue-3 ruling):** `model/store.ts` is the one session store;
   detector/jobs write the `agents[]`/`jobs[]` legs into it via the `Bus`/`SnapshotSectionProvider`
   rather than session-model importing its layer-2 siblings; `detector/records.ts` is demoted to
   explain-data. `daemon-core/wait.ts` reads `pane_output` via the `WaitScrapeProvider` seam.
   A future flip to per-section stores is additive.
10. **`TALLY_AGENT` short vocabulary:** journald writes the SPEC's `pi | cc | shell | <worker>`;
    `contracts/journal.ts` owns the golden-tested `AgentKind → TALLY_AGENT` map (`claude-code → cc`)
    used by both writer and reader/join.
11. **`sessions` module option (provisional):** typed `listOf str`, default `[]` = observe all;
    a list of zmx session-name globs the daemon scopes discovery to (rendered to `config.sessions`,
    read by `model/discovery.ts`). Flagged for Tom's re-ruling like flags 4/5; pure daemon-config,
    no protocol bump.
12. **nix layer-0 placeholders → layer-3 overwrite:** scaffold creates minimal valid
    `nix/hm-module.nix`/`nix/nixos-module.nix`/`nix/dev.nix` so the flake evaluates at layer 0;
    `nix-module` (M3.3) and `dev-rig` (M3.4) overwrite them — one final writer per file, handoff
    recorded in M0.1/M3.3/M3.4.

---

*This plan is the build map; SPEC.md / CLI-SURFACE.md / DECISIONS.md remain the authority
wherever a description here compresses them. The docs win.*
