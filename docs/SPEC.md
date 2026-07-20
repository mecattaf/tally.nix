# tally — canonical product spec

This is the authoritative product specification for tally. It supersedes the deliberative
`projects/tally/PRODUCT-SCOPE.md` in the notes repo. Decisions here are stated as fact; the
notes hold the reasoning lineage. Where a ruling cites a source it is tagged (`PS#n`, `FS§n`,
`OUV-CM rN`, `OUV-MH Rn`, `appendix`).

---

## Outcome

tally holds six properties at once. Everything below exists to keep all six true simultaneously:

1. **One box, one GPU, serialized.** A single per-box lease bounds all heavy concurrency; a runaway
   50-child spawn becomes a 50-deep priority queue, never a melted box. The lease itself is
   **non-preemptible** (never force-evicted mid-hold — RAII/process-exit is the only release);
   higher-priority work is served by **preemption-as-policy**, a cooperative yield-at-checkpoint one
   layer above the lease (see *The inner fold* under Compute pools).
2. **Proof, not self-report.** Work is committed only against artifact-exists ∧ content-hash ∧
   exit-code ∧ witness-span — never an agent's claim.
3. **Sessions survive everything.** Every interactive terminal is a persistent, roaming,
   un-closable-by-accident receiver; sleep/wake and network switches do not lose state.
4. **One queue, any labor.** OCR, "rebuild chromium locally", `pi`, `cc` are the same kind of
   scheduled job — no special cases, no degenerate paths.
5. **Harness-agnostic.** tally interprets no workflow DAG and hardcodes no team paradigm; every
   orchestrator is a client of one enqueue verb.
6. **No duplication.** Three records at three altitudes, meeting only at `session_ref` and the
   one GPU lease — tally never copies a byte of content it does not own.

---

## The one law

**tally tracks CONTENTION and PROOF — never CONTENT or CONTROL.** Every non-duplication result
falls out of holding this single line. (appendix / OUV-MH)

### Three records, zero overlap

| record | altitude | holds | tally's relation |
|---|---|---|---|
| harness JSONL (CC logs, `pi --resume`) | **content** | conversation, reasoning, tool calls | stores only `session_ref`, reads *through*, never copies a byte |
| taskwarrior | **work-item** | `{uuid, urgency, UDAs, status, pool, session_ref}` | points at the JSONL, never re-derives it |
| witness ledger | **proof** | `{artifact-hash, exit-code, gpu-seconds}` | the sole proof-of-labor |

The three meet only at `session_ref` and at the one GPU lease.

### taskwarrior-row-only-for-durable-autonomous

A unit earns a **durable taskwarrior row only if** it needs cross-source urgency ranking or must
survive a crash to be re-dispatched — i.e. autonomous / batch / queued work (the OCR firehose,
gh-issue intake, scheduled jobs). A unit **spawned live by a running orchestrator** takes a pls
lease + emits a witness line but gets **no taskwarrior row**: its queue position is the live lease
queue, its record is the JSONL + the witness. When a harness runs a wave with its own concurrency,
tally creates **no** task per wave-step — that orchestration is control plane, opaque to tally.
(PS#14 / appendix §2)

Ruled 2026-07-09: taskwarrior/TaskChampion is **KEPT**, under the veneer discipline — **"TW remains
a thin durable veneer — one row per durable job, one standing row per drain, and no high-frequency
machine state (heartbeats, leases, evidence) ever leaking into TW rows."** Cross-store features
(staleness, standup) pay the join tax; that is the known price.

### Dedup-by-existence

Before running a job tally does `stat` + `grep` against the on-disk artifact and the success
witness for the `dedup_key`; if the output already exists the GPU run is **skipped**, tagged
`labor_class=reused`, and excluded from canonical GPU-seconds. The artifact is **re-hashed only on an
mtime/size mismatch** — a matching stat trusts the witness line rather than paying for a blind
re-hash. A filesystem check, not a database — "don't OCR a paper that's already done." (appendix §3)

### Four anti-loop invariants (a DAG, not a cycle)

1. **Three planes, not a cycle.** Control *enqueues to* execution; view *observes* control +
   execution. Nobody runs anybody — peers bound by two seams.
2. **The single lease bounds all concurrency.** `PLS_CAPACITY=1` **per box** means one holder at a
   time; the rest queue by priority.
3. **tally dispatches leaf *workers*, never orchestrators.** A dispatched unit is a leaf (pi
   inference, OCR, shell). A unit that wants to fan out is *acting as* a control-plane client,
   started by a human or a timer. Recursion is bounded to orchestrator→leaves, one hop.
4. **The detector scopes to *agent* panes only.** A pane running `tally watch` is a viewer, not in
   the manifest set, so the detector never reads tally's own output and self-loops.

---

## What tally holds

Reconciled subsystem inventory. Anything not in this table is explicitly not tally's job (see
*What tally is NOT*).

| subsystem | resolution |
|---|---|
| Canonical work store | TaskChampion (taskwarrior 3.x, embedded SQLite) — sole; urgency = `priority`; UDAs carry the agent vocabulary |
| Proof-of-labor | append-only witness JSONL; GPU-seconds derived from the witness span — job start/end recorded natively in the ledger (verifiable class only) |
| Observability | structured `journald` (`journalctl -t tally -o json`) — not load-bearing memory |
| Execution unit | the spawn-tracked-agent-job (pi / claude-code / shell), gated by one pls lease |
| Compute gate | pls, per pool; one broker instance per pool; pls also owns pool config |
| Persistence | **assumed** — the dotfiles zmx substrate (neurosnap/zmx); tally keys a `persistence_session_id` and runs inside those sessions, it does not ship the receiver config |
| Agent-state detector | in-daemon supervised thread; `kitty @ get-text` throttled scrape, agent panes only |
| View plane | minimal kitty-native binding + delta stream only; all rendering external (CUBS) |
| Trigger surface | `events/` drop dir + systemd timers + live socket-enqueue → one queue |
| Human PIM | Google Calendar via `gws`, read-only VIEW only; no CalDAV; niri flag-file is sole presence |
| Intake | direct `gh` CLI intake (GitHub @-mention → task; bugwarrior replaced 2026-07-09), shipped OFF by default; query surface per the octo.nvim surface scan; RSS/mail deferred; no email bridge |
| Packaging | `homeManagerModules.tally` (primary) + `packages.tally` (one Bun-compiled binary — TypeScript daemon + CLI, ruled 2026-07-09); `nixosModules.tally` stub |

---

## Three planes & two seams

A "workflow" spans three planes and tally routes them differently. The rule is **one plane deep**:
all GPU-consuming *labor* routes through tally's execution/metering primitive; the orchestration
*logic* and the *rendering* are consumers bound to it by two narrow seams. "All workflows through
tally" is TRUE at the execution plane, FALSE at control and view. (OUV-MH R1)

