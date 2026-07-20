# tally

> [!IMPORTANT]
> **This repo is authoritative. Build from the four `docs/` files below — do not read the notes repo.**
> `notes/projects/tally/` is **closed deliberation lineage**: every decision was lifted into
> `docs/` (chiefly `DECISIONS.md`), so *no decision lives only in the notes*. Reading them cannot
> teach you anything the docs don't — it only bloats context and risks reopening settled rulings.
> If a fresh session feels the urge to open the notes: **don't.**

> A local-first, account-less, single-operator daemon that drains a work queue to the
> contended GPU (one per box), serializes and meters it (GPU-seconds), and emits a
> git-independent, evidence-gated **witness ledger** — proof of *labor* anchored to
> task-UUID + artifact-hash + exit-code. An offline CLI, bridged (not backed) by GitHub.

**Status:** crystallized spec (2026-07-07), pre-build. Work from the four docs below.

The four authoritative docs:

- [`docs/DECISIONS.md`](docs/DECISIONS.md) — the canonical ruling ledger.
- [`docs/SPEC.md`](docs/SPEC.md) — the product spec (planes, seams, pools, witness, boundary).
- [`docs/BUILD-SEQUENCE.md`](docs/BUILD-SEQUENCE.md) — the ordered, gated build plan.
- [`docs/REFERENCES.md`](docs/REFERENCES.md) — the reference-repo lineage (pins for `vendor/`).

Plus the frozen CLI/wire contract (authored 2026-07-07 from `vendor/` + the dotfiles zmx integration):

- [`docs/CLI-SURFACE.md`](docs/CLI-SURFACE.md) — the frozen `tally` CLI verb set, the Seam-B
  delta-stream wire schema, and the kitty/zmx/pi integration notes. Unblocks build step 1 onward.

And the session handoff that chartered it (self-contained; reads only the four docs + `vendor/`):

- [`docs/NEXT-SESSION-CLI-SURFACE.md`](docs/NEXT-SESSION-CLI-SURFACE.md) — charter (now fulfilled by `CLI-SURFACE.md`).

This repo is the buildable product; the dotfiles consume it as a flake like any other input.

## What it is (the six properties, held at once)

The defining bet — from the 2026-06-23 survey verdict *"the intersection does not exist"* —
is the conjunction of all six:

1. **local-authoritative over the Unix PIM substrate** (taskwarrior/TaskChampion) —
   not Postgres/ClickHouse.
2. **owns the full lifecycle** — queue → serialize → meter → witness — not observe-only
   (Langfuse et al.) and not durability-only (Temporal et al.).
3. **GPU-seconds as the meter** — not API-dollars, not wall-clock.
4. **a git-independent labor witness** anchored to `task-UUID + artifact-hash + exit-code`
   — not code-provenance, not a trace, not a crypto-receipt SaaS.
5. **evidence-gated** — gates on the output *existing*, not the agent's self-report.
6. **a single-operator point solution / flake** — no account, no backend, no multi-tenant.

## The one law

**tally tracks *contention* and *proof* — never *content* or *control*.** Three records at
three altitudes meet only at `session_ref` and the one GPU lease: the harness JSONL is
*content* (tally points at it, never copies it), taskwarrior is the *work-item*, the witness
ledger is the *proof*. Orchestration logic and rendering are consumers, not things tally
absorbs.

## Two execution modes, one daemon

- **Autonomous / batch** — systemd drains the queue → headless worker → journald + witness.
  No terminal, no attach. (The academic-paper OCR drain is exactly this — a first-class job,
  not a special case.)
- **Interactive / supervised** (the "kerdr" face) — sessions the human sits with, attaches to,
  and detaches from across devices; **un-attached live state** (blocked / working / done) is
  streamed out-of-band to consumers (the CUBS tab-bar, notifiers). A dispatched job and a
  supervised session are the *same object* seen from two sides.

