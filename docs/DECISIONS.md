# tally — decisions (canonical ruling ledger)

> Decided 2026-07-07 by Tom; supersedes the notes deliberation (`projects/tally/*`). This repo is authoritative.

## Queue & PIM

| Ref | Decision | Ruling | Supersedes |
|---|---|---|---|
| PS#1 | Canonical work store | TaskChampion (taskwarrior 3.x SQLite) is the sole agent-work store; urgency=priority, UDAs carry the agent vocabulary; no second datastore. | "filesystem-as-queue is the database" |
| PS#1 | No filesystem-drain | Filesystem-as-queue is not a built-in tally mechanism; scheduling a file-output workflow is ordinary usage, not a primitive. | filesystem-drain baked into the daemon |
| PS#1 | OCR is first-class | tally does nothing custom for OCR — it is one agent implementation whose outputs happen to be files; same scheduled job as any other. | "single-class OCR degenerate path" |
| PS#3 | Format lineage | taskwarrior canonical for agent-work; iCalendar/vCard canonical for the human layer; VTODO export-only, lossy one-way, never authoritative. | VTODO-canonical |
| PS#4 | Human PIM | No open-protocol PIM store, no CalDAV/vdirsyncer/khal intake; Google Calendar via gws is a read-only view only, never a presence/scheduling input. | calendar-as-presence; vdirsyncer/khal sovereign PIM |
| PS#4 | Presence gate | The niri flag-file (`~/.cache/monitors-off`) is the sole presence gate. | calendar VEVENT presence; fallback signals |
| PS#8 | No email bridge | No email bridge either direction — intake ruled out and outbound-summons dropped for the first release. | FS§8 outbound-summons-only; sasori email intake |
| PS#8 | sasori not an intake path | sasori is NOT a task-intake mechanism — neither its email path (ruled out) nor its front-end. It stays conceptual lineage only (the "generative negative"); the gh-CLI intake is the sole day-1 antenna. | "bring in the sasori front-end to add tasks to the queue" |
| PS#14 | Coordination substrate | Agent-team coordination is a taskwarrior project:/tag VIEW of the one store + the live delta stream — not a second board. | shared filesystem task-board; delta-stream-alone |
| appendix | Single authoritative store | One TaskChampion store, authoritative on the conductor; remote devices (zenbook-duo, laptops) READ it over the reach layer. No multi-store sync, no offline-edit reconciliation — one store, read remotely, never replicated. | duo-local task-sync / TaskChampion replication |
| appendix | Durable-row admission test | A unit earns a taskwarrior row only if it needs cross-source urgency ranking or must survive a crash (autonomous/batch/queued); a live-orchestrator-spawned unit takes a pls lease + witness line, no row. | one-task-per-spawn |
| appendix | Three-records law | tally tracks CONTENTION and PROOF, never CONTENT or CONTROL: harness JSONL=content, taskwarrior=work-item, witness=proof; they meet only at `session_ref` and the one GPU lease. | law left implied across PS#5/#14/#28 |
| appendix | Four-log read-time join | Audit is a read-time join `task export × journald × git log × harness JSONL`, keyed on `session_ref`; no bespoke audit log. (2026-07-09: timewarrior removed — was five-log.) | five-log join (timew export a member) |
| inventory | JSON-projection contract | The universal substrate is the one-record-per-line JSON projection (`task/journalctl export`, jCal), not any on-disk format; readers build against the projection. | "it's all sqlite" |
| PS#21 | gh-CLI intake (was bugwarrior) | Direct `gh` CLI usage is the GitHub intake antenna (@-mention → task), wired in the module but shipped OFF by default, opt-in per-source. bugwarrior REPLACED 2026-07-09. Query surface = the queued octo.nvim surface-scan dispatchable (feeds build step 8). | bugwarrior as the antenna |
| PS#21 | Other antennae deferred | newsboat (RSS) and notmuch (mail) deferred, not day-1; pull-only, copy-and-detach, one source per antenna. | "newsboat + notmuch in the intake set" |
| PS#21 | No Baikal | Baikal dropped entirely — not the work-store, not retained as a human-layer/VTODO-mirror option. | FS§3 "Baikal could serve the human layer" |