```
  CONTROL PLANE  (opaque — CONSUMERS, never absorbed)
  CC dynamic workflow (agent/parallel/pipeline/phase) · pi crew/fork/chain · TeamCreate · a human
        │  seam A: enqueue verb (priority + --wait / done-event)
        ▼
  EXECUTION PLANE  (tally — the ONE mechanism ALL labor routes through)
  spawn-tracked-agent-job = subagent = teammate = Task = crew-spawn = fork = filler job
  per-box heavy slot + governor · pls lease · preemption-as-policy · GPU-seconds meter ·
  evidence gate · append-only witness ledger · agent-state detector (in-daemon)
        │  seam B: delta stream (snapshot + events + control)
        ▼
  VIEW PLANE  (transparent — CONSUMERS)
  CUBS chromium tab-bar · notifier · web projection · nix-desktop kitty-tab projection
```

### The spawn-tracked-agent-job

Every unit of agent labor that consumes a GPU reduces to one spawn-tracked-agent-job. Nothing
touches the heavy slot without going through here — a bypass re-creates unwitnessed labor and OOM.
The dozen-plus agent-team paradigms all reduce to this one unit: thin-subagent-tool and CC
`agent()`/Task are enqueue clients; non-blocking crew is an enqueue client + delta subscriber;
session-branch fork is an enqueue client (its dense report a courtesy `trace_ref`);
external-worktree-orchestrators collapse to a `cwd`/worktree field. No paradigm requires tally to
grow an orchestration engine.

### Seam A — the enqueue verb

tally exposes **one** enqueue control verb:

```
enqueue { priority, source, agent.kind ∈ {pi, claude-code, shell},
          invocation, cwd/worktree, evidence_spec }  [--wait | done-event]
```

It is the sole binding point for `agent()`, `parallel()`'s callees, `crew_spawn`, `fork`, and
TeamCreate. It **subsumes `wait_for_subagents`**: a parallel barrier becomes *enqueue N, await N
done-deltas*, so tally owns the wait and no harness hand-rolls a socket. The `cwd/worktree` field
absorbs the entire external-worktree-orchestrator paradigm. **Model choice is NOT made here** — it
is a declared property carried from ignition. Bind to this generic seam, not to any harness's team
mechanism: Claude Code's TeamCreate is one client among many, and Anthropic now pushes dynamic
workflows / "ultracode" — the harness model may shift again. (OUV-MH R2)

### Seam B — the delta stream

snapshot + newline-JSON events + control, carrying the unified `{session, pane → agent{kind,
status}}` model. `agent.status_changed`, `done`, `blocked` feed CUBS, the notifier, the crew
steering-message, and the `--wait` barrier off *one* stream. The verb grammar adopts herdr's
vocabulary clean-room (`agent.start/list/get/send/focus/read`, `pane.agent_detected`,
`pane.agent_status_changed`). Never lift herdr code; do read it as inspiration fully. (OUV-MH R3)

### Trigger surface

