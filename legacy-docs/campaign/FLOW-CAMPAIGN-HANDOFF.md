# FLOW-CAMPAIGN-HANDOFF — overnight mission, 2026-07-24

You are the campaign orchestrator (Claude, in Claude Code, on the coordinator). Codex
writes all implementation code; tally dispatches and supervises it; you enqueue, verify,
merge, and keep state. You never write implementation code yourself. This file is your
complete mission; the oracle session that wrote it has ended. Tom is asleep — there is no
human in the loop until morning.

RESUME CHECK (first act, every session): if `FLOW-CAMPAIGN-STATE.md` exists at the repo
root, read it plus `git log --oneline -15`, `git status`, `gh issue list --state open`,
and `tally query pools`, then continue from the recorded frontier. Assume no
conversational context survived. If it does not exist, create it now with the lane table
below and begin.

## 1. What is already true

- `main = 04a6149` — the flow-era spec corpus is merged: `docs/FLOW-SPEC.md` (normative),
  `docs/NIX-SPEC-FLOW.md`, `docs/FLOW-GAP.md`, `docs/FLOW-BUILD-SEQUENCE.md`,
  `docs/transfer/` (9 briefs). The spec is FROZEN: codex implements it, nobody redesigns
  it. No open PRs at handoff time.
- Issues, filed and dispatch-ready:
  `#44` FS-1 · `#45` FS-2 · `#46` FS-3 · `#47` FS-4 · `#48` FS-5 · `#49` FS-6 ·
  `#50` FS-7 · `#51` CP-A · `#52` CP-B.
- The deployed daemon predates today's merges — that is fine; it dispatches exactly as it
  did for waves 1–5. Checkpoints run their own isolated dev daemons on fresh builds.
  Do NOT deploy the fleet overnight; deployment is a morning recommendation in CP-B's
  report.
- Feature-completeness law: there is no MVP tier anywhere in this campaign. A unit is
  done when it implements its spec sections to the last exception path. "Poorly written
  but complete" is revisitable; "incomplete" is a failed wave.

## 2. Lanes and order

Two concurrent implementer lanes, each an exclusive capacity-1 lease:

