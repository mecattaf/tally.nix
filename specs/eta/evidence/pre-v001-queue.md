# Pre-v0.0.1 queue — post-eta operator sitting, 2026-08-19

Post-close sitting, held after eta's terminal disarm (P4 green, run-log
final entry). Drains the rulings that remained open at close and fixes the
single checklist the v0.0.1 mint reads instead of five sources. Standing
consumers: the mint session; the journey session (tally's first end-to-end
non-tally build, the daily-driver ratchet evidence); the next sitting that
touches skills/campaign-operator. Findings below were re-verified against
the tree at 14e9f9a1 by a four-surface directed pass this sitting
(decision record, code verification, hygiene, host residue).

## 1. Rulings drained this sitting (operator)

1. **§8.1 seam ruling — CARRY THE WAIVER, affirmatively ruled.** W-316
   stands as a *named known limitation*: jobs admitted via the
   `attached` / full-mode `reused` / `terminal` paths write no durable row
   of their own, so the submitting run's `query jobs --flow-run` window
   can silently omit its own node; the additive membership ledger
   (`crates/tally-core/src/flow_membership.rs`) papers over the visibility
   symptom and has held under live fire. **Operator intent recorded: this
   is not a permanent blessing.** The inconsistency is disliked and the
   root fix (unfreezing the enqueue kernel's row-ownership model) is
   wanted post-journey; until then, release-facing docs must name W-316
   as a known limitation at the mint. The charter's carry-the-waiver
   default is hereby converted from silence into a ruling.
2. **Version numbering — the tree stays 0.1.0, untouched.** Cargo.toml,
   the three flake.nix declarations, and the deployed binary all say
   0.1.0 and remain so right up until the operator opens the actual
   v0.0.1 mint work — at which point the version move happens together
   with the history cut (the gh repo cleared of forge lineage) and the
   RELEASING.md true-up, as one deliberate act. Nothing pre-mint touches
   version bytes.
3. **Deferred-onto-journey-evidence, affirmed:** §8.4 (adapter mix;
   metered window resets 2026-08-22 13:34 UTC), §8.5 (judge-tier replay —
   n=1 corpus today; the repaired verdict recording accumulates the real
   corpus during the journey; the replay is one command once it exists),
   the checkpoint-ancestry question (should checkpoint receipts survive
   re-arm when the proven head is an ancestor — journey measures the
   re-witness tax), and the completed-campaign reopen-verb question
   (journey shows whether immutable completion ever actually hurts).

## 2. The journey (context for every consumer of this file)

Next act: tally builds an ambitious non-tally project end-to-end via the
D1/D2 product path — this is ratchet 1 of the v0.0.1 blessing running
live. Bar: ordinary work flowing through as ordinary campaigns with zero
hand-performed recoveries; every friction tally causes is recorded as a
named finding (what, where, escape used), a first-class deliverable
beside the project itself. The mint waits on both ratchets; nothing in
this file changes that sequence.

## 3. Post-journey refinement queue (each verified in code this sitting)

1. **Completion-fact migration bridge** — dispatch-time done-ness never
   consults the legacy identity; only the release-summary bridge does
   (`crates/tally/src/cli/campaign.rs:2338`,
   `crates/spec-build-driver/src/actions.rs:1357`). Re-arming a
   pre-contract campaign re-dispatches finished work. The record's own
   "first post-eta item." Medium.
2. **Base-only re-admission** — admission keys on worklist-file digest
   alone (`campaign_contract.rs:748`, `campaign_lease.rs:419`); a
   base-only fix cannot re-admit. Fold base-tip awareness into admission
   identity. Medium.
3. **Probe honesty** — `execute_campaign_release_probe` carries separate
   `release_complete`/`teardown_complete` but folds both into `passed`
   (`campaign.rs:1167`), and no gh-scope preflight exists. Do before the
   mint: the mint's release proves through this path. Medium.
4. **Checkpoint ancestry short-circuit** — checkpoint refs require exact
   base_rev match (`actions.rs:~7069`); no is_ancestor logic. Only if the
   journey shows the re-witness tax matters. Medium-large.
5. **Estate-population replay gate** — the one ext2 item with zero code
   behind it (no task, no check); the only fleet-down class without
   standing coverage. Medium-large.

Plus whatever the journey's findings list adds — expected to be the
richer source.

## 4. Field notes (interim home; destination: skills/campaign-operator)

The operator dislikes these living loose; they roll into the
tally-adjacent skills at the first post-journey refinement commit. Until
then, binding on any session operating a campaign:

- Never re-arm a pre-contract campaign (eta/epsilon era) — see queue
  item 1.
- A base-only fix can't re-admit; escape: any honest worklist edit
  forces a fresh digest.
- A completed campaign is closed for good (digest-scoped summary ref,
  by design); extend by amendment before completion or scaffold a
  successor.
- Re-arms re-run all checkpoint gates; batch record commits to
  campaign-quiescent windows — every base push re-keys the observation.
- Never amend/re-arm mid-attempt; stop the poll timer, verify quiet,
  amend in the gap.
- Before arming a delete-a-default task, grep the name repo-wide and put
  every hit inside declared conflictDomains.
- Read the typed outcome and capture tail before any retry; terminal
  conditions (quota, OOM) stop the ladder.
- The escalation inbox has never carried live traffic — the journey's
  first escalation is its first live fire; watch it.
- W-316 (§1 ruling above): an empty own-run job page is not proof of no
  work.

## 5. Pre-mint housekeeping (cheap, unordered, none journey-blocking)

- Delete `mecattaf/tally-probe-20260814-6bf9bac2` (still exists, private;
  needs `gh auth refresh -s delete_repo`, then drop the scope again —
  the token holds `admin:org, gist, repo, workflow` today).
- Settle dotfiles PR #225 (still open: "music-acquire: add verified
  cascade and deploy tally 52eff4db").
- Delete `aug9-pass/` and `aug12-campaign-prep/` (tracked; defended by
  nothing in the record — genuine E4 residue).
- README for `silent-factory-worklists/` naming eta.json the only live
  worklist (ZETA.md promises a zeta.json that never existed; ch0–ch5/chR
  are closed history).
- Rewrite RELEASING.md around `tally campaign release` (it still
  describes the manual gh-release path).
- Scrub the 22 forge `#NNN` refs in the published book (`doc/src/**`);
  the 414 under `specs/**/evidence/` die as frozen provenance.
- Prune `~/.local/state/tally/` (8.9G, of which campaigns/eta is 8.0G) —
  retention call is the operator's.
- Dying at the cut as already planned, no action: V-17's `Sonnet`
  literal in epsilon-extension.json; the three root charters (ETA.md,
  ZETA.md, EPSILON-EXTENSION.md); README's `#321` parenthetical and
  legacy-docs pinned link.

Deployed pin note: daemon runs a51b339c; HEAD (this commit's parent
14e9f9a1 and this commit) is record-only on top of it — no redeploy
owed.
