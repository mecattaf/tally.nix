# AUG12 DAY-RUN HANDOFF — full silent-factory chain, end to end

Written 2026-08-12 morning by the overnight steward session, at the operator's
instruction, immediately before context compaction. The operator's words:
"proceed with the full chain supervision end to end." This document is the
continuation brief; it supersedes AUG12-HANDOFF.md's STEP-C ceiling, which the
operator has explicitly lifted. Everything else in the doctrine stack
(SILENT-FACTORY-PLAN.md decision register, assign-tally, campaign-operator)
still binds. Where this conflicts with the plan, the plan wins.

## Mission

Arm and supervise the realizing campaigns of SILENT-FACTORY-PLAN.md as a
sequential chain, one campaign per chapter, until the pass is complete or the
failure protocol stops it:

    Chapter 1 → Chapter 2 → Chapter P → Chapter 3 → Chapter 4 → Chapter 5

Worklists: `silent-factory-worklists/ch1.json … ch5.json, chR.json` — 46 tasks
total (ch1 6, ch2 16, ch3 6, ch4 6, ch5 7, chR 5), validated, authority bytes
on origin/main since `84786f4`. **Before arming chR, read it to confirm its
subject**: per plan §5.4 the contingent chR worklist was activated as Chapter P
(producers deletion) — verify the task ids match the producers surface, not the
read-model chapter, and slot it after Chapter 2 (its precondition 2.G1, the
Assisted-by relocation, is a ch2 task).

## State at handoff (all verified this morning)

- `origin/main` = `13307c67` (PR #528, `tally flow render`, merged by last
  night's campaign #527 — one clean pass, zero interventions). Local `main`
  may still be at `84786f4`; `git pull --ff-only` is safe and useful.
- Installed pin: `/nix/store/fxn0jycxp2xyyakflw74a2vwk40skxvf-tally-0.1.0`,
  generation 120, proven by campaigns #513 (older pin), sodimo's, and #527.
- Sodimo/os campaigns are the operator's own and run on the shared `campaign`
  mutex (this morning: sodimo/os#43 live). NEVER touch, steer, or report on
  them. Queue time is free (ruled from code, AUG12-overnight.md §6): armed
  runners wait indefinitely at zero budget cost; passes interleave on the
  mutex.
- The 02:00 Aug-12 fleet-deploy skip fired correctly (witnessed); the Aug-13
  02:00 deploy is LIVE again — if the chain is still running Wed night,
  consider that timer before it fires (do not touch it without instruction;
  surface it to the operator in time).
- Records: AUG12-overnight.md §6–§12 (last night, full evidence),
  aug12-campaign-prep/ (artifacts incl. the proven #527 projection input
  `project-worklist-467.json` and the ch1 module ready-diff, which stays
  NOT deployed — it is post-shift shape, blocked by F10 until authority v3).

## Arming mechanism — use the PROVEN #527 pattern, per chapter

Plan D49's module-declared forge:"local" shape is not yet mechanizable:
F10 (`common.nix:3619` hardcodes forge github at render) and F4 (`campaign arm`
requires a GitHub issue URL even with `--allow-test-local-forge`). Do not
experiment mid-chain. Each chapter arms forge-native ad-hoc, exactly like #527:

1. Compose the project worklist document:
   `{schemaVersion: 1, campaign: <manifest object>, tasks: <chN.json tasks>}`.
   - The ch worklist tasks carry goal/deliveredBehaviors/readFirst/
     acceptanceCriteria/conflictDomains — `campaign project` renders briefs
     from those directly (`campaign.rs:2827-2897`); tasks have no `issue`
     field, so project CREATES task sub-issues (that is sanctioned mechanism).
   - Checkpoint tasks (e.g. ch1 `chapter-gate`, argv `bash test/fleet-gate.sh`,
     runtimeMaxSec 7200) pass through with argv+runtimeMaxSec.
   - Campaign object: base it on
     `aug12-campaign-prep/campaign-467-manifest.json` minus `tasks`, with per
     chapter: `name` = `silent-factory-ch<N>`, `maxTasks` = task count,
     `maxParallel: 2` (every implementation task carries conflictDomains, so
     >1 is valid), same repository block, pool `campaign`, same agent
     (codex / danger-full-access / never / diagnosis read-only, 7200s), same
     six-gate ladder, driverRuntimeMaxSec 900, runtimeMaxSec 86400,
     mergeMethod squash, gitAiBinding off.
2. Master issue: prose only, rendered from the plan (D46 — operator's project
   document verbatim): the chapter's table/intent section from
   SILENT-FACTORY-PLAN.md Part 3, plus the self-hosting notice. Then
   `tally campaign project --repo mecattaf/tally.nix --issue <url> <doc.json>`,
   verify markers paired + prose intact, then `tally campaign arm <url>`.
3. The runner queues behind whatever holds the mutex; that is fine and free.

## Sequencing and supervision rules

- Strictly one chapter armed at a time; arm N+1 only after N's master issue is
  CLOSED by its terminal pass (receipts comment posted) and its merges are on
  origin/main. Chapter order is operator arming discipline (plan impedance 1).
- Campaigns run ON THE INSTALLED PIN; chapters change source on main, never
  the running mechanism. NO pin redeploy during the chain (prohibition: never
  restart tally-* units while campaigns run — sodimo's are effectively always
  live). The merged end-state reaches the system at a later deliberate deploy
  by the operator.
- Line numbers cited in later chapters' goals were taken at plan time and WILL
  drift as earlier chapters merge. Expected and priced (workers read the
  tree); do not diagnose drift as spec contradiction unless a task is actually
  unimplementable.
- Known expected weather, do not misdiagnose (plan §5.2): the v1-marker
  spelling appears until ch1's `worklist-task-revision` lands; the campaign
  journal filter is unsound on self-hosted runs — corroborate against
  `tally query run` and forge state only.
- Supervision per assign-tally: healthy = zero intervention; wall-clock is
  never a reason. Failure protocol exactly as written: stderr tail first;
  transient → re-trigger; agent fell short → ONE evidence-based steering
  comment on the master issue, re-trigger; two failures on one task with good
  steering, a spec contradiction, or a mechanism smell → STOP THE CHAIN, write
  the diagnosis into AUG12-overnight.md, leave the remaining chapters unarmed.
- Watcher pattern that worked: background poll (60–120 s) on
  `systemctl --user list-units 'tally-job*'` for the runner uuid + pool
  counters; then `gh issue view <master> --json state` for the terminal close.
- Record as you go: extend AUG12-overnight.md in place per chapter (armed →
  outcome → merges, with query/forge witnesses). Nothing on GitHub beyond the
  campaign mechanism. gh contacts per chapter: master issue + project's writes
  + task sub-issues + the machinery's PRs/merges/closes. No narration.

## Standing prohibitions (unchanged)

No hand edits to tally source, nix modules, skills, tests — every code change
rides a campaign lane. No fleet deploy; no touching either fleet-deploy timer.
No restarting any tally-* unit. No tags, releases, workflows. No reading
scratch state into ledgers. Sodimo campaigns untouched and unreported.

## Definition of done

All six chapter campaigns closed with receipts; origin/main carries the full
pass; AUG12-overnight.md holds the witnessed chain record; a final section
lists per-chapter merges, failures/steering counts (the #455 answer if weather
occurred), and the deliberate-deploy recommendation for the operator. If the
chain stops early, the record says exactly where, why, and what evidence.
