# tally — build sequence

> gated on the crystallized rulings (docs/DECISIONS.md); each step one sitting.
> **Stack, ruled 2026-07-09: daemon + CLI are TypeScript on Bun** — `packages.tally` is ONE Bun-compiled
> binary (`bun build --compile` + `autoPatchelfHook`, bun2nix; Backlog.md's maintained flake is the live
> reference). The frozen wire contract (CLI-SURFACE §2) is language-neutral and unchanged. TaskChampion
> access is `task export`/`import` shell-out; journald via `StandardOutput=journal` (see SPEC.md).
> Sequencing, same ruling set: **a given build pass builds the CLI only; the non-interactive front-end
> substrate comes at the END** (step 10).

Ordered, one-sitting-shaped steps. Each names what it lands and the acceptance shape that proves it done. The tier-router step is **dropped** (PS#2: model class is chosen at ignition by the orchestrating model and carried as a declared task property — tally never escalates). The OCR drain is a **first-class** scheduled job on the same spawn-tracked-agent-job primitive as any workflow — no filesystem-drain codepath. The multi-pool substrate lands **after** the single-GPU meter + witness that serves as its reference implementation.

## 0. Parts-bin sonnet agent (build-start, runs in parallel)

Spawn a dedicated Sonnet "parts-bin" agent at the very start to compile + annotate rotation/admission mechanics from prior art — **clean-room inspiration only, never lifted code** — and map each part onto the pls pool interface:

- teamclaude anthropic-ratelimit-unified-* header-metering + 98%-threshold round-robin;
- LiteLLM zero-cost-fallback budget routing;
- ccusage / Usage-Monitor programmatic-vs-interactive two-meter split;
- herdr per-harness TOML region+regex state manifests (detector reference); cmux harness-hook state (cooperative second strategy).

Output is an annotated parts inventory keyed to the pls pool interface and the in-daemon detector, feeding steps 2, 5, and 9. **Lands:** OUV-CM prior-art map, OUV-MH R3 references. **Acceptance:** a written parts inventory exists mapping each mechanism to a tally seam, with no code copied.

## 1. Repo bootstrap

The layer-0 scaffold under the Bun ruling (IMPLEMENTATION-PLAN M0.1 `scaffold`, M0.2 `contracts`, M3.3 `nix-module`, M3.4 `dev-rig`): stand up the one Bun-compiled binary as daemon + CLI and the flake that packages it, so every later step lands into a green tree. Chunk plan:

- **TS/Bun project layout.** `package.json` (scripts `typecheck`=`bunx tsc --noEmit`, `test`, `build`=`bun build --compile --outfile=dist/tally src/main.ts`, `bun2nix`), `tsconfig.json`, `bunfig.toml`; near-zero deps (`typescript`, `bun-types`, one TOML parser — `smol-toml`) to keep the binary single-file-compilable; `dist/` + `node_modules/` into `.gitignore`. bun is not installed on this box — all invocations run `nix shell nixpkgs#bun --command bun …` (bun 1.3.13 verified).
- **Single entry, no daemon logic yet.** `src/main.ts` dispatches `argv[1] === "daemon"` → daemon boot vs. CLI, via a tiny registry so layer 0 compiles standalone; the socket server is a **`Bun.serve({ unix })`-style** unix listener at `$XDG_RUNTIME_DIR/tally/tally.sock` (mode 0600, NDJSON framing) whose real §2 RPC surface is filled by M1.1 — here it answers `--help` (the frozen §1 verb tree) and a stub `session.snapshot`.
- **Package via bun2nix.** `flake.nix` on **flake-parts**; `packages.tally` = `bun build --compile` + `autoPatchelfHook` fed by a committed `bun.nix` regenerated from `bun.lock` (`nix run .#bun2nix`) — the **Backlog.md maintained-flake pattern** is the live reference (no `bun run --bun` wrapper, no bespoke release-binary derivation). Named substrate inputs pinned each on its own trigger: `nixpkgs`, `flake-parts`, `bun2nix`, `process-compose-flake`, `pls`, `pi` (`flake = false`, interface-bound — pin is stale), `llama-swap`, `taskwarrior` (3.x, `pkgs.taskwarrior3` fallback); `gh` from nixpkgs, ambient auth.
- **Modules + dev rig.** Outputs `packages.tally`, `homeManagerModules.tally` (option surface `enable`/`role`/`conductorHost`/`sessions`/`package` + read-only `watcherScript`; generated units are M3.3's, referenced not built here), `nixosModules.tally` = the ruled **unbuilt thin wrapper stub**, `apps.dev`, `checks`. `nix run .#dev` boots the daemon against mock jobs via **process-compose** (`process-compose-flake`), no GPU/worker needed. Dotfiles pin tally as an input (`inputs.tally.url = "github:mecattaf/tally"`, `inputs.tally.inputs.nixpkgs.follows = "nixpkgs"`).

The flake-input + homeManagerModule consumption pattern is **ratified as THE packaging channel** (boundary ruling 2026-07-09 — no bespoke dotfiles `pkgs/tally.nix` release-binary derivation).

**Lands:** PS#17, FS§7, repo identity; IMPLEMENTATION-PLAN layer 0 + M3.3/M3.4. **Acceptance:** `nix shell nixpkgs#bun --command bun run typecheck` and `bun test` are green; `nix build .#tally` produces the compiled binary and `nix flake check` passes (both roles eval); a dotfiles host builds with tally imported and `tally --help` is on PATH; `nix run .#dev` boots the mock daemon and answers `session.snapshot`.

## 2. pls as the box governor (two GPU pools)

**pls IS the per-box governor** (decided, PS#5) — one broker per box, no in-daemon lock, no bespoke slot-lock service, no wrapper. Land the pls lease as the sole GPU-ownership primitive (RAII / process-exit / systemd-teardown release; recover() reclaims a holderless lease); every heavy tenant — tally or not (ds4-server, OCR vLLM) — acquires it **directly** at its declared priority. The VRAM/MemAvailable gate is a pls **budget pool** (`--cost`=est-VRAM-GB). tally owns the pool config and is the highest-priority client; ship the ambient "every heavy invocation → pls-lease-wrapped" default.

**GPU=2, worker prioritized:** one pls pool per GPU (controller-GPU, worker-GPU), single-lease-per-pool; heavy work targets the headless worker. tally runs on the controller as a client of **both** boxes' pls brokers (worker reachable over the TB3/tailnet). **DS4** is the one cross-box job: an atomic co-allocation of a heavy worker-GPU hold + a light controller-GPU spill. The controller NPU is out of the model (fringe utilities only).

**Lands:** PS#5 (governor = pls; GPU=2 worker-prioritized; DS4 co-allocation), ambient pls-wrap default. **Acceptance:** two competing invocations serialize on one pool's lease; process death frees it; a non-tally VLM invocation also gates directly; a DS4 dispatch co-allocates worker+controller or queues.

## 3. OCR drain as a first-class job

Dispatch it through the ONE spawn-tracked-agent-job primitive: a TaskChampion row (batch/queued class) → pls lease → deterministic OCR worker **pinned to the headless worker node** → artifacts to `papers/<uuid>/` in git. Trigger surface = `events/` drop + systemd timer + live socket-enqueue (PS#16b). **No filesystem-drain codepath** — OCR is one workflow among any (OCR, chromium build, pi, cc); if it is not as easy to schedule as any agent job, the mechanism is wrong.

**Lands:** PS#1, PS#16, PS#20, the spawn-tracked-agent-job. **Acceptance:** enqueue via all three trigger surfaces; OCR runs only on the worker; the TaskChampion auto-log records what papers were processed and when.

## 4. Journald TALLY_* schema wired in

The drain unit emits the pinned structured fields including `TALLY_ATTEMPT`, `TALLY_LEASE_EPOCH`, `TALLY_LABOR_CLASS` (PS#11) and `TALLY_SESSION_REF`. Ship the read-time join query — `task export × journalctl -t tally -o json × git log × harness JSONL`, keyed on `TALLY_TASK_UUID` / `session_ref`. No bespoke audit log.

**Lands:** PS#11, FS§4, the four-log read-time join. **Acceptance:** every transition emits one structured journald entry queryable as JSON; the join reconstructs a run's history from `session_ref` alone.

## 5. Witness ledger emitter + evidence gate

Full canonical on-disk witness record (PS#9: task_uuid, transition_timestamp, verdict, exit_code, artifact_content_hash, gpu_seconds, wall_clock, attempt, lease_epoch, dedup_key, labor_class, optional trace_ref) with `pool` + trust-class-tagged `charge = {unit, amount, class}` reserved **additively** from day-1 (GPU-only populated, so step 9 needs no migration). Plain `O_APPEND` + fsync per line (PS#10). Terminal commit gated on **artifact-exists ∧ content-hash ∧ exit-code ∧ witness-span**, never self-report (PS#9, amended 2026-07-09 — the ledger records job start/end natively; timewarrior removed). GPU-seconds derived from the witness span as the **sole** proof-of-labor meter. Dedup-by-existence: stat + grep witness/artifact pre-run, re-hash only on mtime/size mismatch; skips tagged `labor_class=reused`, excluded from canonical GPU-seconds. recover() = re-present never replay, single monotonic lease-epoch the only fence (pls generation backstopped by a daemon-incremented counter file — 2026-07-12, issue #9: was systemd-incremented; the ExecStartPre bump was removed, the daemon is the sole writer); the 5-field form is a public projection.

**Lands:** PS#9, PS#10, PS#18, PS#21, witness/evidence/meter/recover. **Acceptance:** a torn final write is discarded on replay; the gate rejects a self-reported success with no artifact (`verdict=clean-exit-no-artifact` + `TALLY_EVENT=evidence_fail`); GPU-seconds land on the witness line; a re-run skips an already-transcribed paper and excludes the skip from GPU-seconds.

## 6. Session model + agent-state detector (over the dotfiles zmx env)

tally **assumes** the dotfiles zmx substrate (receiver-everywhere, create/name, reattach, the pickers — see dotfiles #38) and builds on top of it — it ships **no** receiver config. Land: the session data model `{session, pane → (persistence_session_id, kitty_window_id, agent{kind, status})}` read from the existing zmx sessions; the delta-stream unix socket (snapshot + newline-JSON events + control) using herdr's clean-room verb vocabulary; a Workspace → Session → Pane collapsible grouping tier with status dots aggregated up (PS#6). The agent-state detector as a **supervised thread inside the one daemon** (PS#15), classifying blocked/working/done/idle out-of-band via throttled `kitty @ get-text`, **scoped to agent panes only** (never reads a `tally watch` viewer pane). tally owns **only** the minimal kitty-native binding + the delta stream — no dashboard, no tab-bar (PS#13c; rendering is external — CUBS, and the dotfiles picker's #38-Q4 agent-state preview is a consumer of this same detector).

**Lands:** PS#13 (minimal binding only), PS#15, PS#6; the detector→delta-stream seam the dotfiles picker consumes. **Acceptance:** a dispatched job and a supervised session appear as one object two ways on one delta stream; the detector never reads a viewer pane; the dotfiles picker can show live agent-state off the stream.

## 7. Compose with the dotfiles conductor-receiver env

The conductor-receiver environment (receiver-everywhere, `zmx` reattach over kitten-ssh/Tailscale, `mod+enter` escape) is **already provided by the dotfiles** — tally does not generate it. This step verifies tally **composes** correctly into it: the daemon runs on the controller; the headless worker stays the terminal-less oneshot batch path; tally's detector + dispatch + delta stream work across controller / worker / zenbook-duo. The microvm.nix-**shape** module generates only tally's own systemd user units + pls pool config + CLI (minimal option set: `enable`/`role`/`conductorHost`/`sessions` — no `persistence.backend`, since tally ships no persistence).

**Lands:** FS§5, PS#17, the tally/dotfiles boundary. **Acceptance:** tally's daemon runs inside a dotfiles-provided conductor zmx session; the duo (reattaching a conductor session over Tailscale) sees live tally agent-state; the worker batch path runs headless with no terminal.

## 8. Second intake source — direct gh CLI

Direct `gh` CLI intake: GitHub @-mention → TaskChampion row (bugwarrior REPLACED 2026-07-09), wired in the module but **OFF by default**, opt-in per-source via an agent/mention filter. The intake's query surface (which gh queries/verbs it polls, which @-mentions/labels are signals) comes from the queued **octo.nvim surface scan** dispatchable — the intake mirrors the operator's actual daily GitHub workflow, not a guessed one. Proves cross-source urgency ranking (a gh-signaled fix vs the OCR firehose) over the ONE canonical store. Optionally add the off-critical-path gws read-only Google Calendar **view** here (human-layer only; not a presence/scheduling/intake input). No email bridge; newsboat/notmuch deferred; no Baikal.

**Lands:** PS#21 (gh-CLI intake, no-Baikal), PS#4 (gcal view), PS#8, cross-source urgency. **Acceptance:** a gh @-mention enqueues a task that out-ranks the OCR batch on urgency; the gcal view never gates presence.

## 9. Multi-pool compute-budget substrate

Lands **after** the single-GPU meter + witness (its reference implementation). Each compute model is a pool = `{budget/capacity, gate/lease, meter}`; pls is the pool primitive, one broker per pool; tally absorbs pls's configuration role. One interface (ask → gated → metered), two admission predicates (co-residency for GPU/VRAM; windowed-consumption for subscriptions). The selfaware poller becomes a `Restart=always` systemd meter daemon reading the **programmatic** budget; GO/SLOW/STOP becomes a mechanical admission gate via `cc-usage --check` exit codes (0/1/2/3) — **no LLM in the pacing loop**. The **pool-assigner** (strictly below model choice) picks the concrete account/lease for an already-chosen model class: round-robin over GO pools, fall back to the local GPU when all subscription windows STOP, route the API-dollar pool only on an explicit budget UDA. Witness `pool` + trust-class-tagged `charge` populated (GPU = verifiable proof; tokens/USD = annotation, never the proof slot).

**Lands:** OUV-CM r1/r2/r3/r4/r5. **NOTE:** budget-gated pool **assignment** only — NOT a model-tier router (PS#2). **Acceptance:** a STOP'd account is skipped mechanically; the local GPU serves as fallback; zero model tokens are spent pacing.

## 10. CUBS delta-stream consumer

External to the tally repo; unblocked once the delta-stream contract is frozen. CUBS/COWL picks up the session/agent-state model over the delta stream as a pure subscriber. Web viewer is gated/deferred (PS#7): Tom's ambitious plans leverage tally's **output formats** (taskchampion/jCal/witness), not a front-end/back-end coupling.

**Lands:** PS#7 (gated), PS#13c, CUBS consumer. **Acceptance:** the chromium-views tab-bar renders live agent state as a pure delta-stream subscriber; tally ships no renderer.

> **Ruled 2026-07-09 — sequencing + renderer shape.** A given build pass builds the **CLI only**; the **non-interactive front-end substrate comes at the END** — this step's territory. The renderer shape stands: static TypeScript browser-tab bundle · `tally bridge` read-only method allowlist (`{session.snapshot, session.subscribe, session.ack, session.unsubscribe}`) · Anthropic dark-ground register · platform-kit five-component read-only subset. **Pure rendering is the initial-release posture, not a vow** — the load-bearing property is "no mutation capability"; interactivity may arrive later verb-by-verb via bridge-allowlist additions, each a ruling. Shape, not schedule: the front-end need not be built now.

## Next workflows

1. **(this)** BUILD-SEQUENCE crystallized.
2. **CLI-surface + kitty-maximalist source analysis** over `vendor/` — refresh `docs/NEXT-SESSION-CLI-SURFACE.md`: pin the concrete CLI verb set (herdr-shaped `agent.start/list/get/send/focus/read`, `pane.*` events; `pane.send-text` via kitty internals) and the delta-stream wire schema, so every step's "thin CLI" acceptance shape is nailed down.
3. **Implementation from step 1** — repo bootstrap onward, gated on the frozen CLI/socket contract.