## Session & Persistence

| Ref | Decision | Ruling | Supersedes |
|---|---|---|---|
| PS#12 | Persistence backend | **zmx** (neurosnap/zmx) is the sole backend — libghostty-vt rehydration (KKP-aware), maintained, and compatible with kitty's `ssh` kitten. shpool retired. **zmosh (mmonad) dropped 2026-07-07**: unmaintained + antagonist to the kitty ssh kitten. Network-change resilience via Tailscale + reattach, not a UDP layer. | shpool-default swappable; zmosh (the brief mmonad pick) |
| PS#12 | No windows/tabs/splits | zmx persists one pty/shell per handle and provides no windows/tabs/splits — niri/the WM owns layout; one kitty terminal = one panel. | — |
| PS#13 | View plane (hybrid) | tally owns only the minimal kitty-native binding + delta stream + focus/tunnel-in keyed on `kitty_window_id`; all dashboard/tab-bar rendering is external (CUBS). | FS/PS#13 "tally owns tab-per-agent" |
| PS#13 | tab-per-agent not tally's | The kitty-tab-per-agent projection ("every kitty terminal is a projector surface") belongs to the nix desktop/dotfiles, not tally. | any framing where tally owns the projection |
| PS#7 | Web viewer | GATED/deferred — not day-1; Tom's (tally-adjacent) plans leverage tally's OUTPUT FORMATS (taskchampion/jCal/witness), not a front-end/back-end coupling. A pure delta-stream/format consumer if it ever ships. | FS§7 "ship static HTML day-1" |
| PS#15 | Agent-state detector | In-daemon supervised thread; reads blocked/working/done/idle out-of-band via throttled `kitty @ get-text`; scoped to agent panes only (never reads `tally watch`). | PS#15(b) separate restartable writer |
| PS#6 | Grouping tier | Workspace → Session → Pane collapsible tree now, status dots aggregated up; pay the tree cost up front. | PS#6(a) flat Session→Pane |
| FS§6 | Tunnel/receiver invariant | Every kitty terminal is a zmx receiver by default (conductor included, pi or cc alike) — but this substrate is **provided by the dotfiles** (mecattaf/dotfiles zmx migration + #38), which tally ASSUMES and never re-ships. tally is custom-built for Tom's system. `mod+enter` escape also lives in dotfiles. | "kitty-shpool tunneled"; "generated by the tally module" |
| FS§5 | Data model | `{session,pane} → (persistence_session_id, kitty_window_id, agent{kind,status})`; a dispatched job and a supervised session are one object seen two ways on one delta stream. | — |
| PS#19 | Reach orthogonal | tally is an offline CLI; how keypresses arrive is not its problem. Tailscale is the default reach, Headscale a pocket escape hatch — never named inside tally's own design. | "Tailscale is the sole tally-internal reach" |

## Compute & Pools

| Ref | Decision | Ruling | Supersedes |
|---|---|---|---|
| OUV-CM r1 | Multi-pool abstraction | Every compute source is a pool = {budget, gate/lease, meter}; pls is the pool primitive, one broker per pool. Leverage pls maximally. | "GPU-seconds as a single fixed subsystem" |
| OUV-CM r1 | Two admission predicates | One pool interface (ask→gated→metered), two predicates: co-residency (GPU, pls-native budget math) vs windowed-consumption (subscriptions, "meter says GO"). | — |
| OUV-CM | tally absorbs pls config | Pool definitions/capacities/budgets/units are declared through the pls broker config tally owns — no second config system. | — |
| OUV-CM r1 | Three consumption meters | pls meters three native consumption axes and tally uses ALL three: GPU-seconds (GPU pools, the proof meter), subscription-minutes (rolling window), tokens (subscription/API). No separate per-axis tracker where pls already meters. | external per-axis meters |
| OUV-CM r2 | selfaware → mechanical | No LLM in the pacing loop: poller becomes a `Restart=always` systemd meter daemon; GO/SLOW/STOP is a mechanical admission gate via `cc-usage --check` exit codes (0/1/2/3). | selfaware LLM-narrated pacing |
| OUV-CM r2 | Programmatic budget | The mechanical non-interactive drain spends from the monthly programmatic pool (claude -p / Agent-SDK / API); the meter reads programmatic, never interactive credits. | — |
| PS#2 | No model-tier router | Model class is chosen at ignition by the orchestrating model, carried as a declared task property; tally never escalates or re-picks a model. | "scale model up with task"; presence-aware tier router |
| OUV-CM r3 | Pool-assigner | A strictly lower layer than model choice: round-robin across GO subscription pools, fall back to local-GPU when all windows STOP, API-dollar pool only on an explicit budget UDA; automates the cross-account switch. | selfaware's manual human account switch |
| OUV-CM r4 | Meter unification | Witness gains `pool` + trust-class-tagged `charge={unit,amount,class}`: GPU={gpu_seconds,verifiable}=proof; subscription/API={tokens\|usd,annotation}=observability, never proof. | "generic cost field" flattening trust classes |
| 2026-07-09 | timewarrior removed | timewarrior is OUT of the substrate. The verifiable GPU-seconds meter is the witness span — "the witness ledger already records job start/end natively, so corroboration comes from the witness itself rather than a repurposed human time-tracker. One fewer moving part; no loss of proof strength." Verifiable metering still covers the GPU pool only; still not a universal cost ledger. | OUV-CM r4 "timew scope" (timew as the sole proof-of-labor meter) |
| OUV-CM r5 | Timing | Local-GPU pool (pls lease + witness) is built first as the reference; subscription pools second; API pool third-only-if-needed. No premature framework. | — |
| PS#5 | Box governor = pls | The per-box governor IS pls itself (one broker per box). No in-daemon lock, no new bespoke slot-lock service, no wrapping lock. Every heavy tenant — tally or not (ds4-server, OCR vLLM) — acquires the pls lease DIRECTLY at its declared priority; RAII/process-exit is the single release. The VRAM/MemAvailable check is a pls budget pool (`--cost`=est-VRAM-GB). tally owns the pool config and is the highest-priority client. | PS#5 OPEN; in-daemon-vs-standalone; wrapping slot-lock; MemAvailable as a separate check |
| PS#5 | GPU=2, worker-prioritized | Two GPU pools — controller-GPU and worker-GPU — one pls lease per pool (single-lease-per-pool). The **worker GPU is prioritized** (headless, dedicated to models, not sharing the box with chrome/graphical). DS4 is the sole cross-box job: an atomic co-allocation of a heavy worker-GPU hold + a light controller-GPU spill hold. No fleet-wide lock. The controller NPU serves only fringe utilities (e.g. title-picking) and is **NOT a pool** — out of the compute model entirely. | flat "GPU=1"; GPU≈2 hedge |

## Harness & Orchestration

| Ref | Decision | Ruling | Supersedes |
|---|---|---|---|
| OUV-MH R1 | Three planes, one mechanism | All GPU labor routes through the spawn-tracked-agent-job (execution plane); control-plane orchestration and view-plane rendering are consumers bound by two seams. "All workflows through tally" is true at execution, false at control/view. | "tally is a multiplexer/orchestrator" |
| OUV-MH R2 | Seam A — one enqueue verb | One verb `{priority, source, agent.kind∈{pi,claude-code,shell}, invocation, cwd/worktree, evidence_spec}` with `--wait`/done-event; subsumes `wait_for_subagents` (barrier = enqueue-N-await-N). Bind to the generic seam, not TeamCreate. | per-paradigm surfaces; TeamCreate-specific wiring |
| OUV-MH | Seam B — delta stream | snapshot + newline-JSON events + control carrying `{session,pane→agent{kind,status}}`; adopts herdr's verb vocabulary clean-room. | — |
| OUV-MH R3 | herdr/cmux — interface not engine | Reject both multiplexer engines; adopt clean-room (AGPL-safe) herdr's socket verb vocabulary + TOML region/regex manifests and cmux's cooperative harness-hook detection. Never lift code; read as inspiration fully. | adopting any multiplexer engine; zmosh1 mis-identity |
| OUV-MH R4 | Worktrees — a job field | The external-worktree-orchestrator family collapses to a `cwd`/worktree attribute on the job; tally provides the field, the orchestrator LLM decides policy. Documented in the README. | building a worktree lifecycle engine |
| PS#16 | Trigger surface | events/ drop + systemd timers + live socket-enqueue — three ingress paths, one queue, none privileged. | events/+timers only |
| PS#20 | pi AND cc, one primitive | Both pi and claude-code dispatch through the same job; agent.kind spans {pi, claude-code, shell}; deterministic workflows wrappable in a light pi executor; OCR first-class, never degenerate. | harness-exclusive framing |
| PS#appendix | No sandbox in tally | tally ships NO sandboxing/isolation element. Process/filesystem isolation is not tally's concern; if an agent needs it, it is invoked by a specific SKILL inside the pi/claude-code session that tally wraps — a payload of the wrapped run, never a tally primitive. (Closes the July-7 "make a github issue" item: not deferred — out of scope.) | bubblewrap/microVM sandbox as a tally feature; sandbox deferred-to-issue |
| appendix | Dedup-by-existence | Before a run, stat + grep the witness/artifact; if output exists, skip and tag the skip out of canonical GPU-seconds. A filesystem check, not a database. | — |
| appendix | Four anti-loop invariants | (1) three planes not a cycle; (2) single lease bounds all concurrency (PLS_CAPACITY=1 per box); (3) tally dispatches leaf workers never orchestrators (one hop); (4) detector scopes to agent panes only. | — |
| OUV-MH | niri/kitty substrate | One kitty terminal = a single panel (no splits/tabs/multiplex); niri owns workspaces/layouts; `pane.send-text` uses kitty internals; tally never re-grows a multiplexer/UI. | herdr-style in-terminal tabs/splits |

## Witness & Evidence

| Ref | Decision | Ruling | Supersedes |
|---|---|---|---|
| PS#9 | Witness record altitude | The canonical on-disk record carries the full field set (task_uuid, transition_timestamp, verdict, exit_code, artifact_content_hash, gpu_seconds, wall_clock, attempt, lease_epoch, dedup_key, labor_class, optional trace_ref, pool, charge); the 5-field form is a projection. | "field-count altitude open" |
| OUV-CM r4 | Witness pool + charge | Add `pool` + trust-class-tagged `charge={unit,amount,class}`, reserved day-1 (GPU-only populated) so multi-pool is purely additive. | GPU-seconds-only framing |
| PS#10 | Append atomicity | Plain `O_APPEND` + fsync per line; no checksum prefix, no temp-then-rename. recover() discards an un-parseable trailing line. | (b) checksum-prefix |
| PS#11 | journald amended | Add exactly `TALLY_ATTEMPT`, `TALLY_LEASE_EPOCH`, `TALLY_LABOR_CLASS` to the pinned schema. | FS§4 "pin schema as-is" |
| FS§4 | journald schema | Every event is one structured `journalctl -t tally -o json` entry; observability not load-bearing memory; the witness is emitted from these fields but kept a separate artifact. | — |
| PS#9 | Evidence gate | Terminal commit gates on artifact-exists ∧ content-hash ∧ exit-code-ok ∧ witness-span, never self-report; the witness records every run's span natively, cloud runs included (2026-07-09 — was timew-closed, with wall-clock as the cloud substitute). | timew-closed as the fourth conjunct |
| PS#9 | recover() = re-present | Re-derive the queue and re-present in-flight work (`pi --resume`), never replay; five invariants; a single monotonic lease-epoch is the only fence (no election/consensus). | — |
| PS#21 | lease-epoch source | `lease_epoch` = the pls lease generation, backstopped by a persisted counter file so it stays monotone across an unclean reboot; the daemon is the counter's sole increment owner (2026-07-12, issue #9 — was systemd-incremented: the ExecStartPre bump double-incremented alongside the daemon's own boot bump and was removed). | — |
| PS#9 | Ledger-as-truth | The append-only witness JSONL is canonical, permanent, git-independent proof; SQLite/TaskChampion is a derived rebuildable cache, never a second source of truth. | — |
| PS#18 | Evidence richness | Keep the git-independent floor canonical; add optional `trace_ref` (pi-RPC) + optional agent-trace projection (git-touching threads); absent on opaque runs. | — |
| PS#21 | Gate-fail forensics | A clean exit with no gate-passing artifact writes `verdict=clean-exit-no-artifact` (excluded from canonical GPU-seconds) + a `TALLY_EVENT=evidence_fail` journald entry. | — |
| PS#21 | Retention boundary | Permanent: gpu_seconds+wall_clock summary, dedup_key, verdict. TTL-prunable: detector logs (2026-07-09: no timew intervals exist to prune). | raw timew intervals in the prunable set |
| appendix | Witness broader than taskwarrior | Every heavy GPU-touching unit emits a witness line, including live-orchestrator-spawned units with no taskwarrior row; witness→content joins via `session_ref` on taskwarrior/journald, not in the proof line. | — |

## Nix & Packaging

| Ref | Decision | Ruling | Supersedes |
|---|---|---|---|
| FS§7 | Repo identity | Self-contained flake at `github.com/mecattaf/tally`; dotfiles consume it as `inputs.tally.url = "github:mecattaf/tally"` with nixpkgs.follows, importing the module per-role. | — |
| PS#17 | Flake outputs | Three: `packages.tally` = one binary (daemon + CLI) — **Bun-compiled TypeScript per the 2026-07-09 stack ruling below** (was "one Rust binary"); `homeManagerModules.tally` = primary; `nixosModules.tally` = unbuilt thin wrapper stub. Built on flake-parts. | nixosModule-as-primary; package-only; "one Rust binary" |
| FS§5 | Module shape | microvm.nix-SHAPE generator: typed options in → systemd user units + kitty/persistence config + CLI on PATH. Not a bare dotfiles config; microvm imitated as structure only, never a dependency. | "simple user config" |
| PS#17 | Minimal option surface | `enable` / `role (conductor\|receiver)` / `conductorHost` / `sessions`. No `persistence.backend` / `escapeHatch` — zmx + `mod+enter` are dotfiles-owned (assumed). Add an option only when a concrete role need pulls on it. | maximal option surface; `persistence.backend`/`escapeHatch` as tally options |
| appendix§5 | Ambient default shipped BY the tally module | every heavy GPU-touching invocation → pls-lease-wrapped, so a CC-spawned pi subagent is tally-compatible without CC knowing tally exists. The companion "every terminal → zmx receiver" default is **dotfiles-owned** (tally assumes it, see FS§6), not tally's. | "both defaults shipped by the flake" |
| PS#13 | tally/dotfiles boundary | The module does NOT generate the kitty-tab-per-agent projection, niri layout, or the `mod+enter` escape — those belong to the nix desktop/dotfiles. | tally owning the view/tab projection |
| PS#17 | Inputs + dev rig | Pin only what tally leans on (pi, llama-swap — pls promoted to the named list 2026-07-09, see Q3 below; taskwarrior and the gh CLI each on its own named trigger, never bundled). Dev rig = process-compose via `nix run .#dev`; prod = systemd user units. | — |
| FS§9 | Build sequence | Ordered build_steps authored; router step DROPPED (PS#2); OCR is a first-class job on the one primitive; multi-pool substrate lands AFTER the single-GPU meter+witness. | FS§9 step 8 "Router layer last" |

## Resolved 2026-07-07 (afternoon — with mecattaf/dotfiles #38 context)

Both prior OPEN items are now closed — see the two **PS#5** rows in *Compute & Pools*:
- **Box governor = pls itself** (direct acquire; no in-daemon lock, no new bespoke slot-lock, no wrapper).
- **GPU=2, worker-prioritized**; DS4 is the one cross-box co-allocation (worker-heavy + controller-light spill); the controller NPU is out of the compute model (fringe utilities only).

Plus the boundary correction (FS§6 / appendix§5): tally is **custom-built for Tom's system** — it assumes the dotfiles-provided zmx substrate (receiver-everywhere, session create/name, reattach, the fzf picker) and never duplicates it; the dotfiles picker (#38) is a delta-stream **consumer** of tally's agent-state detector. No decisions remain open.

> **2026-07-07 (later) — persistence backend zmosh → zmx.** The brief mmonad/zmosh pick was dropped: unmaintained and antagonist to the kitty `ssh` kitten. Backend is now **zmx** (neurosnap/zmx). Same boundary — the substrate is dotfiles-owned (migration live), tally assumes it. Roaming resilience is delegated to Tailscale + reattach (zmx has no UDP layer), which is a fine trade for kitten-ssh compatibility.

## Resolved 2026-07-09 — companion-tool dispositions

Two companion-tool episodes from the jul9 shape session are closed here:

**agentctl: SKIPPED (2026-07-09).** "The standalone hook-utility companion is dropped as not valuable enough." (Paper record: notes `july26-fable-second/july9-morning-handoff.md` Decision 3; verbatim reasoning in `july9-fable-cubs-ramifications.md` Addendum 2.) The late-jul9 verdict that had shaped it — agentctl as a standalone COMPANION Linux utility governing TOOL CALLS inside a session (observe → trace → optionally gate), orthogonal to tally's JOB plane — is superseded by this skip: the companion is not built. Consequences:

- **§5.2 hook-installer closure reverts to the deep-pass recommendation**: the tally module ships its own cooperative-hook installer. agentctl is no longer the payload of that ruling; the installer closure is recorded as its own entry in this ledger.
- **The Q2 relaxation stays RECORDED as doctrine but is DORMANT**: approval/escalate gating is legal as an explicit opt-in operator flag (analogous to Claude Code's permission modes); the DEFAULT remains no-approval (act + witness + revert). No component currently implements it.
- **The witness hash-chaining steal SURVIVES**: per-line `seq`/`prev_hash`/`hash` SHA-256 chaining (lifted from the chocks/agentctl trace-ledger eval) targets tally's witness JSONL directly and never depended on agentctl existing. Recorded as its own entry in this ledger.

**agent-trace: DROPPED (2026-07-09).** "Code-generation-specific; git commit-granularity (witness × git log × session_ref) already covers attribution for a solo operator." (Paper record: notes `july26-fable-second/july9-morning-handoff.md` Decision 4; verbatim reasoning in `july9-fable-cubs-ramifications.md` Addendum 3.) cursor/agent-trace attributes files → conversations → line-ranges by construction; its marginal value is line-level mixed-authorship attribution inside files — an IDE/team problem, not a problem for a solo operator whose revert unit is the commit. Tom's agents commit their own code via git with conventional commits, and commit-granularity attribution already exists natively: witness (job + artifact hash) × git log (commit) × `session_ref` (conversation). The wholesale model (agent-trace as the witness) also fails PS#9 on the merits: no exit-code field, no span, and its `vcs.revision` stamps the pre-job HEAD, not the commit containing the traced code.

KEPT from the episode: the **models.dev `provider/model-name` convention** for the witness model field (zero-cost; recorded as its own entry in this ledger). Also unaffected: the witness hash-chaining adoption (different source — chocks/agentctl). The spec is filed as a **watched reference** (REFERENCES.md §B). This narrows PS#18: the "optional agent-trace projection (git-touching threads)" is dropped; the optional `trace_ref` (pi-RPC) survives.

## Resolved 2026-07-09 (stack — the Bun flip)

| Ref | Decision | Ruling | Supersedes |
|---|---|---|---|
| jul9 | **Bun is IN** | Daemon + CLI in **TypeScript on Bun** — Tom's explicit ruling, closing the deliberation that the **C2-moot re-weight** ("any argument that rust is better than bun for nix is moot, reduce consideration there"), the **FFI-seam verdict** (in-process TaskChampion unreachable from Bun at sane cost, so C1 stops discriminating), and the **Backlog.md existence proof** (~84k-line solo-operated Bun CLI+web tool Tom runs daily; maintained first-party bun2nix flake) had already tipped. `packages.tally` = ONE Bun-compiled binary (`bun build --compile` + `autoPatchelfHook`, bun2nix). The frozen wire contract (CLI-SURFACE §2) is language-neutral and unchanged. | PS#17 "one Rust binary"; SHAPE-RECOMMENDATION DECISION 1 (Rust, MEDIUM confidence) |
| jul9 | TaskChampion access | `task export`/`task import` **shell-out, never in-process linkage**; exactly one new assumption — a runtime dependency on the `task` binary + its JSON export format, version-pinned. 30-80ms per call, invisible at tally's reactive access pattern. | in-process `taskchampion::Replica` linkage |
| jul9 | journald emission | Structured TALLY_* via **`StandardOutput=journal`** stdout capture in the systemd unit (Bun lacks the AF_UNIX SOCK_DGRAM journal socket). Flip-back pull toward Rust only if native structured emission ever proves load-bearing — a two-line workaround, not a blocker. | `tracing-journald` |
| jul9 | **FFI hybrid ruled OUT** | **Never Rust-inside-Bun-via-FFI**: no `taskchampion` cdylib, no napi-rs `.node` binding. Grounds: ~300-500 lines of owned, unsupported glue (`taskchampion-lib` deprecated; no published binding anywhere); `bun:ffi` experimentally flagged by Bun's own docs; a live `bun build --compile` embedded-`.so` regression (#30717) on the exact distribution path; no latency payoff (marshalling-dominated 1-5ms vs an invisible 30-80ms shell-out at five calls/minute); zero solo-maintainer production precedent. If a future flip ever demands in-process `Replica`, the answer is **all-Rust** — not a hybrid. | "natural Rust↔Bun seam" hypothesis |

Recorded flip-back conditions (none hold today): in-process TaskChampion becoming genuinely hot (→ all-Rust); journald AF_UNIX SOCK_DGRAM proving load-bearing; Claude-assisted TypeScript proving less reliable than assumed.

## Resolved 2026-07-09 (evening) — substrate trims

Tom's rulings of record on the four-component substrate, responding to the confidence ranking
(amended rows above carry the mechanics):

- **taskwarrior/TaskChampion KEPT** — the thin durable veneer, as recommended. Discipline of
  record: *"TW remains a thin durable veneer — one row per durable job, one standing row per
  drain, and no high-frequency machine state (heartbeats, leases, evidence) ever leaking into TW
  rows."* Existing PS#1 / durable-row-admission / ledger-as-truth wording already encodes this;
  no substrate change.
- **timewarrior REMOVED.** Proof triple: `artifact-hash ∧ exit-code ∧ timew-closed` →
  `artifact-hash ∧ exit-code ∧ witness-span` — *"the witness ledger already records job start/end
  natively, so corroboration comes from the witness itself rather than a repurposed human
  time-tracker. One fewer moving part; no loss of proof strength."* Consequence: the standup
  five-log join becomes a **FOUR-log join** (`task export × journald × git log × harness JSONL`).
- **bugwarrior REPLACED by direct `gh` CLI usage** as the GitHub intake antenna (pull-only;
  OFF-by-default, opt-in per-source unchanged). The intake's query surface — which gh
  queries/verbs it polls, which @-mentions/labels are signals — is defined by the queued
  **octo.nvim surface scan** dispatchable (inventory octo.nvim's gh/GraphQL surface → the intake
  job's query set, mirroring the operator's actual daily GitHub workflow). Feeds build step 8.

## Resolved 2026-07-09 — dotfiles boundary contract

Tom's rulings on the boundary-investigation queue (notes
`july26-fable-second/tally-boundary/BOUNDARY-INVESTIGATION.md`; citations verified against
`02cee6f`). All nine closed. The principle, stated once: tally is **maximally reliant on** the
dotfiles substrate (kitty as sensor/actuator, zmx as the sole session-lifecycle owner, ambient
`gh`, journald, systemd linger) yet **fully externalized from it** (installed like any CLI via the
flake-input + homeManagerModule pattern; session create/name/reattach remain forbidden to tally).

| Ref | Decision | Ruling | Supersedes |
|---|---|---|---|
| Q1 | Packaging channel | **RATIFIED** — flake input + `homeManagerModules.tally` (the zmx/Backlog.md consumption pattern), per **PS#17** / FS§7 above; the module is load-bearing (units, pls config), which a bare pkg can't deliver. No bespoke dotfiles `pkgs/tally.nix`. | Backlog-pattern release-binary derivation dotfiles-side |
| Q2 | taskwarrior | tally-pinned flake input **ONLY**; it never enters the dotfiles (no dotfiles taskwarrior exists today — keep it that way). | adding taskwarrior to the dotfiles |
| Q3 | pls | Same posture — and pls is **promoted to tally's named flake-input list** (was missing; only pi + llama-swap were named despite tally owning pls's config and broker units). Nothing lands in dotfiles pkgs/. | pls unnamed in the input list |
| Q4 | kitty watcher line | The dotfiles-owned kitty.conf **carries the `watcher` registration line**; the tally module **exports the watcher script's store path as a read-only option** so the line never rots. kitty.conf stays dotfiles-owned; the payload stays tally-versioned. | tally writing into kitty.conf |
| Q5 | Hook installer | **RATIFICATION** — already closed 2026-07-09 (CLI-SURFACE §5 flag 2; the companion-tool entry above): the tally module ships its own cooperative-hook installer. Cross-reference only; nothing new ruled here. | — |
| Q6 | `claude -p` contingency | **KEPT, non-default**, and documented as the **SOLE boundary exception** — the one place tally *starts* something rather than observes (CLI-SURFACE §3.1 sidenote). | excising the contingency |
| Q7 | NPU thread-titling | Dotfiles-side service, **PERMANENTLY out of tally scope** — dotfiles **#38** is the canonical write-down (qwen3-0.6b via fastflowlm, session titling from `zmx history`), **#40** the enabling infra (nix-strix-halo). tally's existing titling scope-outs gain the #38/#40 cross-reference. | tally-internal titling micro-utility |
| Q8 | gh auth | tally consumes the machine's **already-authenticated `gh` CLI, ambient**; the dotfiles/environment own credential provisioning; **tally never manages credentials**. (Tom: the installed computer's authenticated gh is "the simplest approach anyway." Was addressed nowhere.) | gh auth unowned |
| Q9 | Hostname (**INVERTED**) | Recommendation was land dotfiles **#39** (`harness-desktop` → `coordinator`) first; **Tom ruled the opposite**: #39 is **NOT a blocker** for "the tally repo that is a nix utility" and tally takes **NO sequencing dependency** on the dotfiles. `conductorHost` stays pure configuration with no hostname frozen anywhere in the tally docs; #39 proceeds independently on the dotfiles side. | land-#39-first sequencing |
