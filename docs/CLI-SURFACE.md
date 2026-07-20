# tally — CLI surface & the Seam-B wire contract (frozen)

> Authored 2026-07-07 from source analysis over `vendor/` (herdr · cmux-manaflow · cmux-craigsc ·
> zmx · kitty · pi) plus the live dotfiles zmx integration. This is the **frozen** contract every
> subsequent build step's "thin CLI" / "delta stream" acceptance shape refers to (BUILD-SEQUENCE
> step 1 onward). It ratifies the standing decisions in `DECISIONS.md`/`SPEC.md`; where synthesis
> over-reached, the adversarial pass pulled it back to the boundary (see the change log at the end).
>
> Status legend for shapes below: **JSON output is the contract** (the one-record-per-line
> projection), text output is a convenience. `--json` everywhere.

---

## 0. Conventions (the shape of the whole surface)

- **One binary.** `packages.tally` is a single Bun-compiled binary (TypeScript) that is *both* the daemon *and* this
  thin socket-client CLI. Every CLI verb is a request against the daemon's Unix socket (§2).
- **Verb-prefix convention.** `tally <noun> <verb>`, verbs grouped by noun:
  `queue` · `session` · `pane` · `agent` · `query`. The **one** exception is `tally enqueue`,
  exposed *both* as the canonical `tally queue enqueue` *and* as a top-level alias, because Seam A
  is the load-bearing verb every orchestrator binds to.
- **Two seams, nothing else.** Seam A = the one `enqueue` control verb (§1.1). Seam B = the delta
  stream (§2). Every other verb is a thin read/projection over one of these two.
- **The status enum is frozen at exactly four:** `blocked | working | done | idle`. herdr's fifth
  value `unknown` is an internal transient only; it never reaches the wire (it collapses to
  last-known, or `idle` at first sight). Adding a fifth wire status would be a protocol bump.
- **The three keys, never conflated:**
  - `persistence_session_id` — the **zmx** session handle (a dotfiles timestamp name like
    `term-0707-1530`). The `session` leg of the data model.
  - `kitty_window_id` — the kitty-native binding; the focus/tunnel-in key. The `pane` leg.
  - `session_ref` — the **harness JSONL** id (`pi`/`cc` `--resume`, content-plane join). A *different*
    key from `persistence_session_id`.
- **Data model.** `{session, pane} → (persistence_session_id, kitty_window_id, agent{kind, status})`.
  Grouping tier **Workspace → Session → Pane** (collapsible; status dots aggregated up). `pane.id`
  encodes the composite key as `"<session>:<pane>"`.
