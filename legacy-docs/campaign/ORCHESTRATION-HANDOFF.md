# Orchestration handoff — sequenced issue run, paused at checkpoint 2

Author: Claude (meta-orchestrator), 2026-07-24, end of session.
Companion doc: the orchestrator field report (`ORCHESTRATOR-FEEDBACK.md`, wherever you moved it) — that one is *opinions about the product*; this one is *state of the campaign and the concrete path to checkpoint 3*.

---

## 1. Where the run stands

| Step | Issue | Result | PR | Closed |
|---|---|---|---|---|
| Wave 1 — origin identity + GH safety | #26 | pass, gates green | #36 | #19 #20 #22 #26 |
| Wave 2 — scoped triggers + producer CLI | #27 | pass, gates green | #37 | #21 #27 |
| Wave 3 — launch ergonomics + semantic completion | #28 | pass, gates green | #38 | #28 |
| Checkpoint 1 — GH dispatch scenario, live | #29 | RED → fixed → GREEN | #40 | #29 #25 |
| Wave 4 — durable history, jobs/job/proof | #30 | pass, gates green | #41 | #30 |
| Wave 5 — trace, producers, pagination, watch | #31 | pass, gates green | #42 | #24 #31 |
| Checkpoint 2 — observability contract, live | #32 | **GREEN first try, all 6 assertions** | — | #32 |

Every wave was implemented by a tally-dispatched codex session (witness seqs 57–64 on the production ledger), verified independently by me (full gate ladder re-run per wave), squash-merged. `main` = `1ff5f3d`. Worktrees and branches all cleaned up; no open PRs.

**Still open:** #14 (BS-13 oracle harness), #15 (BS-14 fleet conformance), #33/#34/#35 (the wave-6/wave-7/checkpoint-3 orchestration shells, filed and ready, not dispatched — paused on your instruction).

Checkpoint 1 caught two real defects that 500+ green unit tests missed (canonical `pass` on failed required gate; missing `ghOrigin` in `query status`). Checkpoint 2 caught zero — the #24 contract text was precise enough to implement blind. Both reports live as comments on #29/#32 with exact command transcripts.