| Lane | Pool | Sequence |
|---|---|---|
| A | `build` | FS-1 (#44) → FS-2 (#45) → FS-3 (#46) → FS-6 (#49) |
| B | `coordinator-gpu` (repurposed as a second implementer mutex; GPU idle overnight) | FS-4 (#47) → FS-5 (#48) → FS-7 (#50) |

- Dispatch FS-1 and FS-4 immediately, in parallel.
- FS-5 requires FS-1, FS-3, AND FS-4 merged (rebase its worktree on fresh main) — the
  runner is normatively single-connection-multiplexed, so it needs FS-3's concurrent
  serving in the build it tests against.
- CP-A (#51) runs after FS-5 merges (transitively FS-1/FS-3/FS-4); it may overlap
  lane A's FS-6.
- The 2026-07-24 ambiguity sweep's 28 binding resolutions are already spliced into the
  three docs (commit after 04a6149) — the spec codex reads contains every answer; there
  are no known open questions. A BLOCKED report should therefore be rare and treated as
  news: record it under PENDING-TOM.
- CP-B (#52) runs last, after everything merges. Red checkpoints ⇒ fix session in the
  offending lane (dedup key `<unit>-fix1`) before any further merges.
- Lane B's heavy nix builds should offload to the worker where the existing distributed
  build setup allows; don't fight cargo contention beyond that.

## 3. Dispatch runbook (the proven wave-1..5 pattern)

```bash
git -C ~/mecattaf/tally.nix pull --ff-only
git -C ~/mecattaf/tally.nix worktree add ~/mecattaf/tally.nix-fsN -b fs-N-<slug> origin/main
# checkpoints: use --detach instead of a branch

tally enqueue --pool <lane-pool> --source orchestrator --priority high \
  --runtime-max-sec <cap> --dedup-key <key> --wait -- \
  codex exec --json --dangerously-bypass-approvals-and-sandbox \
  -C /home/tom/mecattaf/tally.nix-fsN -- "<prompt>"
```

Run the enqueue as a background shell so you keep orchestrating while it runs.

- Runtime caps: FS-4 21600; all other units 14400; checkpoints 14400.
- Dedup keys (fresh — never reuse today's wave keys): `fs-1-attach`,
  `fs-2-provenance-brief`, `fs-3-concurrent-wire`, `fs-4-flow-crate`,
  `fs-5-replay-integration`, `fs-6-semantic-truth`, `fs-7-nix-flows`, `cp-a-flow-live`,
  `cp-b-seal`; fix sessions append `-fix1`, `-fix2`.
- Prompt skeleton (held 8/8 today): *authoritative instructions = issue #N — read it with
  `gh issue view N --repo mecattaf/tally.nix`, then read the spec sections and transfer
  briefs it names; work only in this worktree; commit locally; HARD LIMITS: no push, no
  PR, no GitHub mutation (checkpoints: exactly one report comment on their own issue);
  run the full gate ladder and paste real outputs; feature-complete per spec — no stubs,
  no TODOs, no deferred fragments; ambiguity ⇒ STOP and report BLOCKED with the precise
  question and spec citation, never invented semantics.*
- Checkpoint dispatches that drive a dev daemon must sanitize inherited env:
  `env -u TALLY_JOB_ID -u TALLY_SOCKET …` (cp2's `unknown parent job` finding).
- Codex final reports are recoverable from `~/.local/state/tally/capture/<task-uuid>.out`
  via `jq -r 'select(.item.type=="agent_message") | .item.text'`.

## 4. Post-session ritual (per unit)

1. Re-run the full gate ladder yourself in the worktree: `nix develop -c cargo test
   --workspace`; clippy `-D warnings`; `nix flake check`; witness regression
   (valid GREEN / tampered RED); no-stubs grep (`todo!|unimplemented!|TODO` in crates/).
2. Adversarial pass: diff vs the issue's acceptance list; missing behavior or tests that
   don't prove their claim ⇒ fix session, not a merge.
3. Push branch; `gh pr create` with `Closes #N`. **Known incident**: GitHub's PR-create
   endpoint was returning empty 500s at 19:45Z while issues/reads worked. If it still
   500s after two retries: fast-forward merge to main directly from the main checkout
   (the spec corpus landed this way), push, and post the would-be PR body as a comment on
   the issue instead. Never let the PR service block the campaign.
4. Merge from the main checkout (never from inside a worktree), pull, remove worktree,
   delete branch, update `FLOW-CAMPAIGN-STATE.md`, dispatch the lane's next unit on
   fresh main.

## 5. Laws (verbatim from the house canon — they bind you and every codex session)

- **Honesty**: no claimed green without the exact command and pasted output. BLOCKED with
  a precise spec citation beats invented semantics, always. If a codex session reports
  BLOCKED and the spec genuinely doesn't answer, record the question in
  FLOW-CAMPAIGN-STATE.md under "PENDING-TOM", skip-and-continue if other work is
  unblocked, and surface it prominently in the morning report.
- **Spec supremacy**: FLOW-SPEC/NIX-SPEC-FLOW are frozen. You may not relax an acceptance
  bullet, and neither may codex.
- **Compat**: additive-only; witness byte-compat regressions are a stop-everything
  failure, same as a witness-verify flip.
- **Scope fence**: #33/#34/#35 (conformance shells) are NOT tonight's work. Do not
  dispatch them.

## 6. Stop condition and morning report

The campaign ends when CP-B's report comment is posted (or when both lanes are hard
blocked). Final act either way: write `MORNING-REPORT.md` at the repo root — campaign
table (unit → PR/commit → result), checkpoint outcomes, every PENDING-TOM question,
accrued ORACLE-DELTAS obligations, the deployment recommendation (post-campaign main →
fleet via dotfiles flake bump — Tom's act, not yours), and the first three commands for
Tom to see tally-flow alive. Keep `FLOW-CAMPAIGN-STATE.md` truthful at every step; it is
the only memory the next session has.
