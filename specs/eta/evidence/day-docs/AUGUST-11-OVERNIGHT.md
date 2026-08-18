# August 11, overnight — three campaigns, the whole hardening backlog, and the unattended bar cleared

Autonomous overnight supervision session (Claude, per Tom's sign-off ~21:40Z). Repo
`mecattaf/tally.nix`, installed pin `8n1ihbds…` throughout — **zero deploys, zero manual
terminal starts, zero product code outside campaign dispatch**, per the standing rulings:
stay on the current pin all night; docs are the last part, all code-related issues first.

## Score

| Campaign | Outcome | Tasks | Wall clock |
|---|---|---|---|
| ch1 recovery (#473) | **complete 9/9** | +1 fmt task, checkpoint green | 21:45:00Z → 22:08:04Z (23m) |
| ch2 hardening (#497) | **complete 14/14** | entire filed backlog #481–#491, #494 | 22:14:00Z → 04:01:54Z (5h48m) |
| ch3 census (#513) | **complete 4/4** | 3 sweep lanes + checkpoint | 04:06:27Z → 05:58:41Z (1h52m) |

Seventeen pull requests merged by campaign tonight (#496, #500–#512, #524–#526), every
one through the witnessed gate ladder — 27 of 27 tasks across the three campaigns. **Zero worker failures, zero
machinery retries, zero steward escalations, zero steering comments, zero operator
interventions inside any campaign.** The unattended-readiness bar — a stormy-class
campaign completing with no operator intervention — is met, twice over.

## The headline measurements

1. **The marker-walk tax is dead on this pin.** Chapter 1's re-arm (worklist amendment
   adding one task + one dependency to the escalated checkpoint) minted **zero marker
   PRs and re-dispatched zero agents**. The pin deployed at 07:05 yesterday includes
   `bfc080a`'s per-task revision contract: only the checkpoint's revision rotated. The
   handoff's expectation of "7 live dispatches" was calibrated to older pins. This is a
   live confirmation of #490's claim, recorded on #459 by the campaign itself (PR #501's
   worker posted the evidence comment).

2. **The per-task `nix flake check` gate pays for itself immediately** (#488, adopted at
   arm time for ch2). Beyond preventing the #474 class at merge time, it made the
   end-of-chapter checkpoint nearly free: ch2's checkpoint witnessed `exit:0` in 0.7s
   because the final lane's gate had just built every check derivation for the identical
   tree — a pure eval-cache validation. (ch1's checkpoint, without gate warming, took
   ~7 minutes.)

3. **Recovery doctrine works end-to-end on the pin**: project amendment → arm → resume
   (manual pardon, since #456's auto-pardon isn't in the pin) → dispatch → merge →
   checkpoint → complete, in 23 minutes, with the steward never needed.

## What landed (chapter 2, in merge order)

| PR | Issue | Task |
|---|---|---|
| #500 | #494 | serialize linked-worktree preparation (the task-4/task-6 race) |
| #501 | #490 | revision-isolation field set documented; evidence recorded on #459 |
| #502 | #488 | recommended gate ladder + marker-safe changelog gate doctrine |
| #503 | #489 | **merge-control ruling landed — supervisor-verified byte-faithful** |
| #504 | #482 | task identity unified on registrationId; docs corrected |
| #505 | #483 | journal-filter unsoundness + containment boundary documented |
| #506 | #481 | worker findings channel: codex finalMessage capture + publication + arm warning |
| #507 | #484 | arm-time hardened-argv heuristic warnings + argv contract documented |
| #508 | #485 | Python rechartered: drivers/ move, 10 pinned sites updated |
| #509 | #486 | contract corpus single-sourced from Rust (the #471 class, fixed structurally) |
| #510 | #487 | language-entry policy: first tree-policy flake check |
| #511 | #491 | doc-as-oracle lane 1 (blocks 1–2): 91 claims — 58 verified, 6 divergent, 27 untested |
| #512 | — | doc-as-oracle lane 2 (blocks 3+7), incl. the reconcile-vocabulary divergence |

Chapter 3 completed the census (PRs #524, #525, #526 — `doc/audit/campaigns-*.md`).
**The full doc-as-oracle census, all seven doctrine blocks of campaigns.md: 582 claims
audited — 407 verified, 31 divergent, 144 true-but-untested.** Per lane:

| Audit record | Claims | Verified | Divergent | Untested |
|---|---|---|---|---|
| blocks 1–2 (admission, sub-issue walk) | 91 | 58 | 6 | 27 |
| blocks 3+7 (reconcile, parallelism) | 72 | 47 | 10 | 15 |
| gates (block 4) | 119 | 83 | 4 | 32 |
| worklist/checkpoints (block 5) | 141 | 92 | 7 | 42 |
| steering/failure (block 6) | 159 | 127 | 4 | 28 |

The 31 divergent rows are #521's work queue and the docs chapter's gate; the
reconcile blocks (3+7) carry the worst divergence density, which is also where the
agent-facing campaign reference (#465) would lean hardest.

## Interventions ledger (all outside campaigns, all recorded)

1. **21:41Z** stopped `tally-producer-nightly-fleet-deploy.timer` for the night — its
   02:00 root deploy-rs activation of freshly-resolved `dotfiles/main` onto this host
   (`coordinator`) is a mid-campaign redeploy by automation.
2. **21:42Z** fast-forwarded the local checkout `089d000 → 60f698b` (7 behind; hygiene).
3. **04:07–04:09Z, the night's one operator error, contained**: restoring the timer
   fired an immediate `Persistent=` catch-up; `fleet-deploy.service` reached ~15s of its
   *resolve* phase before I stopped it. Nothing was built or activated.
   `fleet-deploy-alert` fired — **if you have an alert notification, this is it.** Unit
   reset, timer restored once today's producer dedup slot made re-fire inert. Next fire:
   **Wed 2026-08-12 02:00, normal.** Lesson: a stopped `OnCalendar` timer with
   `Persistent=true` fires its missed window on restart.

## Filed tonight (supervisor)

#518 preflight rehearsal verb (#484 ask 1) · #519 journal campaign key (#483 ask 1) ·
#520 contract peer copies (#486 residual) · #521 **divergence-fix program — the docs
chapter's gate** · #522 unclaimed campaigns.md regions sweep · #523 **ruling request:
lineage docs under merge control**.

## Decisions waiting for you

1. **Deploy the validated tree.** Both checkpoints proved the integrated tree green under
   the hardened tier. Runbook: bump the `tally` input in dotfiles → build → switch →
   `tally adapter smoke codex --pool campaign-agent --assert-commit` → one-task probe
   campaign (doctrine: probe before first arm on a new pin) → rollback is one generation.
   The first campaign on the new pin finally answers the standing question: does machine
   steering deliver with #455's fix in the pin? Bonus on the new pin: auto-pardon (#456),
   agent-free restamps (#459), checkpoint captures (#457), findings channel (#481),
   arm-time argv warnings (#484). After deploying, delete the current-pin workaround
   notes from the operator skills — they describe the old pin.
2. **The docs chapter** (#462–#466, #469, #470). My recommendation: not until #521's
   divergent rows (6 in blocks 1–2 alone; ch3 will complete the count) are ruled and
   fixed — agent-facing docs written on known-divergent doctrine inherit the divergence.
   The census + fix program makes that a one-day campaign.
3. **#523** — one word settles whether lineage docs (this file included) are
   hand-committable or campaign-only.

Working material still held uncommitted by your ruling: `skills/*.md` edits,
`AUGUST-10-LEARNINGS.md`, `aug10-midday-session.md`, and this report. The full night log
with timestamps: `~/.cache/tally-ch1-handoff/NIGHT-LOG.md`.
