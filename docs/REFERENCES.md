# tally — reference repos

Two sets: (A) repos **cloned into `vendor/`** in this repo for the terminal/session-lineage
source analysis (the next deep-dive session), and (B) the broader inspiration corpus already
analyzed and pinned in the notes repo. `vendor/` is gitignored — these are read for design,
not vendored into the build.

Full idea-lineage (what tally drew from each, and where it's recorded) lives in the notes
repo: `projects/tally/INSPIRATION-REPOS.md`.

## A. Cloned into `vendor/` — session/terminal lineage (pins as of 2026-07-06, shallow)

| Dir | Repo | Pin | Why it's here |
|---|---|---|---|
| `herdr` | github.com/ogulcancelik/herdr | `5b4450c9` (2026-07-07) | The agent-state-detector + socket-API lineage (kerdr's origin harvest); vendors libghostty-vt. Interface adopted, engine (inline VT recompositor) rejected. AGPL-3.0 — clean-room only, never lift code. |
| `cmux-manaflow` | github.com/manaflow-ai/cmux | `adc48877` (2026-07-06) | **cmux disambiguated (2026-07-07): this is the socket/CLI + cooperative agent-hook lineage.** Ghostty GUI + Swift CLI + cloud backend — **engine rejected**. Harvested clean-room: the CLI verb shapes (`ping`, `notify`, `tree`, `send`/`send-key`, `read-screen`, `events` NDJSON, `rpc`, `agent …`) and the per-harness cooperative HOOK detection (`~/.<agent>/…` hooks; lifecycle `running\|idle\|needsInput\|unknown`; `UserPromptSubmit`→turn-start / `Stop`→turn-end active-turn gating). `CMUX_*` pane env → tally's pane-context env. See `docs/CLI-SURFACE.md`. |
| `cmux-craigsc` | github.com/craigsc/cmux | `864d41d4` (2026-06-16) | **cmux disambiguated (2026-07-07): NOT a socket source.** A plain bash git-worktree lifecycle manager ("tmux for Claude Code" — `new/start/cd/ls/merge/rm/init`, no socket, no daemon). It is the **worktree-field reference** (R4: the external-worktree-orchestrator family collapses to a `cwd`/worktree attribute on the job), cited in SPEC's worktree capability statement. The socket-spec lineage is `cmux-manaflow` above. |
| `zmx` | github.com/neurosnap/zmx | `6fabec06` (2026-06-28) | **THE persistence backend (sole, 2026-07-07).** libghostty-vt rehydration (KKP-aware across reattach), actively maintained, and — decisively — compatible with kitty's `ssh` kitten (the terminfo/remote path tally + the dotfiles rely on). Persists one pty/shell per handle; provides NO windows/tabs/splits (niri/the WM owns layout). Network-change resilience is delegated to Tailscale + reattach, not a bespoke UDP layer. |
| ~~`zmosh`~~ | github.com/mmonad/zmosh | — | **DROPPED 2026-07-07.** Briefly adopted (an encrypted-UDP-roaming `zmx` fork) but turned out **unmaintained** and **antagonist to the kitty `ssh` kitten** — the flip conditions bit fast. Superseded by upstream neurosnap/zmx. |
| ~~`shpool`~~ | github.com/shell-pool/shpool | — | **RETIRED (PS#12).** Superseded by zmx; removed from `vendor/`. Known-risk `shpool_vt100` rewrite + zero KKP-across-reattach awareness were the flip conditions. |
| ~~`cataggar/zmosh1`~~ | github.com/cataggar/zmosh1 | — | **DROPPED (mis-identity).** Was `zmx` v0.5.0 on Zig 0.16 — a stale fork, not upstream; the canonical tool is neurosnap/zmx above. |
| `pi` | github.com/badlogic/pi | `812b1f41` (2025-08-05) | The agent runtime tally delegates to (`--mode rpc`, JSONL sessions, `pi --session <id>` resume, `~/.pi/agent/extensions/*.ts` cooperative hooks, `--skill`). **⚠ STALE PIN (resolved 2026-07-07): the cloned commit is the OLD vLLM-pod-manager era** (`v0.2.4`; `pi setup`, `vllm_manager.py`, `pod_setup.sh` — a GPU-pod/model-serving CLI, NOT the agent runtime). The agent-runtime `pi` surface tally binds to is evidenced by the herdr + cmux-manaflow integrations (`pi --session`, `$PI_CODING_AGENT_DIR`, extension hooks). **Re-pin `badlogic/pi` to a current HEAD before building the pi adapter** — bind to the interface, not this clone. |
| `kitty` | github.com/kovidgoyal/kitty | `cc95fccc` (2026-07-07) | The sole terminal emulator tally targets. Remote-control protocol (`kitty @`), watchers, user-vars, ssh kitten — the kitty-maximalist surface. |
| `ghostty` | github.com/ghostty-org/ghostty | `cabbdee3` (2026-07-06) | libghostty-vt — the VT engine consumed transitively via zmx; reference for reattach-repaint behavior. |
| `wmux` | github.com/amirlehmam/wmux-orchestrator | *(not yet cloned)* | A simple cmux clone — the CLEANEST distillation of the socket-verb surface tally's thin CLI mirrors: `wmux ping`/`notify`/`new-workspace`/`list-workspaces`/`split`/`send`/`send-key`/`read-screen`/`tree`, plus `agent spawn`/`spawn-batch`/`list`/`status`/`kill` and a CDP-powered `browser` subcommand (`open`/`snapshot`/`click @eN`/`type`/`screenshot`/`eval`). Confirms the cmux-manaflow verb grammar as a convergent design; added as a verb-surface reference (interface only). **tally's will be better** — tally is the queue/meter/witness/detector underneath, not another multiplexer. The `browser`/`agent spawn-batch` verbs are OUT of tally's scope (control/view plane). |

## B. Analyzed + embedded in the notes repo (`references/devlogs/1h26/june22-repos/`, pins in REPO-PINS.md)

- `pls` — github.com/sniarchos/pls @ `31d5040` — **the GPU-lease primitive** (non-negotiable substrate; RAII/drop-released).
- `sasori` — github.com/kyuz0/sasori @ `302dc2d` — the generative negative ("sasori is not enough").
- `agent-trace` — github.com/cursor/agent-trace @ `2754f07` — what the witness is NOT (git-coupled code-provenance). **DROPPED 2026-07-09; filed as a WATCHED reference.** Code-generation-specific by construction (files → conversations → line-ranges); commit-granularity attribution is already native to tally (witness × git log × `session_ref`). Kept from it: the models.dev `provider/model-name` convention for the witness model field. Spec is RFC-status v0.1.0 (jan-2026); the reference impl is demonstration-grade (338 lines of dependency-free Bun/TS, no tests, spec and impl disagree on their own version string, `content_hash` declared but never computed). Full eval: notes `july26-fable-second/tally-shape/agent-trace-reference-eval.md`.
- `models.dev` — https://models.dev — not a repo pin: the `provider/model-name` model-id
  convention the witness `model` field adopts (jul9; the one thing kept from the agent-trace
  episode).
- `dotfiles #38` — github.com/mecattaf/dotfiles/issues/38 — **WATCHED (2026-07-09 boundary ruling).**
  The canonical write-down of the zmx session-titling service (qwen3-0.6b via fastflowlm, session
  titling from `zmx history`) — dotfiles-side, **permanently out of tally's scope**; tally's only
  relation is the already-specced one (the picker consuming tally's delta stream, #38 Q4).
- `dotfiles #40` — github.com/mecattaf/dotfiles/issues/40 — **WATCHED (2026-07-09 boundary ruling).**
  The enabling infra for #38: nix-strix-halo (fastflowlm/flm XDNA2 NPU server, ds4) + split NPU
  boot. Nothing tally-shaped; sequences the titling service, never tally.
- `cli` (gws) — github.com/googleworkspace/cli @ `a3768d0` — the membrane: vendor CLIs, not open-protocol clients.
- `mcp-taskwarrior-ai` — github.com/storypixel/mcp-taskwarrior-ai @ `2668503`.
- `dankcalendar` — github.com/AvengeMedia/dankcalendar @ `6b5e059`.
- `gpu-service-manager` — github.com/nashspence/gpu-service-manager @ `46c1bb3`.
- `claude-orchestrator` — github.com/ImMathanR/claude-orchestrator @ `041e8f2`.

## C. Conceptual backbone (per Tom)

**TTAL** (taskwarrior-hooks drive agents — the near-twin spine; a DEV Community write-up,
may not be a clonable repo) · **sasori** (§B) · **pls** (§B). These three anchor the three
axes: dispatch spine, generative negative, GPU-lease primitive.

## D. Nix-shape exemplars (patterns imitated, not dependencies)

microvm.nix (flake-STRUCTURE only) · flake-parts · niri-flake · process-compose-flake ·
pi.nix · llm-agents.nix. URLs in the notes `INSPIRATION-REPOS.md`.