**Invariant:** every session is always kitty-**zmx** tunneled, whether the agent runs via
`pi` or `cc`. tally targets **kitty** as the sole terminal emulator. Persistence is
[zmx](https://github.com/neurosnap/zmx) (libghostty-vt rehydration, KKP-aware, and compatible
with kitty's `ssh` kitten) — **provided by the dotfiles, assumed by tally**, not shipped here.

**Conductor-receiver architecture:** all interactive kitty terminals run on the powerful
conductor and are uniform *receivers* for the terminal buffer (even on the conductor itself);
the tailscale-connected zenbook-duo reattaches conductor sessions over kitten-ssh/Tailscale.
`mod+enter` is the local-terminal escape hatch. (This substrate lives in the dotfiles; tally
runs inside it.)

## What tally is / isn't

tally is **not** a multiplexer (niri owns layout; one kitty terminal = one panel, no
tabs/splits), **not** an orchestrator (it interprets no workflow DAG; every harness is a
client of one `enqueue` verb), **not** a model-tier router (model class is chosen at
ignition), **not** a second source of truth, **not** a reach layer (it's an offline CLI —
how keypresses arrive is not its problem; Tailscale is merely the default reach), and **not**
the owner of the kitty-tab-per-agent projection (that belongs to the nix desktop / dotfiles).

**Worktrees (what it can and can't do).** tally absorbs the external-worktree-orchestrator
family (cmux, claude-squad, workmux, agtx, …) as a plain `cwd`/worktree **field** on the job.
It **can** dispatch N agents into N isolated git worktrees, serialize their GPU use on one
per-box lease, witness each run, and stream their state. It **cannot** and will not decide the
branching strategy, render their tabs, or lay them out — those belong to the orchestrator LLM,
CUBS, and niri respectively.

## Host substrate the dotfiles must provide (for other users)

tally **assumes** the terminal substrate rather than shipping it (see the tally/dotfiles boundary in
`SPEC.md`). If you are adopting tally outside Tom's dotfiles, your kitty + persistence layer must
provide two things, or the detector and every `pane`/`agent`/`session` verb are inert:

1. **kitty remote control enabled**, with a per-instance listen socket and window watchers — this is
   the entire surface tally binds to (out-of-band; tally never interposes on the pty). In `kitty.conf`:

   ```conf
   # tally reaches kitty ONLY through remote control — enable it + a listen socket
   allow_remote_control  yes          # or: socket-only
   listen_on             unix:${KITTY_RUNTIME_DIR}/kitty.sock   # -> exported as KITTY_LISTEN_ON
   # window lifecycle edges the daemon consumes (open/close/focus/cmd-start-stop/title/user-var)
   watcher               /path/to/tally-watcher.py
   ```

   tally uses `kitty @ ls` (inventory: window id, cwd, cmdline, foreground process, user-vars, focus),
   `kitty @ get-text` (the detector's grid scrape), `kitty @ send-text` (steering / prompt injection),
   `kitty @ focus-window` (tunnel-in), and the watcher events above. It uses **no** kitty
   splits/tabs/OS-windows/layout — one kitty terminal is one panel; your WM owns layout.

2. **zmx persistence + the kitty `ssh` kitten reach**, so every terminal is a recoverable
   `zmx` session (`zmx attach`/`zmx list`) reachable over `kitten ssh`. tally runs *inside* those
   sessions and reads them; it does not create, name, or reattach them.

## Layout

- `docs/` — the authoritative spec (above) + `REFERENCES.md` (reference-repo lineage).
- `vendor/` — reference repos cloned for source analysis (gitignored; see `docs/REFERENCES.md`).

## Conceptual backbone

**TTAL** (taskwarrior-hooks drive the agent loop) · **sasori** (the generative negative —
"sasori is not enough" named the queue/drain gap) · **pls** (the GPU-lease primitive).
Full lineage in `docs/REFERENCES.md`.
