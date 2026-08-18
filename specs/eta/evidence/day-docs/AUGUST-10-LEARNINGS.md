# August 10 learnings — crm-call-drain: the first real mission on the daily-driven pin

The H3/H4 evidence the grind appendix asked for. Campaign `crm-call-drain`
(mecattaf/dotfiles#163) ran to **completion: 11/11 tasks settled**, on pin
`569ee2a`, starting the evening of Aug 9 and closing itself out early Aug 10.
The chapter's acceptance test passed end-to-end: the pilot contact is queryable
in the live CRM (`crm context c1663`) with sourced context and two logged calls
whose diarized transcripts exist on disk; the public absorption contract landed
as `docs/crm/call-absorption.md`; the merged machinery (crm CLI, call-diarize,
recordings auto-queue drain) activated at the ordinary nightly fleet deploy,
exactly as the campaign scope deferred it.

**The grind's prediction held: every failure was operational, none was
contract-skew.** Reconcile, sweep, preflight, gates, publish/rebase/merge,
checkpoints, lanes, pools, restart recovery, and both terminal outcomes
(escalation and completion) behaved as documented across 6 arms and ~15 passes.

## Score

| Layer | Verdict |
|---|---|
| tally core (admission, pools, lanes, gates, merge criterion, recovery) | 100% — zero defects observed |
| tally steering/diagnosis layer | 1 real defect (#451) + ergonomic gaps below |
| codex adapter (worker survival) | weak link: 2 of 3 call-diarize attempts died to codex-side tool-router exits |
| workload code (call-diarize) | 3 real defects found and fixed *by the campaign itself* |

## Dispatch metrics (the pilot's measured question)

- 11 issues filed (8 original + 3 defect-fix tasks added mid-campaign).
- 7 implementation tasks merged via worker PRs, 4 checkpoints witnessed green.
- **Zero manual terminal starts for any worker code.** All code was written,
  committed, gated, and merged by dispatched agents.
- 9 worker attempts for 7 implementation tasks (call-diarize took 3).
- Backfill checkpoint ran 4 times before green — each failure surfaced a real
  workload defect that became a filed, dispatched, merged fix task.
- Operator interventions (all non-coding): 6 arms, 2 human steering comments,
  3 graph edits (+1 PII scrub), 2 rounds of receipt deletion, 1 tally.nix
  issue filed (#451).

## What the campaign machinery got right (worth keeping verbatim)

1. **Bounded escalation is the right shape.** Frontier quiescence after two
   steered attempts per task stopped the campaign instead of looping GPU burns.
2. **Lane preservation across worker deaths.** Attempt 3 of call-diarize
   inherited ~30 min of ROCm/gfx1151 provisioning from two dead codex sessions.
3. **Forge-derived scheduler state.** Attempts live in issue comments; a
   daemon restart mid-campaign (nightly fleet deploy at 02:06) lost nothing.
4. **The admission door.** Editing an armed issue body halts polling with
   "explicit re-arm is required" — annoying in the moment, correct in doctrine;
   an unadmitted graph never executed.
5. **Checkpoints as defect finders.** The backfill checkpoint converted three
   latent call-diarize bugs (non-idempotent ffmpeg retry, no resume over
   partial evidence, fatal cleanup-shard validation) into dispatchable issues
   with exact stderr evidence. The tool that survived is drain-grade.

## Defects and sharp edges (tally-side)

1. **#451 (filed): rejected steward diagnoses consume machine-steering
   attempts.** Both call-diarize diagnoses were rejected on the past-tense
   grammar rule; the escalation arrived with zero diagnostic content and the
   retry budget spent on machinery faults — contradicting the doctrine that
   only evidence about the work spends an attempt.
2. **No operator verb for post-escalation resume.** The working recovery is:
   delete the machine diagnosis receipts (+ escalation marker), re-arm. It
   follows from receipts-are-counters, but it is undocumented, feels like
   tampering, and deserves a first-class `tally campaign resume`/pardon verb.
3. **Graph edits invalidate completed-task markers → marker-walk tax.** Every
   re-arm after a graph edit re-walks each completed task with a live agent
   (stateless reconcile attempt) and merges an empty marker PR. This mission:
   13 ignored/empty PRs and ~30 min of walk per re-arm at 5–6 completed tasks.
   Correct (stateless recovery) but expensive; consider carrying forward
   completion facts across digest changes, or a marker re-stamp that needs no
   agent dispatch.

   **Correction (2026-08-11):** This measured whole-graph rotation was fixed by
   `bfc080a` (#453, merged 2026-08-10). Completion identity is now per-task, so
   unrelated task, capacity, or scheduler edits do not rotate an already
   completed task's revision; the observation above applies only to pre-#453
   code.
4. **`campaign must contain 1..=8 tasks`** is the manifest's own `maxTasks`
   echoed back without saying so — misread as a hard schema cap until source
   dive. Error should name the field.
5. **Empty PRs are indistinguishable from real ones by title.** Marker PRs
   reuse the task title; only the 0-file diff reveals them. A `[marker]` title
   prefix would save the reviewer a click.

## Worker-side learnings (adapter = codex 0.145, exec mode)

- codex sessions exit 1 when the tool router errors: once on the `rm -rf`
  safety rejection, once after an `apply_patch` verification mismatch. The
  session death, not the underlying error, is what costs an attempt.
- Human steering that works: (a) commit checkpoints in the lane after every
  milestone, (b) never use `rm -f`/`rm -rf` in shell commands — do file
  removal in Python (`shutil`), (c) prefer several small patches over one
  large one, (d) inspect preserved lane state before redoing anything.
  call-diarize attempt 3 ran 50 min to a clean pass under exactly this
  steering; every subsequent worker passed first-try with it inherited.
- For a *deterministic* workload defect, do not let the checkpoint retry:
  file a dedicated implementation task with the stderr evidence and gate the
  checkpoint on it. Checkpoint retries only pay off for transient failures.

## Operator playbook extracted (seeded into the assign-tally skill)

arm → monitor job transitions → on worker death read
`tally query log --task <uuid>` → classify: transient (steer + let retry) vs
deterministic (file fix task, wire dependency, re-arm) vs tally defect (file
in tally.nix, work around) → on escalation: fix root cause, delete junk
receipts, re-arm → completion closes the board itself.

## Open follow-ups

- tally.nix#451 (diagnosis grammar) — filed with repro.
- Consider filing: post-escalation resume verb; marker-walk cost; maxTasks
  error wording; marker-PR title prefix. All observed, none blocking.
- dotfiles#172/#173 (attic cache, drain gating) predate this campaign and
  remain open — not campaign scope.

---

# August 10 learnings, part two — dcal-calendar: the tumultuous second mission

Same day, second campaign, very different ride. Campaign `dcal-calendar`
(mecattaf/dotfiles#210) ran armed 07:56Z → **completion: 13/13 tasks settled**
at 12:05Z (~4h10m), on the post-#199 pin. The mission: vendor an upstream Go
calendar engine into `pkgs/dcal`, strip it headless, rebrand it under a
forbidden-token ruling, build the cobra CLI, project tally's own producers into
a read-only calendar, wire the CRM linkage, package/activate declaratively —
10 planned tasks, 7 implementation + 3 checkpoints. It finished with 13: the
checkpoints caught three real product defects and each became a dispatched,
merged fix task. Where crm-call-drain validated the machinery on a calm sea,
this run validated it in weather: five operator errors, three genuine dcal
bugs, three tally-side defects, three escalations, three pardons, and still —
completion, with zero manual terminal starts and zero worker code written by
the operator.

## Score

| Layer | Verdict |
|---|---|
| tally core (admission, pools, lanes, gates, merge criterion, receipts) | 100% — again zero defects; every failure it reported was real, every merge green |
| tally steering/diagnosis layer | 0% effective — five of five steward diagnoses discarded by its own grammar (#455) |
| escalation lifecycle | correct but operator-dependent: resume verb works, but re-arm-after-surgery silently doesn't pardon (#456) |
| codex adapter | strong run: no session deaths; implementation attempts 8–20 min each, fix tasks 8–12 min |
| workload code (dcal) | 3 real defects found and fixed *by the campaign itself* |
| operator (me) | 5 self-inflicted incidents; every one is now doctrine in the assign-tally skill |

## Dispatch metrics

- 13 issues filed (10 original + 3 defect-fix tasks added by graph surgery).
- 10 implementation tasks merged via worker PRs (#211–#223 lanes), 3
  checkpoints witnessed green on the integrated tree.
- 5 arms (serials 1–5: initial + 3 surgeries + 1 argv amendment), 3 resumes
  (first real-world use of the pardon verb — it works exactly as specified),
  2 human steering comments, 3 tally.nix issues filed (#455, #456, #457).
- Elapsed overhead attributable to incidents: ~2h of the 4h10m. A clean rerun
  of the same graph would land in roughly half the wall clock.

## Item-by-item: what happened and what each event teaches

1. **Arm rejected: missing `:end` markers.** The master body closed the
   manifest and worklist blocks by repeating the *begin* markers.
   `tally campaign arm` refused with the exact missing string, registered
   nothing, and recovery was a body edit + clean re-arm. Tally behaved
   perfectly; the begin/end marker pair was simply undocumented in the
   operator skill. *Lesson: both `:end` closers are mandatory; the arm error
   is precise enough to fix blind.*

2. **The forbidPaths gate caught an innocent chime — and taught the real
   semantics.** Upstream ships `internal/notify/assets/message-new-instant.wav`;
   my gate forbids `*.wav` to keep call recordings out of the public repo. The
   vendor worker's tree was correct three commits deep when the `constraint`
   stage refused the lane. First lesson: *sweep every tree you intend to
   VENDOR against your own gate patterns before arming* — the gate cannot tell
   a notification chime from a call recording. Second, sharper lesson came two
   failures later: **`evaluate_forbid_paths` walks
   `changed_paths_in_history(union_base, head)` — the lane's whole commit
   history, not the final tree.** My steering to `git rm` the asset produced a
   clean HEAD and a third gate failure, because the vendor commit had already
   touched the path. The only cure is rewriting the lane so the file never
   enters any commit (soft-reset to merge-base, recommit without it). The
   machine steward made the same mistake twice ("preserve the existing commits
   and remove the WAV") — tally's own advisor does not know its own gate's
   history-walk rule.

3. **The steward grammar bug is systematic, not occasional (#455).** Last
   night's #451 looked like a past-tense-verb annoyance. Today upgraded it:
   five of five steward diagnoses were rejected — "diagnosis omits the failing
   check id 'no-data-artifacts'" — because the #385 content contract requires
   the diagnosis text to contain the check id and offending path as **literal
   substrings**, and the steward prompt never tells the model that. Every
   discarded diagnosis was substantively correct (the chime, the migration
   race, the readiness race — all correctly identified, with sound fix plans,
   visible only in the redacted excerpt of the rejection notice). Net effect:
   the machinery-retry budget burns, the worker retries blind, and **all
   effective steering in this campaign was human**. This is the single highest
   -leverage fix on the tally side.

4. **Steering comments race attempt-prep.** My first steering comment posted
   71 seconds after the retry worker's prep collected the thread; that attempt
   ran unsteered and spent a budget slot repeating the known failure. Steering
   is collected at prep time, full stop. *Lesson: post steering the instant the
   diagnosis is solid — never wait for the running attempt to finish — and
   expect one wasted attempt when you lose the race.*

5. **Checkpoints as defect finders, round two — and the doctrine works.** The
   integrations smoke exposed a genuine dcal bug: on a fresh data tree,
   `dcal status` (socketless fallback) raced the daemon's first-run goose
   migration and killed the daemon (`database is locked (SQLITE_BUSY)` /
   `table goose_db_version already exists`). Reproduced locally 3/3 before
   acting. Per doctrine — checkpoints verify, they cannot fix — this became
   implementation task #218 (`dcal-migration-race`: advisory-lock migration
   serialization + concurrent-open regression test), wired as a dependency of
   both smokes, re-armed. The worker fixed it in ~8 minutes; my 5/5 fresh-tree
   verification passed. Two more real defects followed the same path:
   `done --dry-run` exiting 1 when no recording exists instead of previewing
   (#220, also proof the checkpoint never touches real recordings), and
   socket-requiring CLI commands failing against a concurrently-starting
   daemon (#222 — the right fix for real systemd-managed usage anyway). Three
   latent bugs that would have bitten the first real deploy, all converted
   into merged fixes by the campaign itself. This pattern is the product
   working as designed.

6. **Cross-task contract drift is the subtlest failure class.** The #218 fix
   made `dcal status` succeed daemon-lessly — correct in isolation, and it
   silently invalidated the smoke checkpoint's `until dcal status` readiness
   loop, which now returned before the daemon bound its socket. My interactive
   shell hid the race (daemon won); tally's hardened unit exposed it (daemon
   lost by milliseconds — the sandbox reproduction showed the socket binding
   *after* `calendar add` had already failed). *Lessons: (a) probe the actual
   condition (socket exists), never a proxy whose semantics a sibling task can
   legitimately change; (b) validate every checkpoint argv under an equivalent
   `systemd-run` sandbox before arming, because the checkpoint environment is
   stricter than your shell — that strictness is a feature, it found a real
   race — but it must be part of authoring, not a surprise.*

7. **The quiescence race (#456a).** At 10:20:04Z the campaign closed itself
   `outcome=quiescent`, naming the integrations smoke blocked — while that
   checkpoint's re-run on the freshly fixed tree was passing in the same
   window (started 10:18:13Z). One settled-fact refresh before declaring
   quiescence would have avoided a full stop. `tally campaign resume` (the
   verb last night's learnings asked for — it exists, it works, it pardons
   counters without deleting the audit trail and posts a receipt) recovered
   cleanly.

8. **Re-arm does not pardon (#456b) — learned twice in one afternoon.** After
   graph surgery on an escalated task, the amended graph dispatches the *new*
   fix task, but the escalated checkpoint keeps its spent two-attempt budget;
   once the fix merges, the frontier goes quiescent again, silently, because
   the escalation comment was already posted. Hit on dcal-smoke (escalated
   11:02Z, eleven minutes before the fix merged) and again on final acceptance
   (escalated 11:42Z, racing my argv amendment by seconds). *Operator rule,
   now in the skill: after any escalation, re-arm and then ALWAYS resume once
   the fix has merged. Tally-side ask: auto-pardon a task whose escalation the
   amendment addresses, or warn in the arm receipt.*

9. **Checkpoint argvs must be hermetic (exit 127).** Final acceptance assumed
   `go` on PATH. True in my shell; false in the hardened unit. The fix — and
   the doctrine — is nix-sourced toolchains inside the argv:
   `nix shell --inputs-from . nixpkgs#go -c sh -euc '…'`, Go caches pointed at
   the unit's private `/tmp` (HOME is read-only), `CGO_ENABLED=0` exported
   globally (vet/test demand a C compiler otherwise). Three sandbox iterations
   to green before amending the manifest; the campaign then settled 13/13 on
   the next pass.

10. **Failed-checkpoint output is nearly unreachable (#457).** The only trace
    of a checkpoint failure is journald's `TALLY_STDERR_TAIL` — the last
    stderr line. Each of the four checkpoint failures cost a full sandbox
    reproduction (~15 min) that a persisted 8KB stdout/stderr capture, linked
    from the retry comment, would have made a one-glance diagnosis.

## What held from part one, verbatim

Lane preservation (the vendor task's 305-file tree survived five verdict
cycles untouched), forge-derived counters, the admission door, bounded
escalation, and the checkpoint-to-fix-task doctrine all behaved exactly as
documented. The marker-walk tax was visible but tolerable at this scale.
The codex adapter had a clean day: zero session deaths, and the survival
guidance baked into every brief (no shell `rm`, python `shutil`, lane commits
per milestone, small patches) appears to have become free insurance.

## The unsupervised-runs verdict (the question this campaign was run to answer)

Tally's **enforcement** half is trustworthy today: nothing false was reported,
nothing red was merged, and the audit trail reconstructs every decision. Its
**self-healing** half is not yet real: #455 reduced machine steering to noise,
and the escalation lifecycle assumes an operator who knows the resume verb and
the re-arm-doesn't-pardon trap. Concretely: a fully unattended dcal run would
have died at the first gate failure and stayed dead. With #455 fixed (steward
advice actually delivered) and #456 fixed (no silent post-surgery stalls),
this same campaign plausibly completes with zero operator interventions —
every fix task it needed was authored correctly by the machine, just never
delivered. That is a much better position than "the advice is wrong": the
knowledge exists, the pipe is clogged.

## Open follow-ups

- Filed with evidence from this run: tally.nix#455 (steward literal-substring
  grammar — supersedes #451's scope), #456 (quiescence fact refresh +
  re-arm/pardon coupling), #457 (checkpoint output capture), #458 (steward
  blind to forbidPaths history-walk; gate message wording), #461 (steering/
  prep race — acknowledge or re-check before dispatch).
- Cleared part one's "consider filing" backlog: #459 (marker-walk tax +
  [marker] title prefix, both campaigns' measurements), #460 (maxTasks error
  wording). The post-escalation resume verb from that list exists and is
  proven — no issue needed.
- assign-tally skill draft updated in dotfiles: `:end` markers, gate
  history-walk semantics + vendor-tree sweep, §6 rewritten around the resume
  verb, new §6b checkpoint-argv doctrine (hermetic toolchains, probe-not-proxy,
  sandbox validation, steering race).
- dcal chapter DoD remains human-gated: Tom verifies the deployed artifact,
  then dotfiles#93 and #198 close together on his explicit approval.
