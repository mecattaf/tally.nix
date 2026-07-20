# tally — CLI-surface + kitty-maximalist session (handoff for a fresh Opus session)

> **Start a FRESH Claude Opus session for this.** Read ONLY this file + the three repo docs
> named below + `vendor/`. **Do NOT open `notes/projects/tally/`** — that morning-of-2026-07-07
> deliberation is CLOSED; re-reading it only bloats context and risks reopening settled rulings.
> This repo is authoritative; the notes are lineage.

## Where truth lives (the only things to read)

The four authoritative docs, then `vendor/`:

1. **`docs/DECISIONS.md`** — the canonical ruling ledger (what is decided; do not reopen).
2. **`docs/SPEC.md`** — the product spec (planes, seams, pools, witness, the tally/dotfiles boundary).
3. **`docs/BUILD-SEQUENCE.md`** — the ordered build plan. This session authors the artifact that
   unblocks the "thin CLI" + delta-stream acceptance shapes from **build step 1 onward**.
4. **`docs/REFERENCES.md`** — the reference-repo lineage: which `vendor/` clone carries which
   design, at what pin, and the AGPL clean-room line.
5. **`vendor/`** — the reference clones themselves, for source reading (pins in `REFERENCES.md`).
   Read for design; nothing is vendored into the build; AGPL code is **never** lifted.

Everything you need is in those four docs + `vendor/`. The notes repo (`notes/projects/tally/`) is
closed lineage — every decision was already lifted into `docs/`, so opening it teaches you nothing
new and only bloats context. If you feel the urge, don't.

## Standing decisions you inherit (do NOT reopen — see DECISIONS.md for the full set)

- **Persistence = `zmx`** (github.com/neurosnap/zmx) — libghostty-vt rehydration, KKP-aware,
  compatible with kitty's `ssh` kitten. It is **dotfiles-owned and assumed by tally** (tally runs
  *inside* existing zmx sessions; it does not create/name/attach them). Backend is DECIDED —
  this session is zmx **integration**, not a backend choice. (The earlier mmonad/zmosh pick was
  dropped: unmaintained + antagonist to the kitty ssh kitten. Do not resurrect it.)
- **Two seams only.** Seam A = one `enqueue` control verb `{priority, source, agent.kind ∈
  {pi,claude-code,shell}, invocation, cwd/worktree, evidence_spec}` with a blocking `--wait`
  (subsumes `wait_for_subagents`). Seam B = the delta stream (snapshot + newline-JSON events +
  control) carrying `{session,pane→agent{kind,status}}`.
- **tally owns only** the minimal kitty-native binding + the delta stream. No multiplexer, no UI,
  no workflow-DAG interpreter, no team paradigm. herdr/cmux **engines are rejected**; their
  **socket verb vocabulary + agent-state heuristics are adopted clean-room**.
- **Boundary:** the terminal substrate (zmx receiver-everywhere, session create/name/reattach, the
  fzf pickers), niri layout, kitty-tab-per-agent projection, and `mod+enter` are **dotfiles-owned**.
  tally never re-ships them. (The dotfiles session picker is a delta-stream *consumer* of tally's
  agent-state detector — one detector, many readers.)
- Governor = pls (one broker per box); GPU=2, worker-prioritized; OCR is a first-class job.

## Charter — decide, from source, and freeze:

1. **The `tally` CLI verb set** (a thin socket client on the one binary): the merged queue verbs
   (`enqueue/cancel/pause/resume`), session verbs (`new/list/attach/detach/watch/send/send-key/
   focus/capture`), and query verbs (`log/status/render --format/+standup`) — exact names, flags,
   JSON output shapes, and the verb-prefix convention. Seam A's `enqueue --wait` is the load-bearing one.
2. **The delta-stream wire schema** (Seam B): the snapshot shape, the full delta-event enumeration
   (`agent.status_changed`, `done`, `blocked`, `pane.*`, `session.snapshot`, …), the control verbs,
   and protocol versioning. This is the highest-leverage first artifact — freeze it contract-first.
3. **kitty-maximalist integration** (`vendor/kitty`): the remote-control protocol (`kitty @
   get-text/launch/focus-window`), watchers, `set-user-vars` for status, the `ssh` kitten terminfo
   path. Nail how tally gets awareness **out-of-band and never interposes on the stream** — the
   design law that makes the un-attached live-state detector work. Note Tom's constraint: one kitty
   terminal = a single panel (no splits/tabs/multiplex per terminal); niri owns workspaces;
   `pane.send-text` uses kitty internals.
4. **zmx integration** (`vendor/zmx`, `vendor/ghostty`): how tally reads/attaches the **existing**
   dotfiles zmx sessions (it does not create them), the libghostty-vt/KKP behaviour across reattach,
   and the session→pane data model keyed on `(persistence_session_id, kitty_window_id)`.
5. **herdr clean-room harvest** (`vendor/herdr`, AGPL — interface only, never lift code): the socket
   verb vocabulary (`agent.start/list/get/send/focus/read`, `pane.agent_status_changed/agent_detected`)
   as the Seam-B grammar, and the per-harness TOML region+regex state manifests as the design
   reference for the in-daemon `kitty @ get-text` detector. **cmux disambiguation:** `vendor/cmux-manaflow`
   (Ghostty GUI + cloud backend — engine rejected) vs `vendor/cmux-craigsc` ("tmux for Claude Code",
   socket/CLI) — determine which carries the socket-spec lineage; harvest interface, reject engine.
   Adopt cmux's cooperative harness-hook detection as the second strategy where a harness offers a hook.

## Method (ultracode workflow)

Parallel source-readers (herdr · cmux×2 · zmx · kitty · pi) → cross-repo synthesis of the CLI verb
set + the socket/delta-stream wire contract → an adversarial pass against the kitty-maximalist
design law (out-of-band only) and the AGPL clean-room line → write **`docs/CLI-SURFACE.md`**.

## Verify-first flags

- `vendor/zmx` = `neurosnap/zmx` @ `6fabec0` — **THE backend.** zmosh (mmonad) is dropped; do not resurrect.
- cmux ambiguity (above) — resolve before harvesting.
- `vendor/pi` = `badlogic/pi` @ `812b1f41` (HEAD dated 2025-08) — confirm the active repo/branch first.

## Output

`docs/CLI-SURFACE.md` — the frozen CLI verb set + delta-stream wire schema. This is the contract
every subsequent build step's "thin CLI" / "delta stream" acceptance shape refers to; authoring it
unblocks implementation (build step 1 onward).