Three ingress paths, one queue: an `events/` drop directory, systemd timers, and live
socket-enqueue. The orchestrator LLM is a high-priority socket-enqueue client; timers + `events/`
cover cron/routine/one-shot autonomous work. No path is privileged. (PS#16b)

---

## Queue & PIM

### Canonical work store

TaskChampion (taskwarrior 3.x, embedded SQLite) is the **sole canonical agent-work store**. The
urgency engine is `priority`; UDAs carry the agent-fleet vocabulary (`agent`, `labor_class`,
`pool`, `session_ref`, `model_class` (the model chosen at ignition — not a tier-router hint),
`cwd`/`worktree`, `trust` — see *Witness & evidence*). tally
never hand-rolls a schema and stands up no second datastore. Only a relational store can rank a gh
@-mention over the OCR firehose and hold a cost ledger; a directory ranks nothing. (PS#1a)

**TaskChampion access is `task export` / `task import` shell-out — never in-process linkage**
(ruled 2026-07-09, with the Bun flip). The daemon is TypeScript on Bun; the `taskchampion` crate
exposes no supported non-Rust surface (`taskchampion-lib` is deprecated with no replacement), and
the Rust-inside-Bun FFI hybrid is explicitly ruled OUT (DECISIONS.md, 2026-07-09). This adds
exactly one assumption: a runtime dependency on the `task` binary and its JSON export format,
version-pinned. At tally's access pattern (reactive dispatch + standup query — never a hot loop)
the 30-80ms subprocess round-trip is invisible against the daemon's I/O-bound hot path.

**Filesystem-as-queue is not a built-in tally mechanism.** There is no filesystem-drain baked into
the daemon.

### OCR is first-class, never a degenerate path

tally does **nothing custom** for OCR. The OCR drain is one agent implementation (deterministic,
vLLM/llama-swap) whose outputs happen to be files; TaskChampion's auto-generated logs record which
papers were processed and when (enabling batched manual review + R2 purge). Any workflow — OCR,
"rebuild chromium locally", `cc`, `pi` — is the **same kind of scheduled tally job**. If tally
cannot make OCR as easy to schedule as any agent job, the mechanism is wrong. (PS#1 / PS#20)

### Format lineage

Resolved **by layer**: taskwarrior/TaskChampion canonical for the agent-work layer;
iCalendar/vCard canonical for the human layer; **VTODO is export-only** — a lossy one-way mirror,
never authoritative. `urgency` (computed) and the UDA agent-vocabulary have no home in iCalendar.
**No Baikal** — dropped entirely, not even as a human-layer / VTODO-mirror future option. (PS#3 /
PS#21)

### The JSON-projection contract

The universal substrate is not any on-disk format but the **JSON-projection contract**: one record
per line, UTF-8, published grammar, lossless JSON projection (`task export`,
`journalctl -o json`, jCal). Every downstream reader, skill, and consumer builds against the
projection, not the on-disk form. Tom's ambitious (tally-adjacent) web-viewer plans leverage these
OUTPUT FORMATS, not a front-end/back-end coupling (which is why the web viewer is gated/deferred,
PS#7).

### The four-log read-time join

Audit and status are a **read-time join**, never a bespoke audit log (four logs since 2026-07-09 —
timewarrior removed; was five):

`task export × journalctl -t tally -o json × git log × the harness JSONL`

keyed on `session_ref` (`TALLY_SESSION_REF` / `TALLY_TASK_UUID`). git is scoped to public
proof-of-work only. taskwarrior *points at* the harness JSONL through `session_ref`; it never
copies it.

### Coordination substrate for agent teams

Agent-team coordination state is a taskwarrior `project:`/tag **VIEW** (a projection) of the ONE
canonical work-store — **not** a bespoke second task-board directory (which would be the rejected
second source of truth). The live delta stream carries events; the crash-surviving coordination
state is the same store reconcile-from-disk re-derives. The grouping tier is **Workspace → Session
→ Pane** (collapsible tree, status dots aggregated up) — the tree shape is paid for up front
because retrofitting it later was flagged the one painful change. (PS#14 / PS#6b)

### Human layer & intake

- **Google Calendar** may be read via `gws` as a **human-layer convenience VIEW only** — never a
  presence input, never a scheduling input. tally holds no open-protocol PIM store and stands up no
  CalDAV/vdirsyncer/khal intake. The niri flag-file (`~/.cache/monitors-off`) is the **sole**
  presence gate. (PS#4)
- **GitHub intake is direct `gh` CLI usage** (@-mention → task; bugwarrior REPLACED 2026-07-09),
  wired in the module but **shipped OFF by default**, opt-in per-source — the one narrow, pull-only
  intake antenna, mirroring the Claude @-tag. Its query surface (which gh queries/verbs, which
  @-mentions/labels count as signals) is defined by the queued **octo.nvim surface scan**
  dispatchable, so the intake mirrors the operator's actual daily GitHub workflow — feeds build
  step 8. (PS#21, amended 2026-07-09) **Auth is ambient (ruled 2026-07-09):** tally consumes the
  machine's already-authenticated `gh` CLI; the dotfiles/environment own credential provisioning —
  tally never manages credentials or tokens.
- **No email bridge** in either direction; **newsboat (RSS) / notmuch (mail)** deferred, not day-1.
  Intake posture is pull-only, copy-and-detach, one source per antenna. (PS#8)
- **sasori is not an intake path** — neither its email path (ruled out) *nor* its front-end. sasori
  stays **conceptual lineage only** (the "generative negative" that named the queue/drain gap), not a
  live task-intake mechanism; the gh-CLI intake is the sole day-1 antenna. (PS#8)

---

## Session & persistence

### Persistence backend — zmx

The persistence backend is **zmx** (neurosnap/zmx), rehydrating through **libghostty-vt** (KKP-aware
across reattach). It is actively maintained and — decisively — **compatible with kitty's `ssh`
kitten**, the terminfo/remote-control path tally and the dotfiles rely on. zmx persists exactly one
pty/shell session per handle and **provides no windows, tabs, or splits — that is the window
manager's job.** The data model stays backend-agnostic (stores only a `persistence_session_id`), but
zmx is the day-1 and **sole named backend**.

Resilience across network changes (Wi-Fi↔cellular, VPN toggles, sleep/wake) is delegated to
**Tailscale + reattach**, not a bespoke UDP transport — a deliberate trade for kitten-ssh
compatibility. shpool is retired. The earlier **mmonad/zmosh** pick (an encrypted-UDP-roaming zmx
fork) was **dropped 2026-07-07**: unmaintained and antagonist to the kitty ssh kitten. (PS#12)

### The kitty-tunnel invariant (provided by the dotfiles, assumed by tally)

**Every session is kitty-zmx tunneled, whether the agent runs via `pi` or `cc`** — every kitty
terminal is a zmx receiver by default on both device roles, including the conductor itself (Tom
has lost sessions to accidental on-device closes; uniform-receiver removes that failure mode). This
invariant is **already provided by the dotfiles zmx substrate** — the zmx migration in
`mecattaf/dotfiles` (`new-terminal`, `remote.fish`'s `desk`, the resume/project pickers, and the
"every terminal is a projector surface" rule; see dotfiles #38). tally is **custom-built for that
system**: it **assumes** the ambient zmx environment and never re-ships or duplicates it. tally's
daemon runs *inside* those persistent zmx sessions and reads them; it does not create, name, or
reattach them.

### Conductor-receiver, cross-device

All interactive kitty terminals run on the **conductor** (the controller Framework Desktop, which
also runs tally, the always-on quadlets, kitty and chrome) and are accessed through the persistence
layer. The headless **worker** node runs models and holds no interactive terminals — it is the
headless batch path (systemd oneshot, no terminal, no zmx session). Remote devices (the
zenbook-duo, laptops) reach the conductor by reattaching over kitten-ssh/Tailscale. tally is built **orthogonal to the
reach layer** — how keypresses arrive is not tally's problem (see *Compute pools → Reach*).

The **TaskChampion store is single and authoritative on the conductor.** Remote devices **read** it
over the reach layer; there is **no multi-store sync and no offline-edit reconciliation** — one
store, read remotely, never replicated. (The duo is a projector/reader, not a second writer.)

### Agent-state detector — in-daemon

The agent-state detector is the one genuinely-new piece of kerdr code and lives **inside the single
tally daemon** as a supervised thread (restart-isolation, not a split binary). It classifies
blocked / working / done / idle **out-of-band** via `kitty @ get-text` on a throttle, emitting a
delta on the same stream autonomous jobs feed — no harness cooperation required. Clean-room
references (never lifting code): herdr's per-harness TOML region+regex state manifests
(`claude.toml`, `pi.toml`, `codex.toml`) as the scrape-rule format; cmux's cooperative harness-hook
state as the *authoritative* second strategy where a harness offers a hook (CC, codex), scrape as
universal fallback. The `osc_title`/`osc_progress` regions bind to `kitty @ ls`
(`foreground_processes[].title`) + OSC progress escapes — **not** `get-text`; OSC-emitting agents
(Claude Code's braille spinner) MAY serve as a zero-latency scrape fast path checked before the
grid read (deep-pass A1, 2026-07-09). **The detector scopes to agent panes only** — a `tally watch` pane is a viewer,
not in the manifest set, so the detector never self-reads. (PS#15a)

### The view plane

tally owns only the **minimal kitty-native binding**: the session data model
`{session, pane} → (persistence_session_id, kitty_window_id, agent{kind, status})`, the delta
stream, and focus/tunnel-in via kitty internals keyed on `kitty_window_id`. It ships **no**
dashboard and **no** tab-bar; all rich rendering (CUBS chromium tab-bar, notifier, web projection)
is a pure delta-stream consumer. A dispatched autonomous job and a supervised interactive session
are the same object seen two ways, feeding one delta stream. (PS#13c)

**Front-end doctrine (ruled 2026-07-09).** Pure rendering is the **initial-release posture, not a
vow** — the load-bearing property is **"no mutation capability."** The eventual rich wall is a
**static TypeScript browser-tab bundle** fed through a `tally bridge` subcommand that proxies Seam
B to a localhost WebSocket behind a hard read-only method allowlist (`{session.snapshot,
session.subscribe, session.ack, session.unsubscribe}`) — non-interactivity is machine-checked, not
disciplinary. Visual register: the Anthropic dark-ground register, composed from the platform-kit
five-component read-only subset. Interactivity MAY arrive later, **verb-by-verb, via
bridge-allowlist additions — each one a ruling**, reviewed against the bright-line test: legal iff
it holds no path that mutates daemon or task state. Sequencing: a given build pass builds the
**CLI only**; the non-interactive front-end substrate comes at the END (shape, not schedule).

### Recovering a tally-owned agent session

A concern the boundary must answer: **can the operator recover a tally-owned `pi`/`claude-code`
session?** Yes — and by construction, not by a bespoke tally path. A tally-dispatched **interactive**
agent runs *inside* a zmx session (its `persistence_session_id`) — either bound at enqueue via
`--session` or started in a session the dotfiles flow (`desk`/`new-terminal`) created. Because tally
owns only the queue/meter/witness/detector and **never** the session lifecycle, a tally-owned agent
session is recovered by the **exact same dotfiles path as any other session**: the `desk-resume` fzf
picker over `zmx list --short`, then `kitten ssh … zmx attach`. The agent process kept running
server-side (linger); the operator reattaches with full state. tally's *only* addition is the
**agent-state annotation** the picker reads off the delta stream (dotfiles #38 Q4) — the same session
list, now showing 🔴 blocked / 🟡 working / 🔵 done beside the live agent. "Can I recover a
tally-owned session?" is answered by the boundary itself: **tally cannot break zmx recovery because
it never touches it, and it makes recovery *better* by telling the picker which sessions hold a live
agent.** (A **headless batch** job — the OCR drain, or a `claude -p` drain — has no terminal to
reattach; it is re-presented by `recover()` / `--resume`, not recovered as an interactive session.)

### The tally / dotfiles boundary (authoritative)

tally is **custom-built for Tom's system** and composes into the same home-manager config the
dotfiles already own. The line is drawn to **avoid duplicating any dotfiles work**, not to make
tally a portable product.

**tally (`homeManagerModules.tally`) SHIPS:**
- the **daemon** — queue/dispatch (the spawn-tracked-agent-job), preemption, evidence gate,
  witness ledger, and the **in-daemon agent-state detector**;
- the **pls pool configuration** (the GPU pools + their budgets/priorities) and the ambient
  **pls-lease-wrap** default — every heavy, GPU-touching invocation is lease-wrapped, so any
  harness-spawned `pi` is tally-compatible without the harness knowing tally exists;
- the **delta stream** (the socket + the `{session,pane→agent{kind,status}}` model) and the CLI;
- its own **systemd user units** (drain timer/oneshot, the pls broker config, metering);
- the **cooperative-hook installer** (closed 2026-07-09): home-manager
  `programs.{claude-code,pi}.hooks` generated by the tally module. The harness hook is tally's
  Strategy-1 *authoritative detector input*, not a terminal-substrate concern — the dotfiles are
  the wrong owner. Closes CLI-SURFACE §5 flag 2; this is the question the skipped standalone
  hook-utility companion (agentctl — see the 2026-07-09 skip in DECISIONS.md) would otherwise
  have answered.

**tally ASSUMES (the dotfiles already provide it — tally never re-ships it):**
- the entire **zmx substrate** — receiver-everywhere/"projector surface" default, session
  create+name (`new-terminal`, `desk`), reattach over kitten-ssh (`remote.fish`), the resume/project
  fzf pickers (dotfiles #38);
- **niri** workspace + layout (one kitty terminal = a single panel; no per-terminal
  splits/tabs/multiplex; `pane.send-text` uses kitty internals; moving tabs uses niri);
- the **kitty-tab-per-agent projection** and the **`mod+enter`** local-terminal escape hatch.

**Consumer seam back to the dotfiles:** the dotfiles picker's "is a `claude`/`pi` running in this
session?" affordance (#38 Q4) is a **delta-stream consumer of tally's agent-state detector** —
exactly like CUBS. One detector, many readers; no duplicated state.

**The boundary contract (ruled 2026-07-09).** The principle, stated once: tally is **maximally
reliant on** the dotfiles substrate — kitty as its sensor/actuator, zmx as the sole
session-lifecycle owner, ambient `gh` auth, journald, systemd linger — yet **fully externalized
from it**: installed like any other CLI via the flake-input + homeManagerModule pattern, with
session create/name/reattach forbidden to tally. Two seam mechanics ruled with it: the kitty
**`watcher` registration line lives in dotfiles-owned kitty.conf**, and the tally module **exports
the watcher script's store path as a read-only option** so the line never rots; and the
**`claude -p` launch contingency** (CLI-SURFACE §3.1 sidenote) is kept, non-default, as the
**sole boundary exception** — the one place tally *starts* something rather than observes.

**tally does NOT own the kitty-tab-per-agent mechanism or the zmx session lifecycle.** This is the
residue of the kerdr/tally split: tally absorbed the queue/meter/witness/detector, but the terminal
substrate and its projection stay dotfiles config logic. (PS#13 / FS§5 / appendix §5 / dotfiles #38)

---

## Compute pools

tally treats every compute source as a **pool** with three organs: `{budget/capacity model,
gate/lease, meter}`. **pls is the pool primitive** — one broker instance per pool. This is a
foremost ruling: pls is a generic additive-cost budget broker ("GPU VRAM, RAM, license seats, API
tokens, credits — anything with an additive cost"), so a GPU is only its flagship label. tally also
**absorbs pls's configuration role**: pool definitions, capacities, budgets and units are declared
through the pls broker config tally owns, not a second config system. (OUV-CM r1)

**Three consumption meters, all via pls.** pls meters three native consumption axes and tally uses
**all three** rather than bolting on a separate tracker per axis: **GPU-seconds** (the GPU pools —
the proof-of-labor meter), **subscription-minutes** (the rolling subscription usage window), and
**tokens** (subscription / API). Leaning on pls's own metering for each axis is what "leverage pls
maximally" means concretely — one broker, three units, no second cost ledger.

### The pools

| Pool | Unit | Capacity / budget model | Meter | Gate |
|---|---|---|---|---|
| (1a) **worker GPU** (prioritized) | GPU-seconds / VRAM-GB | one pls lease; headless, dedicated to models — the default target for heavy work | **witness-span GPU-seconds** (proof-of-labor, class: verifiable) | pls (the box governor) |
| (1b) controller GPU | GPU-seconds / VRAM-GB | one pls lease; shares the box with chrome/graphical, so light/co-resident tenants only (incl. the DS4 spill) | **witness-span GPU-seconds** (verifiable) | pls (the box governor) |
| (2a/2b/…) Subscription account | window % / tokens | rolling usage window (5h + weekly) — the **programmatic** budget | cc-usage-style poller (annotation) | mechanical `cc-usage --check` admission |
| (3) Pay-per-use API | USD | dollar budget | token→USD counter (annotation) | pls `--cost`, admitted only on an explicit budget UDA |

The controller **NPU** (fastflowlm) exists only for fringe micro-utilities (e.g. session title-picking) and is deliberately **not modelled as a pool** — it never contends for a GPU slot and carries no labor. The worker's NPU is disabled by design (a small GPU-only speed-up). The titling service itself is **dotfiles-side, permanently out of tally's scope** (ruled 2026-07-09): dotfiles **#38** is the canonical write-down (qwen3-0.6b via fastflowlm, session titling from `zmx history`), dotfiles **#40** (nix-strix-halo) the enabling infra.

### One interface, two admission predicates

All pools share `ask → gated → metered`, but there are two admission-predicate kinds and conflating
them is the classic error:

- **Co-residency** (GPU/VRAM): admit while summed `--cost` of tickets *held right now* ≤ budget —
  pls-native BUDGET math.
- **Windowed-consumption** (subscriptions): admit while the meter reads **GO** over a rolling usage
  window.

Subscription pools reuse pls's ticket/queue/lease *shape* (ask-before-use, priority+FIFO,
heartbeat-reaped, fail-open) but their predicate is "the meter says GO," not "summed held cost <
budget." This distinction is the engineering content of the generalization. (OUV-CM r1 nuance)

### selfaware → mechanical: no LLM in the pacing loop

Budget-pacing moves off the LLM entirely. (OUV-CM r2)

- The **poller becomes a systemd user meter daemon** (`Restart=always`) keeping each pool's usage
  cache fresh; staleness self-heals by `Restart=`.
- The **GO/SLOW/STOP decision becomes a mechanical admission gate**, consumed via `cc-usage --check`
  exit codes (`0/1/2/3` = GO/SLOW/STOP/UNKNOWN) one of three equivalent ways: a pls pool admission
  (cleanest — same shape as the GPU lease), a drain-loop gate, or a systemd `ExecCondition=`.
- **Semantics carry over:** `STOP` → don't admit (queue / fall to another pool); `SLOW` → admit only
  cheap/local tiers; `GO` → admit freely.

The dividend is twofold: zero model tokens on the pacing path (selfaware polled before every spawn,
the model burning its budget to measure its budget), and crash-safe gating (the substrate won't
admit past budget regardless of any agent's compliance). A paid plan carries **two** budgets —
interactive credits and a monthly **programmatic** pool (`claude -p` / Agent-SDK / API, effective
June 15 2026); tally's mechanical non-interactive drain spends the **programmatic** pool, and the
subscription meter reads the programmatic budget, never the interactive one.

### No tier-router, but yes budget-gated pool-assigner (explicit reconciliation)

These are **two different altitudes** and do not collide:

- **NO model-tier router** (PS#2). Model class is chosen at **ignition** by the orchestrating model
  that defined the task and carried as a declared task property. tally never escalates or re-picks a
  model — "scale the model up with the task" is a rejected heuristic.
- **YES pool-assigner** (OUV-CM r3), a strictly lower layer. Given an already-chosen model class, it
  picks the concrete account/lease, mechanically (declared-property + gate, never LLM judgment):
  round-robin across subscription pools reading `GO`; **fall back to the local-GPU pool** when every
  subscription window reads `STOP` (the scarce metered GPU is the *cheapest backstop*, because the
  subscription window is what runs out); route the API-dollar pool only on an explicit budget UDA.

The pool-assigner never changes the class — it only routes a fixed class to a live lease. This
automates the cross-account switch selfaware deferred to Tom.

### Meter unification — proof vs annotation

The witness line gains a **`pool`** field and a **trust-class-tagged `charge = {unit, amount,
class}`**:

- **GPU pool** → `{unit: gpu_seconds, amount, class: verifiable}` — the proof-of-labor, derived
  from the **witness span** (the ledger records job start/end natively), covering the GPU pool
  **only**.
- **Subscription / API pools** → `{unit: tokens|usd, amount, class: annotation}` — observability,
  **never the proof**.

`pool` and `charge.class` parallel the existing `labor_class` discriminator, keeping estimated cost
out of the proof slot; day-1 only `pool=GPU` / `class=verifiable` is populated. The verifiable
meter covers the GPU pool only — it is **not** a universal cost ledger. (OUV-CM r4, amended
2026-07-09: timew removed; the witness span is the meter.)

### The governor and GPU=2 (decided)

**pls IS the per-box governor** — one broker per box, no separate in-daemon lock, no new bespoke
slot-lock service, and no wrapping lock. Every heavy tenant — tally or not (ds4-server, the OCR
vLLM) — acquires the pls lease **directly** at its declared priority; pls's RAII/process-exit
release is the single unconditional free (a wrapper would add a second release path). The old
"advisory-lock + MemAvailable check" collapses into a pls **budget pool** (`--cost` = estimated
VRAM-GB). tally **owns the pool configuration** and is simply the highest-priority client. (PS#5)

Hardware is two Framework Desktop nodes (AMD Halo Strix, 128GB unified) over Thunderbolt 3:

- a **controller** — runs tally, the always-on quadlets, kitty and chrome; has a GPU **and** an NPU;
- a headless **worker** — dedicated to models, not sharing the box with graphical programs; GPU-only
  (its NPU is disabled by design for a small GPU speed-up).

So **GPU=2**, one pls pool per GPU, **single-lease-per-pool**. The **worker GPU is prioritized** for
heavy work (headless, uncontended). Ordinary jobs acquire their single box's pool — OCR → worker;
occasional controller parallelism → controller. **DS4** (deepseek-4-flash) is the *only* cross-box
job: an **atomic co-allocation** of a heavy worker-GPU hold + a **light** controller-GPU spill
(secondary, so a small co-resident `--cost` that leaves controller headroom). If either lease can't
be granted, the DS4 dispatch queues. There is no fleet-wide lock — coordination is per-pool leases
plus this one co-allocation. tally runs on the controller and is a client of **both** boxes' pls
brokers (the worker's reachable over the TB3/tailnet link). (PS#5)

### The inner fold — preemption-as-policy

The lease is **non-preemptible**, yet high-priority work never waits for a whole batch: preemption is
a **policy above the lease, by cooperative yield.** The OCR-vs-interactive fold, concretely — the OCR
batch holds the worker lease at **low** priority; a Claude-Code-spawned local `pi` subagent requests
it at **high** priority; the holder **yields at a safe checkpoint** (an OCR page boundary), records
its `session_ref`, releases the lease (RAII / process-exit — the single release path), hands the GPU
to the `pi` subagent, and the batch is later re-dispatched via `--resume`. No forced mid-hold
eviction; no second release path; the interactive job never queues behind the whole batch.

**"tally-compatible" therefore means exactly one thing:** the unit **acquires the pls lease before it
touches the GPU** — no taskwarrior row, no knowledge of the queue required, just a numbered ticket on
the one GPU. Every heavy tenant (tally's or not) obeys this. (appendix §4 / PS#5)

### Reach — orthogonal

tally is built **orthogonal to the reach layer** — it is an offline CLI; how keypresses arrive is
not tally's concern. Tailscale is the default reach (what Tom uses), with Headscale as a pocket
escape hatch. tally's own design never names Tailscale as its mechanism; only if a reach layer
proves genuinely necessary is it built around Tailscale. (PS#19)

### Timing

The local-GPU pool (pls lease + witness) is built **first** as the reference
implementation; subscription pools are the second instance; the API pool is third-only-if-needed.
The multi-pool abstraction is generalized **after** a second real pool exists to prove it against —
no premature one-tenant framework. (OUV-CM r5)

---

## Witness & evidence

The witness ledger is the canonical, permanent, git-independent proof-of-labor — an append-only
JSONL, one immutable line per state transition. Proof lives here and nowhere else: the
TaskChampion/SQLite store is a derived, rebuildable cache, never a second source of truth.
**Ledger-as-truth, taskwarrior-as-derivable-cache.** (PS#9 prop-4)

### Record schema (canonical, on-disk)

The stored record carries the full field set. The 5-field `{task_uuid, gpu_seconds, artifact_hash,
exit_code, evidence_checks[]}` form is a **projection**, never the stored shape. (PS#9)

| Field | Content |
|---|---|
| `task_uuid` | anchor; the same UUID journald and taskwarrior key on |
| `transition_timestamp` | one line per transition |
| `verdict` | enum: `pass \| clean-exit-no-artifact \| …` |
| `exit_code` | numeric exit code |
| `artifact_content_hash` | content hash of the output artifact(s) |
| `gpu_seconds` | metered GPU-seconds (derived from the witness span) — proof-of-labor; absent on cloud runs |
| `wall_clock` | wall-clock duration |
| `attempt` | retry attempt number |
| `lease_epoch` | monotonic fencing token (pls lease generation) |
| `dedup_key` | existence key for skip-if-already-done |
| `labor_class` | `fresh \| recovered \| reused` — non-`fresh` excluded from canonical GPU-seconds |
| `trace_ref` | optional; pi-RPC trace pointer (absent on opaque runs) |
| `pool` | the compute pool that served the unit (day-1: GPU only) |
| `charge` | `{unit, amount, class}`, trust-class-tagged (GPU verifiable; subscription/API annotation) |
| `model` | executing model as a models.dev `provider/model-name` id; absent on shell runs (jul9) |
| `seq` | monotonic sequence number — per-line hash chain (jul9) |
| `prev_hash` | `sha256:<hex>` hash of the prior ledger line in the chain (jul9) |
| `hash` | `sha256:<hex>` over the line's own JSON with the `hash` field cleared (jul9) |

`pool` + `charge` are additive and altitude-preserving — reserved from day-1 (GPU-only populated)
so the multi-pool substrate is a purely additive change with no schema migration. Every heavy
(GPU-touching) unit emits a line — **including a live-orchestrator-spawned unit that takes a pls
lease but gets no taskwarrior row.** The ledger is the universal proof plane, broader than
taskwarrior's row set; witness→content joins go through `session_ref` on taskwarrior/journald and
are **not** duplicated into the proof line. (OUV-CM r4 / appendix)

**`model` is a models.dev id** (jul9): the `provider/model-name` convention — `anthropic/claude-…`,
`openai/gpt-…`, `google/gemini-…`. Ids already containing `/` pass through; bare harness-reported
names are prefix-normalized. Kept from the otherwise-dropped agent-trace episode as the one
zero-cost convention worth carrying.

### Physical append

Plain `O_APPEND` + `fsync` per line. No checksum prefix, no write-temp-then-rename. Each line is a
complete JSON object, so `fsync`-per-line makes "row exists ⟺ work finished" hold for every fully
written line. A crash mid-write loses only the in-flight line; recover() detects the torn trailing
write by JSON parse failure and discards it. This is the accepted tradeoff of the simple append.
(PS#10a)

### Per-line hash chain (jul9)

Every witness line is chained to its predecessor: `seq` (monotonic), `prev_hash` (hash of the
prior line), `hash` = `"sha256:" + hex(sha256(the line's JSON with the hash field cleared))`.
A pure linked-list chain — no Merkle tree, no keys. Without chaining the ledger can be silently
truncated or reordered; with it, tampering is detectable by any observer holding a copy of the
ledger. This is the one steal that survives the agentctl SKIP ruling (jul9): the mechanism is
ported from chocks/agentctl's trace ledger (`pkg/trace/chain.go` semantics, ~150 lines of real
code), and it targets tally's witness JSONL directly.

- **Restart-surviving.** The daemon holds `(last_seq, last_hash)` in memory; on start it recovers
  the chain head by scanning the ledger forward, so the chain continues across daemon restarts —
  one unbroken chain per ledger, not one per process lifetime. A torn trailing line is discarded
  by the same JSON-parse-failure rule as recover() (PS#10a) before head recovery.
- **Independently verifiable.** `tally witness verify` walks records in `seq` order, recomputes
  each `hash`, checks each `prev_hash` against its predecessor, and reports the exact breaking
  `seq` and reason; sequence-gap (completeness) checking is a separate pass. Runs on any copy of
  the ledger — no daemon required.
- **Physical framing unchanged.** PS#10a stands: plain `O_APPEND` + `fsync` JSON lines; the chain
  lives in-record, not as a checksum prefix.
- **Open implementation choice** (not a spec question): one ledger-wide chain vs per-job chains
  keyed by `task_uuid`. The agentctl eval leans per-job as the natural fit for tally's per-job
  witness semantics; agentctl itself uses a single installation-wide chain.

### Evidence gate

Terminal commit gates on **artifact-exists ∧ content-hash-matches ∧ exit-code-ok ∧ witness-span** —
never on an agent's self-report. (2026-07-09, was timew-interval-closed: the witness ledger already
records job start/end natively, so corroboration comes from the witness itself rather than a
repurposed human time-tracker — one fewer moving part, no loss of proof strength.) This is the
single differentiator (the gap durable-execution engines leave). Cloud runs (`gpu_seconds` absent)
satisfy the same floor — the witness records their span natively; their cost is
`charge.class=annotation`, never the proof.

- **Gate-fail forensics.** A run that exits clean but produces no gate-passing artifact is recorded
  `verdict=clean-exit-no-artifact` (excluded from canonical GPU-seconds), mirrored by a
  `TALLY_EVENT=evidence_fail` journald entry. (PS#21)
- **Evidence richness.** The git-independent floor is canonical; optional `trace_ref` (pi-RPC runs)
  and an optional agent-trace projection (git-touching threads) enrich the witness where the runner
  can emit them, and cost nothing on opaque runs. (PS#18)

### The `trust` review UDA (adopted; written down 2026-07-09 — deep-pass T0a, Appendix A Q3)

A `trust` UDA — values `unreviewed | reviewed | recalled` — is written `unreviewed` at job
completion by the L2 wrapper, flipped by the morning-report skill or a voluntary recall, and
queried via `task trust:unreviewed`. It rides TaskChampion UDA config: zero new tally code, and
TaskChampion syncs the field for free. `recalled` is the post-hoc revert record — the field
describes **past** work, never blocks future work (consistent with the no-approval default:
act + witness + revert). MECHANICAL. Motivated independently by git-ai (its explicit amendment
proposal) and agent-trace (`contributor.type` human/ai/mixed/unknown maps onto exactly this
tally-layer field); this lands the Appendix A Q3 resolution that was adopted-in-principle but
never written into the frozen docs.

### recover() — re-present, never replay

Recovery re-derives the queue and re-**presents** in-flight work (`pi --resume`); it never
deterministically replays agent work (agent work is non-replayable). Five invariants:

1. **witness_lsn reconciliation** on boot;
2. **ACK-gated retry** — only retry a unit whose completion was not ACKed;
3. **zombie fencing via lease-epoch** — a stale holder from a prior epoch is fenced;
4. **undeleted-row = re-present** — an unfinished row is re-dispatched, not replayed;
5. **bounded requeue** — attempt-capped, no infinite retry.

A single monotonic **lease-epoch is the only fence** — no leader election, no consensus.
`lease_epoch` = the pls lease generation as primary source, backstopped by a persisted counter
file so it stays monotone across an unclean reboot — the daemon is that counter's sole increment
owner (2026-07-12, issue #9 — was "systemd-incremented": the unit's ExecStartPre bump
double-incremented alongside the daemon's own boot bump and was removed; the daemon's boot-time
bump in `epoch.ts` covers both launch paths, so the persisted file always equals the announced
epoch). Projection rebuild uses
trust-with-cheap-check: compare a ledger tail-hash against the max applied `witness_lsn` on boot;
full replay only on mismatch. (PS#9 / PS#21)

### Retention

Permanent: `gpu_seconds`+`wall_clock` summary, `dedup_key`, `verdict` per line. TTL-prunable:
kerdr/detector logs. (PS#21, amended 2026-07-09 — no timew intervals exist to prune.)

### journald TALLY_* event schema

Every tally event is one structured journald entry (`journalctl -t tally -o json`). journald is
**observability, not load-bearing memory** — the witness ledger is emitted from these fields but
kept a separate artifact. (FS§4, amended per PS#11)

**Emission path (ruled 2026-07-09, with the Bun flip):** the daemon emits the structured TALLY_*
fields via `StandardOutput=journal` in its systemd unit (stdout capture) — not a native
journal-socket client. Bun lacks the AF_UNIX SOCK_DGRAM socket type the native journal protocol
uses; stdout capture is the two-line workaround of record. Native structured emission proving
load-bearing is a recorded flip-back pull toward Rust — not a blocker today.

| Field | Content | Required |
|---|---|---|
| `SYSLOG_IDENTIFIER` | `tally` (fixed) | always |
| `TALLY_EVENT` | `enqueued \| dispatched \| started \| heartbeat \| preempted \| resumed \| completed \| failed \| evidence_pass \| evidence_fail \| witness_emitted` | always |
| `TALLY_TASK_UUID` | task UUID (the witness anchor) | always |
| `TALLY_CLASS` | `high \| medium \| low` | always |
| `TALLY_SOURCE` | `r2 \| gh \| calendar \| manual \| orchestrator` | always |
| `TALLY_AGENT` | `pi \| cc \| shell \| <worker>` | at dispatch+ |
| `TALLY_SESSION_REF` | pi/cc JSONL session id (`--resume` + content-plane join) | when agent-run |
| `TALLY_UNIT` | systemd unit that executed | at start+ |
| `TALLY_EXIT_CODE` | numeric exit code | at completed/failed |
| `TALLY_GPU_SECONDS` | metered GPU-seconds | at completed/failed |
| `TALLY_ARTIFACT_HASH` | content hash of artifact(s) | at completed |
| `TALLY_EVIDENCE` | evidence-gate verdict + checked path(s) | at evidence_* |
| `TALLY_ATTEMPT` | retry attempt number | at dispatch+ (PS#11) |
| `TALLY_LEASE_EPOCH` | monotonic fencing token | at dispatch+ (PS#11) |
| `TALLY_LABOR_CLASS` | `fresh \| recovered \| reused` | at completed/failed (PS#11) |
| `MESSAGE` | one human-readable line | always |

---

## Nix packaging & consumption

### Repo

`github.com/mecattaf/tally` — a self-contained flake, consumed by the dotfiles like any other
input. The design deliberation lives in the notes repo (`projects/tally/`); this repo is the
buildable product. (FS§7)

### Flake outputs

Built on **flake-parts**. Three outputs:

- **`packages.tally`** — ONE Bun-compiled binary (TypeScript; `bun build --compile` +
  `autoPatchelfHook`, packaged via bun2nix — Backlog.md's maintained first-party flake is the live
  reference) that is the daemon *and* the CLI subcommands (thin socket client; verb set deferred to
  the CLI-surface session).
- **`homeManagerModules.tally`** — the **primary** module. Typed options in → generated artifacts
  out (microvm.nix-*shape*, never microvm as a dependency): systemd **user** units (the daemon;
  drain timer with `Persistent=true` + oneshot; the pls broker + pool config; metering/witness), the
  ambient pls-lease-wrap default, and the CLI on PATH. It ships **no** zmx/receiver/kitty config —
  that substrate is dotfiles-owned. Everything tally owns is user-lifecycle, so home-manager is primary.
- **`nixosModules.tally`** — an **unbuilt thin wrapper** stub. No system-level need is named yet; do
  not pre-build it (use-case precedes surface).

### Module option surface (minimal)

`enable` · `role = "conductor" | "receiver"` (does the daemon run here) · `conductorHost` (where
clients reach it) · `sessions`. No `persistence.backend` and no `escapeHatch` — the zmx substrate
and the `mod+enter` escape are dotfiles-owned (tally assumes them). Add an option only when a
concrete role need pulls on it. (PS#17)

`conductorHost` is **pure configuration** — no hostname is frozen anywhere in tally (host names in
doc examples are snapshots, never normative pins), and tally takes **no sequencing dependency** on
the dotfiles hostname rename: dotfiles #39 proceeds independently, not a blocker for this repo
(ruled 2026-07-09).

### The ambient default the module ships

**every heavy (GPU-touching) invocation → pls-lease-wrapped** — GPU-gated by default, so a
CC-spawned pi subagent is tally-compatible without CC knowing tally exists. (The companion "every
terminal → zmx receiver" default is **dotfiles-owned**, not tally's — tally assumes it; see *The
tally / dotfiles boundary*.)

### Inputs & dev rig

Inputs pin only what tally leans on — `pi`, `pls`, and `llama-swap` (**pls named explicitly**,
boundary ruling 2026-07-09: tally owns pls's pool config and broker units, so the binary is pinned
here too — nothing pls-shaped ever lands in the dotfiles); TaskChampion and the `gh` CLI (the
intake antenna) enter as pinned inputs each on its own named trigger, never bundled. taskwarrior
likewise never enters the dotfiles — none exists there today, and it stays that way (2026-07-09).
Dev rig = a
process-compose derivation exposed as `nix run .#dev` booting the daemon against mock jobs;
production is systemd user units. (PS#17-Nix)

### Consumption

```nix
inputs.tally.url = "github:mecattaf/tally";
inputs.tally.inputs.nixpkgs.follows = "nixpkgs";
```

Each host imports `homeManagerModules.tally` and sets `role` (`conductor` on the controller box,
`receiver` on the zenbook-duo) — this places the daemon and points clients at it. The zmx
receiver substrate and the `mod+enter` escape are provided by the dotfiles alongside it, not by this
module.

### Build sequence (spine)

1. Repo bootstrap — flake-parts skeleton, `packages.tally` stub, `homeManagerModules.tally`
   (`enable`/`role`/`package`), `nixosModules.tally` stub, `nix run .#dev` mock daemon, dotfiles pin.
2. pls as the box governor — one pls broker per box, two GPU pools (worker prioritized), "every
   heavy command lease-wrapped" default; tenants acquire directly. DS4 = the cross-box co-allocation
   (worker-heavy + controller-light). (PS#5 decided.)
3. OCR drain as a first-class job — through the ONE spawn-tracked-agent-job primitive, pinned to the
   headless worker; dedup-by-existence; all three trigger surfaces. No filesystem-drain codepath.
4. journald TALLY_* schema (incl. PS#11 fields) + the read-time join query.
5. Witness ledger emitter + evidence gate (witness-span metering); record shaped additively for `pool` + `charge`.
6. zmx session/interactive layer + agent-state detector (in-daemon, agent panes only); delta
   stream.
7. Compose with the dotfiles conductor-receiver env (already provided) — verify tally's detector +
   dispatch + delta stream work across controller / headless worker / zenbook-duo; tally ships no
   receiver config of its own.
8. Second intake — direct `gh` CLI (@-mention → task), OFF by default; query surface per the
   octo.nvim surface scan; proves cross-source urgency ranking.
9. Multi-pool compute-budget substrate — **after** the single-GPU meter+witness; selfaware poller →
   `Restart=always` meter daemon; `cc-usage --check` mechanical gate; pool-assigner (budget-gated,
   NOT a model-tier router).
10. CUBS delta-stream consumer — external to this repo; unblocked once the delta-stream contract
    freezes.

The router step from the old FS§9 is **dropped** (PS#2: model choice is made at ignition).

Two 2026-07-09 rulings bind this spine: **a given build pass builds the CLI only — the
non-interactive front-end substrate comes at the END** (step 10 territory); and the stack is
**TypeScript on Bun** — step 1's detailed chunk plan is pending a Bun-shaped rewrite (was
Rust-shaped; see BUILD-SEQUENCE.md).

---

## What tally is NOT (guardrails) + capability statement

tally is **not**:

- **not a multiplexer** — no windows/tabs/splits; niri owns layout, one kitty terminal is a single
  panel;
- **not an orchestrator** — it interprets no workflow DAG and hardcodes no team paradigm; every
  harness is a client of one enqueue verb;
- **not a model-tier router** — it never selects or escalates a model class (chosen at ignition);
- **not a second source of truth** — the witness ledger is proof, taskwarrior is a derivable cache,
  the harness JSONL is content tally only points at;
- **not a filesystem-as-queue** — no drain codepath is baked in; OCR is an ordinary job;
- **not a content store** — it copies no conversation, reasoning, or tool-call bytes;
- **not a reach layer** — it is an offline CLI; how keypresses arrive is not its problem;
- **not a human PIM store** — no CalDAV/vdirsyncer/khal; no Baikal; no email bridge;
- **not the owner of the kitty-tab-per-agent projection** — that is the nix desktop / dotfiles;
- **not a sandbox / isolation layer** — process/filesystem isolation is not tally's; if an agent
  needs it, isolation is invoked by a specific **skill inside the pi/cc session tally wraps**, a
  payload of the wrapped run, never a tally primitive. (PS#appendix)

**Recorded doctrine, currently DORMANT (Tom's Q2 relaxation — ruling of record, 2026-07-09).**
Approval/escalate gating is LEGAL as an explicit **opt-in operator flag** — analogous to Claude
Code's permission modes (dangerously-skip-permissions / accept-edits / plan mode). The DEFAULT
remains no-approval: act + witness + revert. **No tally component implements this** — the daemon
gains no new states (a gated agent simply reads `blocked`, a detector state that already exists on
the delta stream) and no escalate flag exists anywhere in the surface. Recorded so a future opt-in
escalate flag is a doctrine-compliant addition, never a re-litigation.

**Capability statement (worktrees, R4).** tally absorbs the external-worktree-orchestrator family
(cmux-craigsc, claude-squad, workmux, agtx, repowire) as a **`cwd`/worktree attribute** on the
spawn-tracked-agent-job — not a separate orchestrator, not a worktree engine, not a tmux lifecycle
CLI. tally provides the field; the orchestrator LLM decides worktree policy. Worktree isolation is a
cwd/branch policy, orthogonal to GPU metering. Concretely: tally **can** dispatch N agents into N
isolated worktrees, serialize their GPU use on one per-box lease, witness each, and stream their
state; it **cannot** and will not decide the branching strategy, render their tabs, or lay them out
— those belong to the orchestrator LLM, CUBS, and niri respectively. This capability and its limits
are documented so users understand what tally can and cannot do.