Product-level learnings live in the field report and the two checkpoint transcripts (#29, #32) — not repeated here. The one operational note that affects future dispatches: a tally job that drives another tally instance must sanitize its inherited `TALLY_*` env (`env -u …`) or the child daemon fail-closes admission with `unknown parent job` (correct, but surprising).

---

## 2. The path to checkpoint 3 — comprehensive plan

The three ON-HOLD steps are fully specified in #33, #34, #35. Here is what each actually involves, what will happen when dispatched, where the risks are, and how your reshape most plausibly slots in.

### 2.1 Wave 6 — BS-13 golden-oracle harness (#33, closes #14)

**What it is.** A diff rig proving the Rust implementation byte-identical to the archived Bun prototype on the *surviving frozen surface only*: NDJSON wire frames + witness JSONL. Ports `dev/mock/{fake-worker.sh,pls.sh,enqueue-samples,events-samples}` and the seven surviving e2e fixtures (`barrier`, `cli-surface`, `ocr-drain`, `recover`, `witness-verify`, `handlers-wire`, `evidence-fail`) from the `archive-bun` branch (`c36616a` — the old repo is deleted; this branch is the only oracle; bun comes from nixpkgs in the harness devshell).

**Classification law (your ruling, baked into #33):** every observed diff is exactly one of (a) *intentional contract change*, recorded in a `docs/ORACLE-DELTAS.md` table with justification and a pointer to where the new behavior is tested, or (b) *Rust regression*, fixed in the wave. Unclassified diff = wave failure. Surface added by waves 1–5 (origin schema, gate manifests, protocol-2/3 query) is out of the rig's scope by definition — the rig governs only what the Bun oracle ever spoke.

**Expected friction, honestly:** waves 1–5 touched serialization-adjacent code (`wire.rs`, witness completion facts, verdict path). The witness regression stayed green all day and the cp1 fix explicitly preserved manifest-free byte-identity, so I expect the *dominant* test to diff clean — but the `handlers-wire` and `cli-surface` fixtures may surface small intentional deltas (e.g., new optional fields in frames). That's precisely what ORACLE-DELTAS is for; expect a handful of entries, not zero.

**Sequencing argument — do this BEFORE your major improvements.** The harness's entire value is anchoring the frozen surface while it is still frozen. If the improvements land first and intentionally reshape the surface, every fixture diffs dirty and the classification table becomes an archaeology project. Running wave 6 now costs one codex session and gives your reshape a *certified baseline*: after it, any diff the harness reports during your improvements is by construction an intentional change you're making, pre-sorted from regressions. If you only take one of the two conformance waves before reshaping, take this one.

### 2.2 Wave 7 — BS-14 remaining fleet conformance (#34, closes #15)

**Already landed (do-not-redo, verified still green as part of the wave):** fan-out guardrails, slow-sqlite rebuild, pool-vanished/pool-return — shipped in the original build; checkpoint 2 incidentally re-exercised pool-vanished/recovered live.

**Remaining four scenarios:**
1. *Network blip vs true vanish* — hysteresis discrimination. Surface exists (pool-reachability producer + health events); expect implementable.
2. *Coordinator switch mid-lease* — remote re-adoption via bumped epoch. Surface is the fail-closed remote executors from PR #23; partially exists. This is the scenario most likely to surface real design gaps — treat a BLOCKED here as *valuable output*, not failure.
3. *Cooperative-yield timing* — low holder yields within `yieldGraceSec` → `preempted`. Surface exists (yield channel from BS-4); expect implementable.
4. *Remote-dmem capability downgrade + worker servingSlice stamp* — **most likely BLOCKED**: dmem enforcement was deliberately deferred out of the original build (`enforce = cooperative` only) and PR #23 did not add it. The fail-closed rule in #34 applies: record `BLOCKED(<scenario>): <exact missing surface>`, spin off a precise issue, never stub.

**Interaction with your reshape:** if the "major improvements" include the dmem/remote surface, you may prefer to fold scenario 4 (and possibly 2) into that work and let wave 7 cover only 1+3 — in that case, edit #34's scope line before dispatching and let #15 close against the narrowed set + spin-offs. The issue text already anticipates exactly this via the fail-closed rule, so the reshape is a one-paragraph edit, not a rewrite.

### 2.3 Checkpoint 3 — final gate (#35)

Four assertion groups, all coordinator-local (worker-tb = nix build offload only):
1. Full BS-13 harness run from a clean checkout — every fixture clean or classified.
2. Full BS-14 suite (original three + wave-7 additions) — every scenario green, BLOCKED entries matching spun-off issues exactly.
3. Full gate ladder on final `main`.
4. The "everything composes" smoke: one real codex job through the wave-3 adapter path with a gate manifest, on an isolated dev daemon, reconstructed end-to-end through the #24 query surface after a restart.

Output: the completion report for the whole campaign — per-wave PRs, checkpoint results, ORACLE-DELTAS entries, BLOCKED→issue spin-offs, and the first three real-world commands to exercise the new GitHub dispatch path on the *deployed* daemon.

**Reshape note:** if your improvements land between wave 7 and checkpoint 3, checkpoint 3 stays valid as written — it tests suites and composition, not specific surface. It is deliberately the natural "seal" after any body of work. You can also cheaply re-run it *again* after the improvements; it's one codex session.

### 2.4 Recommended orderings (pick per your improvement plan)

- **Conservative (my default):** wave 6 → wave 7 → checkpoint 3 → deploy → your improvements. Certified baseline + closed backlog before the surface moves. Cost: ~3 codex sessions, likely 1 fix session.
- **Improvements-first:** deploy post-wave-5 main → your improvements → wave 6 (larger ORACLE-DELTAS) → wave 7 → checkpoint 3. Choose only if the improvements deliberately rewrite the frozen surface, making a pre-baseline moot.
- **Split:** wave 6 now (cheap, locks baseline) → your improvements → wave 7 + checkpoint 3 after, with #34 rescoped to whatever fleet surface then exists. This is the highest-information-per-token option and my recommendation if the improvements touch remote/dmem.

Either way: **deploy the post-wave-5 build to the fleet early.** Everything painful about today's orchestration was running against a pre-wave daemon; every future dogfood session gets better data on the new binary. The deployed daemon also needs a gh producer (wave-2 scoped sources) + bot identity before the @-mention path can be exercised for real — self-mentions never generate notifications (#25 boundary 1, confirmed live).

### 2.5 Candidate new issues out of this campaign (for the reshape backlog)

From the field report + checkpoints, in rough value order:
1. Structured job *brief* distinct from argv (generalize `TALLY_GH_CONTEXT`), rendered separately in query output, hashed into the witness.
2. Adapter-provisioned gate manifests by default (`TALLY_GATE_MANIFEST` path; declared-but-absent ⇒ visible `not-run`), so agent jobs are semantically checkable without orchestrator re-runs.
3. `tally enqueue --explain` (dry-run showing resolved adapter argv/env/pools/policy) — extends the producer preview trilogy that proved itself in checkpoint 1.
4. First-class "final agent message" projection (today: jq spelunking in captures).
5. `--detach-parent` (or equivalent) for jobs that legitimately drive another tally instance (cp2's `env -u` finding).
6. Documented dedup-key hit semantics (what a duplicate enqueue returns).
7. Whatever wave 7 spins off as BLOCKED (expect dmem/servingSlice at minimum).

---

## 3. Runbook — dispatching the remaining steps

The exact pattern used for all eight dispatches today, for you or a future session (also in Claude's memory as `tally-nix-issue-wave-run`):

```bash
# per wave/checkpoint N with issue URL and slug:
git -C ~/mecattaf/tally.nix pull --ff-only
git -C ~/mecattaf/tally.nix worktree add ~/mecattaf/tally.nix-waveN -b wave-N-slug origin/main
# checkpoints use --detach instead of a branch

tally enqueue --pool build --source orchestrator --priority high \
  --runtime-max-sec 14400 --dedup-key wave-N-slug --wait -- \
  codex exec --json --dangerously-bypass-approvals-and-sandbox \
  -C /home/tom/mecattaf/tally.nix-waveN -- "<prompt pointing at the wave issue>"
```

Prompt skeleton that held up across all sessions: *authoritative instructions = the wave issue (read it with `gh issue view`); work only in the worktree; commit locally; HARD LIMITS: no push, no PR, no GitHub mutation (checkpoints: narrowly widened envelope, e.g. "one report comment on issue N"); run the full gate ladder and paste outputs; ambiguity ⇒ BLOCKED with the precise question, never invented semantics.* Codex respected these limits in 8/8 sessions.

Orchestrator's post-session ritual: re-run the gate ladder yourself in the worktree (`nix develop -c cargo test --workspace`; clippy `-D warnings`; `nix flake check`; witness verify valid GREEN / tampered RED; no-stubs grep) → push → PR with `Closes #…` → **merge from the main checkout, not the worktree** (gh tries to check out main and collides with the primary worktree; also never `cd` into a worktree you're about to remove) → pull, remove worktree, delete branch. Codex final reports are recoverable from `~/.local/state/tally/capture/<task-uuid>.out` via `jq -r 'select(.item.type=="agent_message") | .item.text'`.

Dedup keys used so far (do not reuse): `wave-1-gh-origin`, `wave-2-triggers`, `wave-3-ergonomics`, `checkpoint-1-gh-dispatch`, `cp1-fixes`, `wave-4-durable-query`, `wave-5-trace-watch`, `checkpoint-2-observability`.

---

## 4. One-line summary

Backlog #19–#31 is implemented, live-verified, and closed in one day of tally-dispatched codex sessions with two decisive checkpoints; what remains is the conformance pair (#33/#34 → #14/#15) and the final seal (#35), all filed and dispatch-ready — my recommendation is to lock the oracle baseline (wave 6) before your major improvements, rescope wave 7 around them, and keep checkpoint 3 as the seal whenever the dust settles.