- **Model choice is never made by tally.** `--model-class` is a *declared* property carried from
  ignition; the pool-assigner routes a fixed class to a live lease but never re-picks it (PS#2).

---

## 1. The frozen CLI verb set

Five groups. Verbs marked **(Seam A)** / **(Seam B)** are the load-bearing ones; the rest are
projections. Where a verb delegates to a dotfiles-owned surface it says so explicitly — tally
**reads/points-at**, it never manages that surface.

### 1.1 `queue` — the control plane (Seam A lives here)

| Verb | Purpose | Key args | `--json` shape |
|---|---|---|---|
| **`tally enqueue`** *(alias of `tally queue enqueue`)* **(Seam A)** | The **sole** control verb — admit one spawn-tracked-agent-job. Binding point for `agent()`/`parallel()`/`crew_spawn`/`fork`/TeamCreate/timers/`events/`. `--wait` subsumes `wait_for_subagents`. | see **§1.1a** | `{task_uuid, job_id, lease_epoch, pool, status, session_ref, dedup_key, witness_lsn, verdict}` |
| `tally queue cancel <uuid\|selector>` | Remove a queued/dispatched unit; `--force` fences an in-flight holder by `lease_epoch`. | `--force` | `{task_uuid, status:"cancelled", was, lease_epoch}` |
| `tally queue pause [pool \| --all]` | Stop admitting new leases for a pool (drain gate; running holders keep their lease). Backs the cc-usage STOP/SLOW admission semantics. | `[pool]`, `--all` | `{paused:true, pool, queued_depth}` |
| `tally queue resume [pool \| --all]` | Resume admission for a paused pool. | `[pool]`, `--all` | `{paused:false, pool, queued_depth}` |

`cancel`/`pause`/`resume` are tally-native (neither herdr nor cmux has a queue); fencing = the
`recover()` lease-epoch invariant, the drain gate sits over the pls broker admission predicate.

#### 1.1a Seam A — `tally enqueue`, in full

Every orchestrator (CC dynamic workflow, `pi` crew/fork/chain, a human, a systemd timer, the
`events/` drop dir, the OCR firehose) is a **client of this one verb**. Bind to this generic seam,
never to any harness's team mechanism (OUV-MH R2). Model choice is **not** made here.

```
tally enqueue \
  --priority   <high|medium|low>                      # TaskChampion urgency; TALLY_CLASS
  --source     <r2|gh|calendar|manual|orchestrator>   # provenance (TALLY_SOURCE); no path privileged (PS#16)
  --kind       <pi|claude-code|shell>                 # agent.kind — the only three (PS#20)
  --invocation "<cmd>" | -- <argv...>                 # the leaf-worker command run under the pls lease
  --cwd <path> | --worktree <branch>                  # absorbs the worktree-orchestrator family as a FIELD (OUV-MH R4)
  --evidence   <check>   (repeatable)                 # artifact:<path> · hash:<algo> · exit:<code> ; witness-span implicit
  --pool       <worker-gpu|controller-gpu|sub:<acct>|api>   # pool hint for the budget-gated assigner (OUV-CM r3)
  --model-class <class>                               # DECLARED, carried from ignition — tally never escalates (PS#2)
  --dedup-key  <key>                                  # dedup-by-existence
  --session    <persistence_session_id>               # OPTIONAL: bind to an EXISTING zmx session (read, never create)
  --barrier <gid> | --wait-group <gid> | --wait-count <N> | --wait | --timeout <dur> | --detach | --json
```

**`--invocation` has no shell semantics.** The `"<cmd>"` string is tokenized by a small quote-aware
argv splitter (honoring single/double quotes and backslash-escapes) and the resulting argv is exec'd
**directly** — tally never runs it through a shell (no glob, no env expansion, no redirection, no
pipes, no `&&`/`;` chaining). A command that needs any of those needs an *explicit* shell: pass
`-- sh -c "..."` as the leaf argv. Redirection/pipe characters typed into `--invocation` become
literal argv tokens handed to the leaf command, not shell operators — e.g. `--invocation "printf x >
out"` runs `printf` with the literal arguments `x`, `>`, `out`; nothing is redirected. The evidence
gate (§1.1a) is the backstop that catches a run like this at verdict time, but prefer the `-- sh -c`
form up front when the leaf command needs shell semantics.

**Durable-row admission (appendix).** A TaskChampion row is written **only if** the unit needs
cross-source urgency ranking **or** must survive a crash to be re-dispatched (autonomous/batch/queued
— the OCR firehose, gh intake, timers). A unit **spawned live by a running orchestrator** takes a
pls lease + emits a witness line but gets **no row** → `task_uuid` is `null`; its queue position is
the live lease queue, its record is the JSONL + the witness.

**Dedup-by-existence.** Before dispatch, `stat`+`grep` the on-disk artifact and the success witness
for `--dedup-key`; if present, **skip** the GPU run, tag `labor_class=reused`, exclude from canonical
GPU-seconds, return `status:"reused"`.

**`--wait` / barrier semantics — the only place `wait_for_subagents` lives.**
- `--detach` (default): return once admitted/queued.
- `--wait`: the thin CLI **blocks** until *this* unit's terminal delta (`job.completed | job.failed
  | job.evidence_fail`) via the daemon's job-subject wait (§2.4 semantics — an already-terminal
  delta resolves immediately, so a fast unit never races the subscribe). The identity is exact,
  never a fence: a rowed unit keys by `task_uuid`; a rowless (`task_uuid:null`) unit by the `job_id`
  its enqueue result carries (additive result field, not a protocol bump §2.5 — terminal deltas
  carry no `lease_epoch`, so an epoch comparison cannot identify a job; issue #4). The process exit
  code mirrors the verdict (`0` = `evidence_pass`; non-zero = `failed` / `clean-exit-no-artifact`).
  tally owns the wait off **its** daemon; no harness hand-rolls a socket.
- **A parallel barrier = enqueue-N-await-N:** call `tally enqueue --barrier <gid> …` N times, then
  block on `tally enqueue --wait-group <gid> --wait-count N` (equivalently `tally agent wait`), which
  awaits N terminal done-deltas off **one** stream. This *is* `wait_for_subagents`.
- `--timeout` bounds any wait; on timeout the units keep running (barrier ≠ cancel).

Every heavy (GPU-touching) unit — row or no row — emits a witness line; the ledger is broader than
the taskwarrior row set.

### 1.2 `session` — READ/observe existing zmx sessions (lifecycle is dotfiles-owned)

> **Boundary (enforced by the adversarial pass).** tally **does not create, name, attach, reattach,
> or detach** zmx sessions — that whole lifecycle is the dotfiles' (`zmx attach <s> [fish]`,
> `zmx a <s>`, the `desk`/`desk-resume` fzf pickers, `remote.fish`; dotfiles #38). tally holds only
> **read/observe** verbs. Reach is orthogonal (PS#19): **no `--remote` flag exists** — how a client
> reaches the daemon is not tally's concern.

| Verb | Purpose | Key args | `--json` shape (one record/line) |
|---|---|---|---|
| `tally session list` | List live sessions with the Workspace→Session→Pane rollup + aggregated status dots. **Delegates the enumeration read to `zmx list --short`**, joined with tally's `{session,pane→agent}` model. | `--short`, `--workspace <w>` | `{session, persistence_session_id, workspace, status_rollup, panes:[{pane, kitty_window_id, agent:{kind,status}}]}` |
| `tally session watch [session \| --all]` **(Seam B)** | Subscribe to the delta stream (§2): initial snapshot then newline-JSON events. **This viewer pane is excluded from the detector manifest set** (anti-self-loop invariant #4). | `--all`, `--snapshot-only`, `--since <seq>`, `--format <jsonl\|tree>` | snapshot line, then event lines `{seq, event, session, pane, agent:{kind,status}}` |

To create/attach a session, a caller uses the dotfiles surface directly (`zmx attach <s> fish`) —
tally never wraps it. `session watch` is how the dotfiles picker's "is a `claude`/`pi` running
here?" affordance (#38 Q4) reads state: one detector, many readers.

### 1.3 `pane` — the minimal kitty-native binding (keyed on `kitty_window_id`)

One kitty terminal = one niri panel. **No splits, tabs, or multiplex.** tally **observes** panes it
sees (via `pane.created`); it does not launch windows (niri/dotfiles own the kitty-tab-per-agent
projection).

| Verb | Purpose | Key args | `--json` shape |
|---|---|---|---|
| `tally pane send <sel> <text>` | Send text into a pane via **kitty internals** (`@ send-text`) — not a pty interpose. | `--enter` | `{pane, kitty_window_id, sent:true}` |
| `tally pane send-key <sel> <key>` | Send one key/chord (`enter`, `esc`, `ctrl+c`, …) via kitty internals. | — | `{pane, kitty_window_id, key, sent:true}` |
| `tally pane focus <sel>` | Focus/tunnel-in to the pane's kitty window (`@ focus-window`). Cross-terminal *moves* belong to niri. | — | `{pane, kitty_window_id, focused:true}` |
| `tally pane capture <sel>` | Read pane text **out-of-band** via throttled `kitty @ get-text` — the same read the detector uses; never interposes on the pty stream. **`--source detection` refuses `is_viewer` panes** (mirrors the detector's exclusion). | `--source <visible\|recent\|detection>`, `--lines <n>`, `--format <text\|ansi>` | `{pane, kitty_window_id, source, lines, text}` |

`<sel>` is a `session`/`pane`/`agent` selector (`term-0707-1530:p2`, an `agent_id`, or a bare
`pane`). Lineage: herdr `pane.send_text`/`send_keys`/`read`; cmux `send`/`send-key`/`read-screen`.

### 1.4 `agent` — herdr's socket verb *vocabulary*, adopted clean-room

Read-projections of the in-daemon detector, scoped to **agent panes only**. **`agent.start` is
deliberately absent** — starting an agent *is* `tally enqueue` (Seam A).

| Verb | Purpose | Key args | `--json` shape |
|---|---|---|---|
| `tally agent list` | Every detected agent with kind+status+`session_ref` (the delta-stream projection as a table). | `--status`, `--kind` | `{session, pane, kind, status, session_ref, cwd}` |
| `tally agent get <sel>` | One agent record in full (incl. `agent_session` ref for `--resume` joins). | — | `{pane, kind, status, agent_session:{kind,value}, foreground_cwd}` |
| `tally agent read <sel>` | Out-of-band read of an agent pane's **detection** snapshot (the bottom-buffer the detector classifies). | `--source`, `--format` | `{pane, source, text}` |
| `tally agent explain <sel>` | Detector debug: *why* a pane is `working/blocked/done` — matched rule, manifest source/version, `strategy(hook\|scrape)`. Backs the TOML-manifest tuning loop. | — | `{pane, state, manifest, matched_rule, strategy}` |
| `tally agent wait <sel>` | One-shot blocking wait on a semantic agent state (the primitive the enqueue `--wait` barrier uses; exposed for scripts). | `--status <done\|blocked\|idle\|working>`, `--timeout`, `--count` | `{pane, status, reached:true, waited_ms}` |
| `tally agent send <sel> <text>` | Steering message to a running agent (non-blocking crew steering). Resolves to the pane, then `pane send`. | `--enter` | `{pane, kind, sent:true}` |
| `tally agent focus <sel>` | Focus the agent's kitty window. | — | `{pane, kitty_window_id, focused:true}` |

### 1.5 `query` — read-time joins over the JSON-projection contract

No bespoke audit log; everything is a projection over `task export × journalctl -t tally -o json ×
git log × harness JSONL`, keyed on `session_ref` (the four-log join; timewarrior removed 2026-07-09).

| Verb | Purpose | Key args | `--json` shape |
|---|---|---|---|
| `tally query status` | Live snapshot: per-pool lease/queue depth, session-tree rollup, `protocol_version`. The `ping`+status probe. | `--pool` | `{protocol_version, pools:[{pool,held,queued,budget,broker_queued?,diverged?}], sessions:[{session,status_rollup}]}` |
| `tally query log` | Tail/replay the witness ledger + journald `TALLY_*` events as one filtered feed. | `--task`, `--session`, `--event`, `--since`, `--follow` | `{TALLY_EVENT, TALLY_TASK_UUID, TALLY_GPU_SECONDS, TALLY_ARTIFACT_HASH, verdict}` |
| `tally query render` | Render the Workspace→Session→Pane tree / status / ledger in a chosen projection (the grouping tier for consumers like CUBS). | `--format <text\|json\|jsonl\|tree\|jcal>`, `--scope <sessions\|queue\|witness>`, `--collapse` | `{workspaces:[{workspace, sessions:[{session, panes:[…], status_rollup}]}]}` |
| `tally query standup` | The read-time-join digest keyed on `session_ref` → "what happened / what is in-flight". | `--since`, `--source`, `--format <text\|json\|md>` | `{window, completed:[{task_uuid,gpu_seconds,verdict,session_ref}], in_flight:[…], reused, gate_fails, cancelled:[…]}` |

> **`query status` broker/daemon divergence (issue #5, fixed).** `pools[].queued` keeps its frozen
> meaning — the jobs engine's own per-pool depth. `broker_queued` is an additive field carrying the
> broker's waiting-ticket count straight off `pls status` (`BrokerPoolStatus.queued`), and `diverged`
> is `true` when the two disagree. Both are omitted on a transient broker-read failure (the existing
> degrade-don't-fail path); additive-optional fields are not a protocol bump (§2.5). Filed after the
> 2026-07-11 incident where the daemon reported `queued:2` while the broker held `queued:93` — the
> engine-only number alone made a starved pool look nearly empty.

> **RECOMMENDED-ADOPT — explicit ruling PENDING (deep-pass A2, 2026-07-09; not yet frozen, do not
> build as ruled).** `tally query standup --stale-hours N`: an optional flag adding a `stale`
> bucket to the JSON output alongside `completed`/`in_flight`/`reused`/`gate_fails` — the
> `in_flight` jobs whose last `job.heartbeat` is older than N hours. Rides the existing read-time
> join; the daemon already tracks last-heartbeat per job (the `gpu_seconds` tick). No new wire
> events, no new storage. Value: catches a hung overnight OCR/CC run parked in `in_flight` without
> a terminal event — the difference between noticing and losing a night of GPU time. The deep-pass
> recommendation is adopt-now; Tom has not ruled.

> **`query standup` bucket quirks (issue #7, fixed).** Three digest fixes from the 2026-07-11
> on-device audit. (1) `cancelled` is an additive top-level array alongside
> `completed`/`in_flight`/`gate_fails` — cancelled rows were success-shaped under `completed`,
> inflating the count; additive-optional fields are not a protocol bump (§2.5). (2) `gate_fails`
> widened from the ledger's `clean-exit-no-artifact` verdict alone to the union with tasks whose
> journald spine carries an `evidence_fail` event — a recovered/exit-nonzero evidence fail is
> witnessed as plain `failed`, the gate detail lives only in journald (PS#21 forensics). (3)
> `--stale-hours` stays unbuilt (ruling pending, above) but now warns on stderr instead of being
> silently swallowed — the operator believed they had filtered and had not.

---

## 2. Seam B — the delta-stream wire schema (contract-first)

The highest-leverage artifact: kerdr, CUBS, the notifier, the dotfiles picker, and the `--wait`
barrier are all pure consumers of **one** stream.

### 2.1 Transport

- **Socket.** One Unix-domain stream socket at `$XDG_RUNTIME_DIR/tally/tally.sock` (mode `0600`,
  single operator). **Local-only** by construction — no TCP/UDP, no roaming (zmosh dropped). Remote
  boxes reach the daemon the same way they reach sessions, over `kitten ssh harness-desktop` —
  that reach is dotfiles-owned and **not named inside tally's design** (PS#19).
  `loginctl enable-linger` keeps the daemon + socket alive across logout.
- **Framing.** NDJSON — exactly one UTF-8 JSON object per line, LF-terminated, no raw embedded
  newlines. One connection carries three frame kinds, disambiguated structurally:
  - **request** `{id, method, params}`
  - **response** `{id, result | error}` (correlate by `id`)
  - **event** `{seq, event, …}` (unsolicited, no `id`)
  A connection may interleave RPC and pushed events after `session.subscribe`.
- **Seq + dedupe.** Every *replayable* event carries a monotonic `seq` (per `lease_epoch`) and a
  stable `id` (uuid) for idempotent dedupe across a resume overlap. Clients persist the last `seq`
  (a cursor-file is the idiom) and reconnect with `session.subscribe{from_seq:last_seq}`.
- **Replay + reconnect.** The daemon keeps a bounded in-memory replay ring (**tally's own bound:
  4096 events** — a memory budget, not inherited from any source). The subscribe ACK's `resume`
  block reports `after_seq/oldest_seq/latest_seq/next_seq/gap`. `gap=true` (requested `from_seq`
  older than `oldest_seq`) **or** a changed `lease_epoch` ⇒ the client MUST `session.snapshot` then
  re-subscribe from `snapshot.seq`.
- **Bounds/backpressure.** Per-frame cap **64 KiB** (`pane.output_matched.read.truncated=true` when
  a scrape read exceeds it). A subscriber whose unacked backlog exceeds **1024 frames** gets a final
  `stream.overflow` frame and is disconnected (it reconnects + re-snapshots). Idle connections get a
  `heartbeat{latest_seq}` every ~15s unless `include_heartbeat=false`. *(These bounds are tally's,
  set from its own frame budget; cmux's `events.jsonl` is the design reference, not the source of the
  constants — see clean-room note §4.)*

### 2.2 `session.snapshot` — the bootstrap frame

One-shot request/response (`method:"session.snapshot"`); **not** a subscription. Read once, seed the
local cache, then `session.subscribe(from_seq = snapshot.seq)`. Re-fetch after reconnect when the
epoch changed or a gap was reported.

```jsonc
{
  "protocol": "tally.delta",       // fixed stream identifier
  "protocol_version": 1,           // integer; bumps only on a breaking change (§2.5)
  "daemon_version": "0.1.0",       // informational semver of the one binary; never gates behavior
  "lease_epoch": 42,               // monotonic fence (PS#9/#11). Changes ONLY on daemon (re)start.
                                   //   New epoch ⇒ client's seq cursor is void ⇒ MUST re-snapshot.
  "seq": 90714,                    // latest event seq at snapshot time; subscribe from here
  "ts": "2026-07-07T15:30:04.512Z",

  "focus": { "workspace": "harness-desktop", "session": "term-0707-1530", "pane": "term-0707-1530:p2" },

  // TREE TIER 1 — Workspace (niri panel grouping; tally OBSERVES, niri owns layout)
  "workspaces": [
    { "id": "harness-desktop", "label": "harness-desktop", "focused_session": "term-0707-1530" }
  ],

  // TREE TIER 2 — Session (a zmx session; DOTFILES-OWNED. tally never creates/names/attaches)
  "sessions": [
    { "id": "term-0707-1530", "workspace_id": "harness-desktop",
      "persistence_session_id": "term-0707-1530",   // the handle `zmx attach <session>` uses
      "backend": "zmx",
      "observed_at": "2026-07-07T15:30:01Z",         // when tally first SAW a pane in it (not creation)
      "pane_ids": ["term-0707-1530:p1", "term-0707-1530:p2"],
      "status_rollup": { "blocked": 0, "working": 1, "done": 0, "idle": 1 } }  // client aggregation hint
  ],

  // TREE TIER 3 — Pane (one kitty window; one kitty terminal = one panel)
  "panes": [
    { "id": "term-0707-1530:p2", "session_id": "term-0707-1530",
      "kitty_window_id": 7,                          // focus/tunnel-in key
      "cwd": "/home/tom/work/api", "worktree": "worktree/api",  // carried from the job at ignition
      "agent_id": "ag_91be",                         // → agents[] (null for a bare shell / viewer)
      "is_viewer": false }                           // true = a `tally watch` pane; detector NEVER scrapes it
  ],

  // Agent records — the agent{kind,status} leg, keyed to a pane
  "agents": [
    { "id": "ag_91be", "pane_id": "term-0707-1530:p2", "session_id": "term-0707-1530",
      "kind": "claude-code",                         // pi | claude-code | shell (declared at ignition)
      "status": "working",                           // blocked | working | done | idle — the only 4
      "custom_status": "editing",                    // opaque harness sub-label; NOT canonical
      "detector": "hook",                            // hook (cooperative, AUTHORITATIVE) | scrape (fallback)
      "persistence_session_id": "term-0707-1530",
      "session_ref": "3d9c1a2e-…",                   // harness JSONL id (may be null)
      "job_id": "job_0f21",                          // set when this agent is a dispatched job; null if interactive
      "since": "2026-07-07T15:30:03.9Z" }
  ],

  // In-flight jobs — bootstrap for the --wait barrier so a late subscriber sees pending work.
  //   Job event names mirror journald TALLY_EVENT verbatim (ONE vocabulary, not a second source of truth).
  "jobs": [
    { "job_id": "job_0f21", "task_uuid": "b2c4…-uuid", "state": "started",
      "class": "high", "source": "orchestrator", "agent_kind": "claude-code",
      "pane_id": "term-0707-1530:p2", "lease_epoch": 42, "attempt": 1, "gpu_seconds": 0 }
  ]
}
```

### 2.3 The delta-event enumeration

Every consumer **MUST ignore unknown event names and unknown fields** (forward-compat is a hard
contract). Event families: `agent.*`, `pane.*`, `session.*`/`workspace.*` (observational), `job.*`
(mirror journald `TALLY_EVENT`), and stream-control frames.

**Agent (the spine).**

| Event | When | Payload |
|---|---|---|
| `agent.detected` | A harness agent is first identified in a pane (kind classified via hook handshake or scrape signature). One per agent per pane lifetime. | `{agent_id, pane_id, session_id, kind, status, detector, persistence_session_id, session_ref?, kitty_window_id}` |
| `agent.status_changed` | **SPINE event.** Every real transition of the 4-state status. Internal `unknown` never reaches the wire. | `{agent_id, pane_id, session_id, status, prev_status, detector, custom_status?, since}` |
| `agent.blocked` | Convenience frame emitted **right after** the `agent.status_changed` whose target is `blocked` (awaiting operator input; cmux `needsInput`). Lets consumers filter by name. | `{agent_id, pane_id, session_id, detector, reason?, prompt_excerpt?, since}` |
| `agent.done` | Convenience frame right after the `agent.status_changed` whose target is `done` (cmux `Stop`). Feeds notifier, crew steering, and the `--wait` barrier for interactive agents. | `{agent_id, pane_id, session_id, detector, since}` |
| `agent.released` | Agent authority ends: process exited, authority cleared, or pane closed. Record removed. | `{agent_id, pane_id, session_id, reason:"exited\|cleared\|pane_closed"}` |

> `agent.blocked`/`agent.done` are **tally-native convenience frames over herdr's blocked/done status
> *value*** — herdr has no such events (only `pane.agent_status_changed` carrying the value). The
> spine is `agent.status_changed`; the two frames are pure filter sugar.

**Pane (observed kitty windows).**

| Event | When | Payload |
|---|---|---|
| `pane.created` | A new pane (kitty window) becomes **visible to the detector** inside an observed zmx session. tally observes; it does not launch. | `{pane_id, session_id, kitty_window_id, cwd, worktree?, is_viewer}` |
| `pane.closed` | The pane's kitty window is gone. | `{pane_id, session_id, reason}` |
| `pane.focused` | Focused pane changes (drives kerdr highlight; updates `snapshot.focus`). | `{pane_id, session_id, workspace_id, prev_pane_id?}` |
| `pane.output_matched` | The out-of-band detector (throttled `kitty @ get-text`, **agent panes only**) or an active `session.wait` `pane_output` predicate (**also agent-panes-only; `is_viewer` rejected**) matches a region+regex. Never interposes on the pty stream. | `{pane_id, session_id, matched_line, read:{source, format, text, revision, truncated}}` |

**Session / Workspace (observational — tally never creates).**

| Event | When | Payload |
|---|---|---|
| `session.observed` | tally first sees any pane belonging to a zmx session (it does **not** create it). Maintains tier 2. | `{session_id, workspace_id, persistence_session_id, backend:"zmx", observed_at}` |
| `session.ended` | The last pane of a zmx session leaves tally's view (the zmx session itself may persist). | `{session_id, workspace_id, reason}` |
| `workspace.focused` | Focused niri panel changes; updates `snapshot.focus` and tier 1. | `{workspace_id, prev_workspace_id?}` |

**Job (mirror journald `TALLY_EVENT` verbatim — one vocabulary, not a second store).**

| Event | When | Payload |
|---|---|---|
| `job.enqueued` | Seam-A enqueue accepted (any of the 3 ingress paths). | `{job_id, task_uuid, class, source, agent_kind, invocation, cwd, worktree?, evidence_spec, priority}` |
| `job.dispatched` | Job took the single pls GPU lease, handed to a systemd unit (serialize point). | `{job_id, task_uuid, agent_kind, unit, lease_epoch, attempt}` |
| `job.started` | Agent process live; join to the pane/agent records. | `{job_id, task_uuid, pane_id?, agent_id?, session_ref?, unit, ts}` |
| `job.heartbeat` | Throttled liveness + running GPU-seconds tick. | `{job_id, gpu_seconds}` |
| `job.preempted` | Running job yielded the lease to higher-priority work. | `{job_id, reason}` |
| `job.resumed` | A preempted/recovered job re-took the lease (`recover()` re-presents, never replays). | `{job_id, labor_class:"recovered\|reused", lease_epoch, attempt}` |
| `job.evidence_pass` / `job.evidence_fail` | Evidence gate verdict (incl. `verdict=clean-exit-no-artifact` on fail). | `{job_id, task_uuid, verdict, checked_paths[]}` |
| `job.completed` | **TERMINAL success.** Satisfies a `--wait`/`session.wait` barrier. | `{job_id, task_uuid, exit_code, gpu_seconds, artifact_hash, labor_class}` |
| `job.failed` | **TERMINAL failure.** Also satisfies a barrier (error outcome). | `{job_id, task_uuid, exit_code, gpu_seconds, verdict?, labor_class}` |
| `job.witness_emitted` | Witness-ledger entry written (a separate artifact from journald). | `{job_id, task_uuid, witness_ref}` |

> **`prev_*` shadow fields (adopted; written down 2026-07-09 — deep-pass T0b, Appendix A Q4).**
> Selected `job.*`/`pane.*` events carry optional `prev_*` fields (`prev_state`, `prev_status`)
> derived from TaskChampion's operation log — which already computes attribute-level deltas for
> undo/sync, so tally re-derives nothing ("lean maximally on TaskChampion internals"). Consumers
> SHOULD read `prev_*` rather than re-derive prior state. Additive-optional: never bumps
> `protocol_version` (§2.5).

**Stream-control frames.**

| Event | When | Payload |
|---|---|---|
| `heartbeat` | ~15s when idle (suppressible). **Not replayable** — no own `seq`, does not advance the cursor. | `{ts, latest_seq}` |
| `stream.overflow` | Final frame to a slow subscriber whose unacked backlog exceeded 1024, just before disconnect. Client reconnects + re-snapshots. | `{reason, oldest_seq, latest_seq}` |

### 2.4 Control verbs (RPC methods, grouped under `session.`)

| Method | Shape |
|---|---|
| `session.snapshot` | req `{method:"session.snapshot"}` → the §2.2 bootstrap frame. One-shot, no side effects; safe to re-call to reseed. |
| `session.subscribe` | params `{from_seq?, names?:[string], categories?:[agent\|pane\|session\|workspace\|job\|control], include_heartbeat?=true, min_protocol?, max_protocol?}`. First response is an ACK `{type:"subscription", subscription_id, protocol_version, epoch, resume:{after_seq, oldest_seq, latest_seq, next_seq, gap}}`. `gap==true` **or** a changed epoch ⇒ re-snapshot + re-subscribe. Omitting `from_seq` subscribes live from `next_seq`. |
| `session.wait` | params `{predicate, timeout_ms?}`. One-shot **blocking** (internally subscribe+filter+first-match+auto-unsubscribe). **This is the Seam-A `--wait` barrier.** `predicate.subject` one of: `job {job_ids[], until:["completed","failed"], count:N}` (barrier = enqueue-N-await-N); `agent {agent_ids[], until_status:"done"\|"blocked", count:N}`; `pane_output {pane_id, regex}` — **constrained to agent panes; `is_viewer` panes rejected** (upholds anti-loop invariant #4). Result carries the satisfying event(s); on timeout `{timed_out:true, satisfied, pending}`. |
| `session.ack` | params `{subscription_id, seq}`. Advances the daemon's per-subscriber cursor (prunes backlog, resets the slow-subscriber counter, doubles as liveness). Recommended so a healthy-but-quiet reader never trips the 1024-pending disconnect. |
| `session.unsubscribe` | params `{subscription_id}`. Cleanly closes the push stream; the socket may stay open for further RPCs. |

### 2.5 Versioning — two orthogonal axes

- **`protocol_version`** (integer, starts at `1`, carried in the snapshot **and** the subscribe ACK).
  **Additive changes never bump it** — new event names, new categories, new *optional* fields are
  always allowed, and every consumer MUST ignore unknowns. A bump (`1→2`) happens **only** on a
  breaking change: removing/renaming a field, narrowing an enum (e.g. adding a 5th status), or
  changing an existing field's meaning. Clients MAY negotiate `min_protocol`/`max_protocol`; the
  daemon returns `{code:"unsupported_protocol", supported:[…]}` if it can't serve the range.
- **`lease_epoch`** is a *runtime fence* (PS#9/#11 — the single monotonic fence, no election/consensus),
  **not** a schema version. It changes on every daemon (re)start; `seq` is monotonic only *within*
  an epoch. A client that sees a new epoch treats its cursor as void and re-snapshots.
  Every `lease_epoch` value on the wire — the boot fence in the snapshot/ACK header AND the
  per-grant pls generation stamped on `job.*` events, witness lines, and `TALLY_LEASE_EPOCH` — is a
  point in **one** monotone series (PS#21: the pls lease generation, backstopped by the counter
  file). The daemon's boot bump floors at the shim's `pls-generation` counter and the shim's grant
  bump floors at the daemon's `epoch` counter, each writing strictly `max+1`, so any two values are
  totally ordered (fence comparisons `<` are always meaningful) and the field is never two
  independent counters mixed under one name.

`daemon_version` (binary semver) is informational only.

---

## 3. Integration notes

> Hostnames in this section (`harness-desktop` in examples and reach paths) are snapshots of
> today's dotfiles, resolved via the `conductorHost` option — never normative pins (SPEC "Module
> option surface"; boundary ruling 2026-07-09).

### 3.1 kitty (the out-of-band design law)

The load-bearing invariant (PS#15a): **tally never interposes on the pty byte stream.** Every read
is a side-channel poll of kitty's emulated grid via `kitty @ get-text`, throttled, off the keystroke
hot path. This is the direct analogue of cmux's own finding (`streaming-agent-updates.md`): the
correct source is the **terminal-emulator screen text**, not a raw-byte parse — interactive agents
paint with synchronized-output frames (`ESC[?2026h/l`), absolute-column moves (`ESC[<col>G`), and
bottom "live" regions that never linearly append, so recovering prose from bytes means re-running an
emulator. kitty already *is* that emulator; tally scrapes its grid.

tally's kitty surface, keyed on `kitty_window_id`:

- **`kitty @ get-text`** — the detector's (and `pane capture`'s) grid read. `--match id:<window_id>`
  scoping + extent flags map onto herdr's *grid* regions; the OSC regions ride `kitty @ ls`, never
  `get-text` (§3.3, corrected 2026-07-09).
- **`kitty @ focus-window --match id:<window_id>`** — the tunnel-in/focus affordance; focus keys on
  `kitty_window_id`, never on tab/split identity.
- **`kitty @ send-text`** (+ key escapes) — implements `pane send`/`send-key`/`agent send` via kitty
  internals; tally never re-grows a multiplexer to do it.

  > **Sidenote — the `claude -p` contingency (why send-text earns its keep).** `send-text` is not
  > only for interactive steering. Anthropic has (and may again) **externalize `claude -p` headless
  > runs from the subscription meter** — billing scheduled print-mode runs separately from the
  > interactive plan, a decision they made, rolled back, and could remake. If that flips, tally can
  > **mock `claude -p` scheduling with the interactive TUI**: launch `claude` in a kitty window, wait
  > ~10 s, and `kitty @ send-text` a short autonomous-mode kickoff (`@`-mentioning the system prompt,
  > the skill(s), and the plan `.md` files — e.g. *"today you're implementing @plan.md using @skill;
  > you're unsupervised in autonomous mode, so I won't be steering — this is the last message you get
  > from me"*). tally then keeps the TaskChampion rows / witness lines for that session exactly as for any
  > run. It is a contingency **built into the one binary — a flip away**, not a default path. Side
  > benefit: the scheduled run becomes a **recoverable zmx-backed session** (see SPEC "Recovering a
  > tally-owned agent session") instead of a headless print job.
- **kitty watchers** (`on_close`/`on_focus_change`/…) — the event edge that tells the daemon a window
  opened/closed/focus-changed, feeding the delta stream instead of polling for existence.
- **`kitty @ set-user-vars`** — **kept minimal: at most an opaque identity back-reference** (e.g. the
  pane's composite key), *not* a status mirror. Agent kind/status is a **delta-stream-only** fact so
  CUBS and the dotfiles picker read one source (no second store — see open flag §5).
- **ssh kitten terminfo path** — the remote reach (`kitten ssh harness-desktop -t zmx attach …`)
  that ships kitty's terminfo; the exact reason zmx was chosen over zmosh (zmx is ssh-kitten
  compatible; zmosh was antagonist). tally **assumes** this path; it does not own it.

**Not tally's:** `kitty @ launch` (window/pane creation). tally **observes** pane creation via
`pane.created` and binds the `kitty_window_id` it sees — niri/dotfiles own the kitty-tab-per-agent
projection (PS#13); autonomous jobs run headless (systemd oneshot, no terminal); interactive agents
run inside operator-opened zmx sessions. **One kitty terminal = one panel** — no `pane.split`,
`tab.*`, `new-split`, or `move-surface` behavior is adopted (only the verb *names* inform Seam B).

### 3.2 zmx (dotfiles-owned; tally reads, never manages)

Confirmed from `~/mecattaf/dotfiles/home/dot_config/fish/conf.d/remote.fish` +
`skills/remote-connection/README.md`:

**What tally does (read/observe only):**
- **Enumerate** existing sessions via `zmx list --short` (the same list feeding the dotfiles `desk-resume`
  fzf picker) to discover the session universe.
- **Key** the model: `persistence_session_id` = the zmx session id (a dotfiles timestamp name like
  `term-0707-1530`). Distinct from `session_ref` (the harness JSONL id) — never conflate.
- Run the daemon's supervised thread **inside** those already-persistent sessions and read their
  panes. A dispatched autonomous job and a supervised interactive session are the same object two
  ways on one stream.

**What tally must NOT do — the whole lifecycle is dotfiles territory:**
- **not** create sessions (`zmx attach <s> [fish]` / `zmx a <s>` create-or-attach; the `new-terminal`/`desk`
  flow) · **not** name them (the `term-MMDD-HHMMSS` scheme is the dotfiles' convention) · **not**
  reattach/roam (remote boxes use `kitten ssh harness-desktop -t zmx attach <s> fish`; resilience is
  Tailscale + reattach, no tally UDP layer) · **ships no receiver/persistence config** in the nix
  module (no `persistence.backend` option; the module surface is only `enable`/`role`/`conductorHost`/
  `sessions`).

Coordinator host = the **`conductorHost` machine** (the conductor; `harness-desktop` in today's
dotfiles snapshot — pure configuration, never a pin; boundary ruling 2026-07-09) — daemon + all
interactive kitty terminals live here; the headless worker runs models with no terminal/zmx session.
`loginctl enable-linger` keeps the daemons alive across logout.

### 3.3 The agent-state detector (the one genuinely-new piece)

An **in-daemon supervised thread** (restart-isolation, not a split binary; PS#15a) that classifies
`blocked/working/done/idle` and emits deltas on the same stream autonomous jobs feed. **Two
strategies, precedence-ordered**, and every agent record/event carries `detector:"hook"|"scrape"`:

**Strategy 1 — cooperative harness-hook state (AUTHORITATIVE where available).** Adopt cmux's
cooperative-hook *idea* + verb shapes only (engine rejected — see §4). Hook-exposing harnesses report
a lifecycle enum `{running, idle, needsInput, unknown}` → tally's vocabulary
(`running→working`, `idle→idle`, `needsInput→blocked`, `unknown→hold/scrape-fallback`). Turn
boundaries gate the scraper: `UserPromptSubmit` = turn start, `Stop` = turn end. Hooks also carry the
resume/session ref used by `recover()` (`pi --session <id>`, `claude --resume <id>`, `codex resume
<id>`). For **pi**, the mechanism is pi's **extension system** — auto-discovered from
`~/.pi/agent/extensions/` or `$PI_CODING_AGENT_DIR/extensions/` (cmux installs `cmux-session.ts`
there; tally installs its own analogue — the tally module ships the hook installer; §5 flag 2,
closed 2026-07-09).

**Strategy 2 — throttled out-of-band scrape (UNIVERSAL FALLBACK).** `kitty @ get-text` polls the
emulated grid, gated to active turns. Scrape rules use **herdr's per-harness TOML region+regex
state-manifest *format* as reference** (clean-room — never lift code). The manifest shape (from
herdr `pi.toml`/`claude.toml`): `[[rules]]` with `id`, target `state`, integer `priority` (highest
wins), a named `region`, match predicates (`contains`/`regex`/`line_regex`/`any`/`all`/`not`), and
flags `visible_working`/`visible_blocker`/`visible_idle`/`skip_state_update`. herdr's region vocabulary — `whole_recent`, `osc_title`, `osc_progress`,
`after_last_horizontal_rule`, `prompt_box_body`, `bottom_non_empty_lines(N)` — is the reference for
scoping a read, **split by mechanism (corrected 2026-07-09, deep-pass A1 — the prior wording implied
every region maps onto `get-text` extent flags, which is false for OSC data):** the *grid* regions
(`whole_recent`, `after_last_horizontal_rule`, `prompt_box_body`, `bottom_non_empty_lines(N)`)
scope a `kitty @ get-text` extract; the *OSC* regions (`osc_title`, `osc_progress`) bind to
`kitty @ ls` `foreground_processes[].title` + OSC progress escapes — never `get-text`. OSC-emitting
agents (Claude Code's braille spinner) MAY use the OSC regions as a zero-latency Strategy-2 fast
path checked before the grid scrape — a second, independent detection channel riding a verb family
tally already uses (`kitty @ ls`); no format, manifest, or protocol change.
Two herdr laws to inherit: **match invariant visible controls** with explicit AND/OR gates (never
incidental whole-pane text), and **never key off the user-scrollable viewport** (use bottom-buffer/
OSC regions).

**Scoping (anti-loop invariant #4).** The detector scopes to **agent panes only**. A pane running
`tally watch` is `is_viewer=true`, not in the manifest set — the detector never reads tally's own
output and self-loops. This constraint is enforced at *both* entry points: the autonomous detector
loop **and** the operator-facing `pane capture --source detection` / `session.wait pane_output`
(which refuse `is_viewer` panes). Detector logs are TTL-prunable (not permanent proof).

### 3.4 pi (agent-runtime binding + ⚠ stale-pin flag)

> **⚠ The vendored pin is stale.** `vendor/pi` = `badlogic/pi @ 812b1f41`, `v0.2.4`, dated 2025-08-05
> — a **GPU-pod / vLLM manager** (`README: "GPU Pod Manager"`, `package.json`: "managing vLLM
> deployments on GPU pods", ships `pod_setup.sh` + `vllm_manager.py`). This is **NOT** the
> agent-runtime pi tally binds to. **Re-pin `badlogic/pi` to a current agent-runtime HEAD before
> building the pi adapter** (target commit is an open flag, §5).

`pi` is one of the three `agent.kind` values `{pi, claude-code, shell}` dispatched through the one
enqueue verb. The agent-runtime surface tally binds to — evidenced by the herdr + cmux integrations,
not by the stale clone:
- **resume/recover:** `pi --session <id>` (herdr `session-state.mdx`; cmux session restore) — used by
  `recover()` re-present, carried as witness `trace_ref` / journald `TALLY_SESSION_REF`;
- **JSONL sessions** = the content plane tally only points at via `session_ref`, never copies;
- **cooperative hook** = the pi extension system at `~/.pi/agent/extensions/*.ts` /
  `$PI_CODING_AGENT_DIR` (Strategy-1 authoritative state for pi panes);
- **`--mode rpc` and `--skill`** — asserted by the tally spec (PS#18 pi-RPC `trace_ref` enrichment;
  PS#20 "deterministic workflows wrappable in a light pi executor") but **UNVERIFIED against any
  vendor source** (the only `--skill` in `vendor/` is cmux's unrelated skills installer). Treat as
  provisional; confirm against the re-pinned agent-runtime pi before relying on them.

*(OMP is a pi sibling — shares `$PI_CODING_AGENT_DIR`, resumes via `omp --resume=<id>`; not in the
day-1 `{pi,claude-code,shell}` set but confirms the extension/session lineage.)*

### 3.5 worktrees (a plain job field)

`cwd`/`worktree` is **one attribute** on the enqueue verb — it absorbs the entire
external-worktree-orchestrator family (cmux-craigsc, claude-squad, workmux, agtx, repowire) without
tally growing a worktree engine (OUV-MH R4). **cmux-craigsc is the reference only as "worktree = a
cwd field on the job"** — it is a plain bash git-worktree lifecycle manager
(`new/start/cd/ls/merge/rm/init`, no socket); tally takes the data-model insight, none of the CLI.
*(herdr's `worktree.{list,create,open,remove}` methods are herdr-engine surface, not adopted.)*

- **tally CAN:** dispatch N agents into N isolated worktrees, serialize their GPU use on one per-box
  pls lease (`PLS_CAPACITY=1`), witness each run (the `cwd`/`worktree` on the taskwarrior UDA +
  witness line), and stream all their states on the one delta stream. Worktree isolation is a
  cwd/branch policy, orthogonal to GPU metering.
- **tally CANNOT / WILL NOT:** decide the branching strategy (orchestrator LLM's call, opaque to
  tally), render their tabs (CUBS), or lay them out (niri).

---

## 4. Clean-room & license line

Two strong-copyleft sources were **read for interface, never lifted**:

- **herdr** — `github.com/ogulcancelik/herdr`, **AGPL-3.0-or-later**. Adopted: the socket verb
  *vocabulary* (`agent.list/get/read/explain/send/focus`, `pane.send_text/send_keys/read`,
  `session.snapshot`, `events.subscribe/wait`, `pane.agent_detected`/`pane.agent_status_changed`) and
  the per-harness **TOML region+regex manifest *format***. Rejected: the engine (its inline VT
  recompositor).
- **cmux-manaflow** — `github.com/manaflow-ai/cmux`, **GPL-3.0-or-later**. Adopted: the CLI verb
  *shapes* (`send`/`send-key`/`read-screen`, `events` NDJSON, `wait-for`, `ping`, `tree`) and the
  **cooperative harness-hook detection idea** (lifecycle + turn-gating). Rejected: the engine
  (Ghostty GUI + Swift + cloud backend).

**No herdr or cmux code is copied.** The no-code-lift discipline applies to **both** (both are strong
copyleft). Design *parameters* (the 4096 replay ring, 1024 slow-subscriber cutoff, 64 KiB frame cap)
are **tally's own**, set from tally's frame/memory budget — cmux's `events.jsonl` is a design
reference, not the source of the constants.

**Two deliberate divergences from herdr's grammar** (both intentional, both recorded here):
1. **No `agent.start`** — starting an agent *is* `tally enqueue` (Seam A).
2. **`events.*` folded into `session.*`** — tally groups the subscription/wait/ack/unsubscribe
   control methods under the `session.` noun (herdr groups them under `events.`), to match the
   verb-prefix convention.

**Lineage corrections applied** (from the adversarial pass): cmux-**craigsc** is a bash worktree tool,
**not** a socket source (the socket lineage is cmux-**manaflow**); the `agent.blocked`/`agent.done`
frames are tally-native over herdr's status *value* (herdr has no such events); the stale
`badlogic/pi` pin is the pod-manager, not the agent runtime.

---

## 5. Open flags (genuine human decisions — do not guess)

1. **kitty user-var schema.** Confirmed *conservative* here (opaque identity back-reference only;
   status is delta-stream-only). If a future need wants windows to self-describe status, that's a
   deliberate second-store decision to make explicitly.
2. **Who installs the cooperative hooks — CLOSED 2026-07-09.** The tally module ships its own
   pi/CC hook-extension installer (home-manager `programs.{claude-code,pi}.hooks` generated by the
   tally module). The harness hook is tally's Strategy-1 *authoritative detector input*, not a
   terminal-substrate concern — the dotfiles are the wrong owner. No new spec field. Three
   reference repos (worktrunk `wt config plugins claude install`, ghostex daemon-writes-hook,
   beads `bd setup claude`) independently converged on the installer-as-module pattern. This
   closes the question the skipped agentctl companion would have answered (see the 2026-07-09
   agentctl-skip entry in DECISIONS.md).
3. **The pi re-pin target.** The canonical upstream repo/branch/commit for the agent-runtime pi
   (`--session`/extension-system/`--mode rpc`) is not established (`badlogic/pi` HEAD is the stale
   pod-manager; `@mariozechner/pi` npm is the same lineage). Confirm before re-pinning `REFERENCES.md`.
4. **Scrape cadence for un-hooked panes.** For `agent.kind=shell` (no hook, no herdr TOML) or a
   pi/CC pane whose hook didn't install, there's no `UserPromptSubmit`/`Stop` to gate on — the
   polling policy (fixed interval? watcher-triggered only?) needs deciding.
5. **Are `shell` panes classified at all?** Or tracked purely by process/exit state with no
   `blocked/working/idle`?

**Deferred behind first-need (recorded 2026-07-09 — deferred, not open):**

- **`tally enqueue --ephemeral` (deep-pass A3).** Would set `labor_class=ephemeral` and suppress
  witness-line emission for non-proof-worthy micro-spans (probes, health checks, patrol-cycle
  sub-runs), excluded from canonical GPU-seconds; **the pls lease and the journald entry still
  fire** — the single-lease anti-OOM invariant is never suppressed. Rides the existing
  `labor_class` discriminator (additive enum value, zero schema migration). Adopt only if
  witness-JSONL noise from overnight micro-spans is actually observed — the durable-row admission
  test already keeps live-spawned sub-units out of the taskwarrior store.
- **`tally pane annotate` (`custom_status` + TTL — herdr-plugins Δ2).** A genuine additive verb,
  but its only consumer is a future CUBS tab-bar sub-label ("editing"/"searching") that does not
  yet exist. Adopt when that consumer is built (MECHANICAL when it comes: in-daemon TTL state,
  `custom_status` already in the schema, additive verb, no protocol bump).

---

## 6. What this unblocks

This freezes the contract every downstream step refers to:

- **BUILD-SEQUENCE step 1** (repo bootstrap) — `tally --help` exposes the §1 verb tree; the mock
  daemon speaks the §2 framing.
- **step 5** (witness/evidence) — `job.*` events (§2.3) *are* the journald `TALLY_EVENT` vocabulary;
  `enqueue --wait` (§1.1a) blocks on `job.completed`/`job.failed`.
- **step 6** (session model + detector) — §2.2 snapshot, the `agent.*`/`pane.*` events, and §3.3 are
  the acceptance shape; the dotfiles picker (#38 Q4) is a `session.subscribe{categories:["agent"]}`
  consumer.
- **step 10** (CUBS) — a pure Seam-B subscriber against §2, unblocked the moment this contract is
  frozen.

> **Frozen 2026-07-07.** Additive changes (new events, new optional fields, new verbs) are allowed
> without a protocol bump; breaking changes bump `protocol_version` (§2.5). The open flags in §5 are
> the only genuinely-undecided points and are scoped so they don't block build step 1.
