# AUG 12 — overnight steward record (PREP PHASE)

Status: **prep complete through the NixOS switch; stopped before the probe campaign
and the last forge-native campaign**, at the operator's instruction (mid-session:
"once, and only once, the prep work is done (that includes the nixos switch fwiw)
you will give me a /handoff"). This file is the running record; the continuation
session extends it.

Nothing was filed on GitHub. No issue, comment, sub-issue, label, tag, release, or
workflow was created or modified. The only outbound act was one `git push` of the
sanctioned plan commit (D8 contact 1).

---

## 1. FF-SYNC — COMPLETE

`git fetch origin` then fast-forward-only merge. No rebase, no force.

- Before: local `60f698b`, origin/main `23509ab`, `git rev-list --left-right --count HEAD...origin/main` = `0	17` (0 ahead, 17 behind — a clean ff).
- After: `git rev-parse HEAD` = `23509ab11ebf8567114e96c87314c88fc2fc9eda` = origin/main.
- The `drivers/` relocation landed (`examples/flows/{spec_build_driver,campaign_worktrees,agency_nightly_driver}.py` → `drivers/`), so D48 item 3 is satisfied: the worklists' paths are no longer phantom.

**Obstacle found and resolved (finding F1).** The ff was blocked by an untracked
working-tree copy of `AUGUST-10-LEARNINGS.md`, which the 17 commits also add as a
tracked file. Diffed before touching: the local copy is **strictly older** — it lacks
the upstream-added paragraph "**Correction (2026-08-11):** This measured whole-graph
rotation was fixed by `bfc080a` (#453, merged 2026-08-10)…". No local content existed
that upstream lacked. The stale copy was preserved and then removed so the ff could
restore the tracked, newer version.

Backup: `<scratchpad>/AUGUST-10-LEARNINGS.local-stale-backup.md`
(sha256 `ccfc06ac…647ca2`; upstream version is `b342afa5…20d7b`).

## 2. THE ONE HAND COMMIT — COMPLETE

Commit `84786f4` on `main`, pushed to origin (`23509ab..84786f4  main -> main`).

    docs(plan): land the silent-factory pass plan and worklists

Staged exactly the three sanctioned artifacts, 8 files / 2,355 insertions:
`SILENT-FACTORY-PLAN.md`, `AUG11-evening-pass.md`, `silent-factory-worklists/ch{1,2,3,4,5,R}.json`.

Deliberately **not** touched, left for the operator: the modified
`skills/assign-tally/SKILL.md`, `skills/campaign-operator/SKILL.md`,
and untracked `AUGUST-11-OVERNIGHT.md`, `aug10-midday-session.md`.

**Witness — §5.2.3 satisfied (worklist authority requires committed remote bytes):**

    $ git ls-tree -r --full-tree origin/main --name-only | grep -E 'silent-factory-worklists|SILENT-FACTORY-PLAN'
    SILENT-FACTORY-PLAN.md
    silent-factory-worklists/ch1.json … chR.json

**Witness — worklists re-validated against the checked-out driver** (the plan's own
validation method, `normalize_task(..., require_conflict_domains=True)` imported from
`drivers/spec_build_driver.py` at `23509ab`):

    ch1 6 tasks ok · ch2 16 · ch3 6 · ch4 6 · ch5 7 · chR 5
    TOTAL validated 46 tasks, 0 problems

Top-level keys are exactly `{schemaVersion, tasks}` in all six; dependency
order and id-uniqueness checked per file. This reproduces the plan's "46 tasks,
zero rejections" claim on the post-ff tree.

## 3. PIN DEPLOY — COMPLETE AND GREEN (and a no-op at the binary level)

Runbook followed: bump → build → switch → adapter smoke. **Probe campaign NOT run**
(see §4 and finding F3).

**Bump.** `~/mecattaf/dotfiles` input `tally` tracks `github:mecattaf/tally.nix` with the
rev pinned in `flake.lock`, so the bump is `nix flake update tally`:
`089d0006` → `84786f4fd211f1006c726b0c1b89f880a062d953`
(narHash `sha256-bqLcbgYxar9kxB+GdGAChHfXYTEP6W2BxUeClY3C9MQ=`).
`git diff flake.lock` afterwards touches exactly one input (`"repo": "tally.nix"`).

**Finding F2 — the dirty lock was not a tally-only bump; it was reverted before the
bump.** The working-tree `flake.lock` arrived carrying a five-input update —
`nixpkgs` (`335f0738` → `fe6deff5`), `flake-parts`, `llm-agents.nix`, `treefmt-nix`,
and `tally`. Deploying that would have been a world rebuild, contradicting D48's
"deploy the validated pin — **first, alone**." I backed the operator's pending lock up,
restored the committed lock, and bumped tally alone.
Backup: `<scratchpad>/dotfiles-flake.lock.operator-pending-backup`.
That pending multi-input update is **still pending** and is the operator's to land.

**Build + switch.** `nixos-rebuild build --flake .#coordinator` exit 0, then
`sudo nixos-rebuild switch --flake .#coordinator`.
System generation **119 → 120** (`2026-08-11 20:56:08`, current).
New toplevel: `/nix/store/k9yp6fg3yzmsyg54fh1pl0khp6bf9xy8-nixos-system-coordinator-26.11.20260723.e2587ca`.
Daemon restarted cleanly: `tally-daemon.service` active (running) since 20:56:17,
status `"tally daemon ready"`. No campaign was armed at switch time
(`tally campaign list` → `[]`), so the "never restart a tally-* unit while a campaign
runs" prohibition was not engaged.

**Finding F3 (important) — the deploy is a genuine no-op at the binary level, and the
validated pin was already live.** After the switch the tally store path is *unchanged*:
`/nix/store/fxn0jycxp2xyyakflw74a2vwk40skxvf-tally-0.1.0`, both before and after, and
the new generation's own `tally-daemon.service` references that same path. Traced to
ground truth rather than assumed:

    $ nix-store --query --deriver /nix/store/fxn0jyc…-tally-0.1.0
    /nix/store/zdqmw9a4ikr42az208y0xa1yf26fypxi-tally-0.1.0.drv
    src = /nix/store/j4x3m0b6ni8n7pfhavc425j3qddxf78d-source
    $ ls $src → Cargo.lock Cargo.toml clippy.toml crates doc drivers examples test
    $ ls $src/drivers → campaign_worktrees.py spec_build_driver.py     # the relocation
    $ ls $src/test/fixtures/spec-build/contract-corpus.json → PRESENT   # added in the 17

The deployed source therefore already contains the 17 commits (it has both the
`drivers/` relocation and `contract-corpus.json`), i.e. the running binary was already
built from ≥`23509ab` — generation 119 (09:31 today) had been switched using the dirty
lock. And `84786f4` builds to the *same* store path as `23509ab` because the package
`src` filter admits only `Cargo.*`, `clippy.toml`, `crates/`, `doc/`, `drivers/`,
`examples/`, `test/` — the docs-only commit (root `*.md` + `silent-factory-worklists/`)
is filtered out entirely. **Consequence: the validated pin is live, the lock is now
honest about it, and no rollback was needed or performed. Zero rollback events.**

**Adapter smoke — PASS, under the policies that actually apply.** `campaign-agent` is
not declared in `home/tally.nix`; it comes from the campaign layer (`tally query pools`
shows it: capacity 4, resource `slot`). Codex's `commitCapableSandboxPolicies` are
`danger-full-access` and `dangerously-bypass`, so a stock smoke would have measured a
configuration nothing runs (assign-tally's explicit warning). Run as:

    tally adapter smoke codex --pool campaign-agent --assert-commit \
      --sandbox danger-full-access --approval-policy never \
      --state-dir /home/tom/.local/state/tally

    {"verdict":"pass","exitCode":0,"verdictState":"PASS","witnessSeq":1869,
     "pool":"campaign-agent","taskUuid":"019ff230-aa33-7232-8d49-690e1d403b7e",
     "captureStatus":"verified","captures":{"finalMessage":"done","sessionRef":"019ff230-…"},
     "commitProbe":{"status":"verified","commits":1,"worktreeStatus":[],
                    "baseRev":"385d4fee…","headRev":"232d27dc…"}}

**Daemon health after the switch** — `tally query pools`: all ten pools `signal: GO`,
`held: 0`, `queued: 0` (build, campaign, campaign-agent, campaign-control, codex-window,
coordinator-gpu, flow, flow-build, local-ai-review, nas-hdd).

## 3b. GATES CLEARED BY THE OPERATOR (2026-08-11, ~21:10)

All three blockers named below were settled by the operator in-session:

1. **Online / forge-native is authorized for tonight.** The offline `forge:"local"` path
   matters after today. The campaign's own master issue is therefore sanctioned mechanism,
   not narration — F4 is dissolved for tonight.
2. **The 02:00 nightly fleet deploy was killed by the operator** for tonight. F5 is retired
   for this run.
3. **Codex quota is not a constraint tonight.**

Prep artifacts written for the continuation, in `aug12-campaign-prep/` (untracked):
`campaign-467-manifest.json` (field-checked against the current contract — no unknown
top-level or task fields, all six gate ids unique, gate decode clean via the driver's
`normalize_campaign_gates`), `master-issue-467-prose.md`, and `issue-467-original-body.md`
(archived *before* any adoption, because `campaign project` overwrites adopted bodies).
The paste-ready continuation prompt is `AUG12-HANDOFF.md`.

**Probe necessity — settled with evidence.** Generations 118, 119 and 120 all carry the
same tally (`fxn0jyc…`), but the most recent campaign #513 closed 2026-08-11 07:58 CEST,
under generation 117's tally (`8n1ihbds…`). So the running binary has **never executed a
campaign**, and campaign-operator §2's probe requirement genuinely applies.

## 4. LAST FORGE-NATIVE CAMPAIGN — NOT STARTED

Board triage was done; arming was not, and the blocker below is a real one.

**Candidate selection (from 15 open issues).** Excluded per instruction and plan:
#519/#520/#521 (owned — and #519/#520 are named in `chR.json`/`ch5.json`); docs issues
#522, #470, #469, #466, #465, #464, #463, #462 (D53 puts *all* documentation work out
of scope); #523 (D55 — standing queue item, and it is a ruling request for the operator,
not implementable work); producers work (Chapter P owns that deletion — none open).

Surviving, owned by no chapter (verified: no chapter table lists them, and no worklist
references them): **#467** `flow: tally flow render — static mermaid extraction from a
checked script`, **#518** `campaign preflight: rehearse checkpoint and gate argvs under
the real resolved hardening tier`, **#468** `pools: provider budgets as admission facts`.

My recommendation, for the continuation: **#467 alone**, plus a gate checkpoint. It is
well-specified with conservative-by-construction behavior and its own acceptance
(renders all `examples/flows/` entries; flake-check smoke), and it is the least collision
with the plan's line-anchored worklists. #518 is the opposite: L-sized and concentrated
in `campaign.rs`/`spec_build_driver.py`, the two files whose line numbers Ch1/Ch2 tasks
cite. #468's cheapest tier is documentation (excluded by D53) and the rest is design-shaped.

**Finding F4 — the blocker, and why I stopped rather than guessed.** `tally campaign arm`
on the deployed pin takes a **GitHub master issue URL as a required positional argument**,
and `--allow-test-local-forge` does not remove it (plan impedance 6: the arm record
demands `issue_url`/`issue_number` until authority v3). So *both* the one-task probe
campaign (step 3's last item) and the forge-native campaign (step 4) require either a
newly created GitHub issue or a synthetic URL local mode will accept. That collides
head-on with the absolute prohibition on "any GitHub issue/comment/sub-issue creation".
Step 4's "arm it forge-native as the skills prescribe" plausibly authorizes the campaign's
own container as mechanism rather than narration — but on a public repo, unsupervised,
that is an outward-facing and hard-to-reverse read of an *absolute* prohibition, so it is
the operator's call, not mine.

A second hazard for whoever arms it: campaign-operator §0 warns that `tally campaign
project` **overwrites the title and body** of every issue it adopts. #467's body is
substantial and carefully written; it must be archived before adoption.

**#455 machine-steering question: UNCHANGED / still open.** No campaign ran, so no
steering weather was observed. Per the instruction this is a valid outcome to record;
no storm was manufactured.

## 5. CHAPTER-1 DECLARATION — NOT WRITTEN

Step 5 was gated on step 4 being green. Not reached.

---

## Findings for triage (filed here as text, nothing on GitHub)

- **F1** — ff blocked by a stale untracked `AUGUST-10-LEARNINGS.md`; resolved, backup path above.
- **F2** — the dotfiles `flake.lock` carried an unrelated four-input update (incl. `nixpkgs`) alongside the tally bump; reverted and backed up, still pending, operator's to land.
- **F3** — the "validated pin" was already deployed (gen 119); tonight's bump is docs-only and filtered out of the package `src`, so it produced an identical store path. The deploy is honest bookkeeping, not a code change. Zero rollbacks.
- **F4** — `campaign arm` requires a GitHub issue URL even under `--allow-test-local-forge`; probe and forge-native campaigns are both blocked on the issue-creation prohibition. **This is the decision that unblocks the rest of the night.**
- **F5 — deploy-collision risk, live.** `tally-producer-nightly-fleet-deploy.timer` fires at **02:00 CEST** and runs `sudo systemctl --wait start fleet-deploy.service` (pools `build`, `coordinator-gpu`, `flow-build`). A fleet deploy at 02:00 would move the pin under any campaign still running, which is exactly the "never move the substrate mid-campaign" hazard. Any campaign armed tonight should either finish before 02:00 or the timer be considered first. I did not touch the timer (that would be a config change and a unit restart).
- **F6** — `~/.config/tally/config.json` still carries `gitAi: {enable: true, mode: "advisory"}`. Consistent with D31 removing git-ai in Chapter 2, but noting that the deployed config still enables it, and AUG11-evening-pass records the checkpoint feed on this host as dead (48 empty notes). Descope, not repair — no action taken.
- **F7** — the tally package `src` filter excludes root `*.md` and `silent-factory-worklists/`. Worth knowing: plan/worklist commits will never change the tally store path, so a worklist-only change needs no redeploy — but it also means `readFirst` pointers into `SILENT-FACTORY-PLAN.md` resolve from the *checkout*, not from the pinned package.

## Prohibitions — all honored

No GitHub issue/comment/sub-issue creation. No hand edit to tally source, nix modules,
skills, or tests. No second fleet deploy. No tally-* unit restarted while a campaign ran
(none ran; the switch's restart happened with zero campaigns armed). No tags, releases,
or workflows. No scratch state read back into any ledger. One `git push` — the sanctioned
plan commit.

---

# CONTINUATION — overnight steward, from ~21:45 CEST (execution phase)

The prep record above stands. This continuation executed under the operator's four
21:40 CEST corrections (paste-prompt), which override AUG12-HANDOFF.md where they
disagree. Established at session start and honored throughout: **sodimo/os#19**
(registrationId `019ff234-d58b-79b0-823c-24b5bf3083f5`, armed 21:02 CEST) is the
operator's own campaign, holds the `campaign` pool, and was not touched, steered,
commented on, or treated as a finding.

## 6. THE POOL QUESTION — RULED FROM CODE: queued time is FREE

Question (correction 2a): does time queued for a pool burn against the campaign's
`driverRuntimeMaxSec` (900 s in the manifest), or does that clock start at dispatch?

**Answer: the clock starts at dispatch. Queued time burns nothing.** Three independent
facts from the admission and budget paths, each with file:line:

1. **The only job that requests the `campaign` mutex is the pass runner, and its
   budget is `runtimeMaxSec`, not `driverRuntimeMaxSec`.** The dispatch payload sets
   `pools: ["flow", manifest.pool]` with `runtime_max_sec: manifest.runtime_max_sec`
   (86400 here) — `crates/tally/src/cli/campaign.rs:2204,2237`. `driverRuntimeMaxSec`
   reaches only the driver nodes via the brief (`campaign.rs:2184`), and those run in
   `pools: ["campaign-control"]` (`examples/flows/spec-build.js:1370-1385`, and the
   continuation node comment at `campaign.rs:2187-2190`) — capacity 4, free tonight.
   Driver nodes do not exist until the runner is already running, so no driver clock
   can overlap mutex-queue time at all.
2. **A queued job has no process.** `LeaseEngine::admit_at` returns
   `AdmitOutcome::Queued` when the pool is full (`crates/tally-core/src/lease.rs:
   586-615`); the daemon leaves the job in `JobState::Queued` and only a later lease
   grant promotes it to `Running` and calls `spawn_execution`
   (`crates/tally-core/src/daemon/run.rs:880-882, 887-900, 919-931`).
3. **Both runtime-budget enforcement mechanisms start at process launch.** Systemd
   backend: `RuntimeMaxSec={n}s` is a unit property applied at launch
   (`crates/tally-core/src/executor/launch.rs:41-43`). Direct backend: a tokio
   timeout wrapping `child.wait()` after spawn
   (`crates/tally-core/src/executor/lifecycle.rs:1078-1079`).

Corroborating: the lease queue has no eviction — the aging threshold only raises a
long-waiter's effective priority rank (`lease.rs:1056-1058`), and the co-residency
predicate's grant rule is simply `holders < capacity` (`lease.rs:1172`), so a second
campaign's runner queues indefinitely rather than being refused.

**Consequence: path (b) of the correction was taken. No `nixos-rebuild switch`, no
daemon restart, no pool override, no module change. Campaign #467 was armed into the
queue behind sodimo's mutex; both campaigns get their night without overlapping.**
The path-(c) switch authorization was not used.

## 7. FLEET-DEPLOY KILL — VERIFIED EFFECTIVE (correction 4; neither timer touched)

The armed-looking timer is expected and harmless. The kill is a drop-in on the
*service*: `~/.config/systemd/user/tally-producer-nightly-fleet-deploy.service.d/
skip-2026-08-12.conf` sets

    ExecCondition=/run/current-system/sw/bin/sh -c 'test "$(date +%s)" -ge 1786579200'

- `1786579200` = **Wed 2026-08-13 02:00 CEST**. The Wed 2026-08-12 02:00 firing runs
  at epoch `1786492800`; the test fails; systemd skips `ExecStart` (an ExecCondition
  exit in 1–254 marks the unit skipped, not failed). The dispatch never runs.
- The condition self-expires: at the Aug 13 02:00 firing `now >= threshold` holds and
  the deploy proceeds normally; the Thu 03:00 cleanup timer only removes the file.
- Verified *loaded*, not just on disk: `systemctl --user show` lists the drop-in in
  `DropInPaths` and the condition in `ExecCondition`.
- **Witnessed at the firing itself (02:00:02 CEST, Aug 12):** journal —
  `tally-producer-nightly-fleet-deploy.service: Skipped due to 'exec-condition'`;
  unit state `Result=exec-condition`, `ConditionResult=no`; and
  `fleet-deploy.service` shows `ActiveState=inactive` with **no**
  `ExecMainStartTimestamp` — the deploy dispatch never ran. The pin did not move
  under the live campaign. The kill is confirmed by observation, not just by
  mechanism reading.

## 8. PROBE DECISION — RETIRED AS A CAMPAIGN, NARROWED TO A REHEARSAL (correction 3)

The handoff's STEP A rationale ("fxn0jyc has NEVER executed a campaign") is false by
direct observation. Live evidence at ~21:45 CEST:

- `tally campaign list`: sodimo/os#19 armed 21:02 CEST, flow =
  `/nix/store/fxn0jyc…-tally-0.1.0/share/tally/flows/spec-build.js` — the deployed
  pin's own flow asset.
- `systemctl --user list-units 'tally-job*'`: the sodimo **runner**
  (`tally-job-019ff24b-4920-…`: `tally flow run …fxn0jyc…/spec-build.js
  --max-nodes 39`) and a **live codex implementation lane**
  (`tally-job-019ff234-…-crm-pipeline-…`: `codex exec … --sandbox
  danger-full-access` in a campaign workspace worktree), both running the fxn0jyc
  binary.

So arm → projection → reconcile → dispatch → codex-lane execution — the exact
mechanism chain campaign-operator §2's probe exists to exercise, including the very
policies #467's manifest sets (`danger-full-access`, `approval never`) — is being
proven on this pin, this daemon, this host, right now, by a real campaign. Reasoning
recorded per the correction:

- **Retired as a separate campaign.** A probe would buy no mechanism information the
  estate does not already have live; it would cost one more real GitHub issue (an
  outward act permitted only as mechanism), and its runner would queue *ahead of*
  #467 on the mutex, delaying the mission for a duplicate answer. Its "ten minutes"
  premise is void while sodimo holds the mutex — it could not have run promptly
  anyway.
- **Narrowed residue executed directly.** What sodimo's run cannot prove is
  tally.nix-repo-specific: the cheap gates on this checkout. Both were run verbatim
  (argv byte-identical to the manifest) in the checkout: `fmt` preflight + argv →
  PASS; `no-stubs` preflight + argv → PASS. The remaining gates (deny/tests/clippy)
  are #472-proven shapes and run first on a pristine fetched base as gating
  preflights inside the campaign itself before any agent dispatches — a mechanism
  probe of exactly the right scope, at zero extra forge cost.
- Not skipped silently, not run mechanically: this section is the record.

## 9. CAMPAIGN #467 — ARMED AS #527, QUEUED BEHIND SODIMO (STEP B)

Sequence per the skills, all mechanism, nothing hand-authored:

1. **Master issue created with prose only**:
   <https://github.com/mecattaf/tally.nix/issues/527>, body =
   `aug12-campaign-prep/master-issue-467-prose.md` (title line lifted to the issue
   title).
2. **`tally campaign project --repo mecattaf/tally.nix --issue …/527`** with a
   worklist document composed from the prepared manifest: `campaign` object =
   manifest minus tasks; one task `flow-render` adopting `issue: 467` with a full
   brief. (`project` requires task briefs in the worklist — the prepared
   reference-shaped manifest alone is not its input; the composed document and the
   brief are preserved as `aug12-campaign-prep/project-worklist-467.json` and
   `aug12-campaign-prep/flow-render-brief.md`.) The brief folds in the **archived original #467 body
   verbatim** (campaign-operator §0), a **tree-state section** (self-hosting §8:
   verified no `render` subcommand and zero mermaid references in `crates/` at
   84786f4 and in the pin — greenfield, nothing already done), the **self-hosting
   notice**, and a D53 note scoping the doc-example line out.
   Result: `{"issue":"…/527","tasks":[{"id":"flow-render","issue":467}],"merged":[]}`.
3. **Verified before arming**: #527 carries prose intact plus correctly paired
   `tally:campaign:v1`/`:end` and `tally:campaign-worklist:v1`/`:end` markers,
   label `tally-campaign`; #467 title preserved, body now the brief (with archive
   folded in). Body frozen from this point; any further human word is a comment.
4. **`tally campaign arm https://github.com/mecattaf/tally.nix/issues/527`** →
   `disposition: created`, runner task_uuid `019ff261-03bb-7c72-a7fb-35a710c66840`,
   `state: "queued"`, attempt 1, projection native-sub-issues.
5. **Queue posture witnessed**: `tally query pools` → `campaign: held 1, queued 1,
   STOP` (sodimo holding, #467's runner waiting); `flow: held 1, queued 1, GO`.
   `tally campaign list` shows both registrations (sodimo/os#19 armed 19:02 UTC,
   mecattaf/tally.nix#527 armed 19:51 UTC).

**Arm-time warnings, assessed and accepted (not a defect):** arm printed ten
warnings — every gate's nix/cargo argv lacks an in-argv cache redirect and "may fail
under the resolved adapter's hardened tier". Read the source: this is an
unconditional static argv lint (`argv_hazard_warnings`,
`crates/tally/src/cli/campaign.rs:1797-1810`); it does not consult the host's
resolved tier. The deployed config declares **no hardening preset on any adapter**
(`~/.config/tally/config.json`; compatibility default constrains nothing), #472 ran
this exact gate shape green on this host, and sodimo's campaign is running under the
same config now. Advisory noise on this host; a real constraint to carry into any
future hardened-tier estate.

## 10. STEP C — CHAPTER-1 DECLARATION WRITTEN (not deployed)

Ready diff at **`aug12-campaign-prep/ch1-module-declaration.diff`** (untracked):
`campaigns.silent-factory-ch1` for `~/mecattaf/dotfiles/home/tally.nix` —
`enable = false`, worklist `silent-factory-worklists/ch1.json`, maxTasks 6,
maxParallel 2, codex agent under the #472/#527 policy set, the six-gate ladder,
dedicated `pool.name = "campaign-ch1"` (installed additively by the mutexPools fold;
the ad-hoc `campaign` pool untouched). Two seams documented in the diff header:
`renderCampaignRepositories` hardcodes `forge = "github"`
(`nix/modules/common.nix:3619`), so forge:"local" does not render until the
Chapter-2 authority work lands — the declaration documents intent and shape for
morning review; and pre-v3 local campaigns need `--allow-test-local-forge` (plan
impedance 6).

## 11. CAMPAIGN #527 PASS 1 — COMPLETE AND CLEAN (one pass, ~52 min, zero interventions)

Timeline (CEST): runner dispatched **22:09:16** (sodimo's pass released the mutex;
queued wait ≈ 2h18m from the 19:51 UTC arm, burning nothing per §6); pass ended
**23:01:48**.

Witness evidence, per claim:

- `tally query run 019ff261-03bb-7c72-a7fb-35a710c66840` → **`complete`**,
  `Tasks: 1 done, 0 running, 0 blocked, 0 pending`; task row `flow-render` → done,
  pointing at <https://github.com/mecattaf/tally.nix/pull/528>. **30 attested of 30
  expected attempts over 30 member tasks** — every node of the pass carries an
  attestation. Usage: 29.8M tokens (334k fresh input, 29.4M cache read, 60k output).
- Forge state: **PR #528 MERGED** at 21:00:59Z (23:00:59 CEST), base `main`, squash
  commit `13307c67`, +1180/−4 across 10 files; `origin/main` now
  `13307c67 flow-render: flow: tally flow render — static mermaid extraction from a
  checked script` atop `84786f4`. **Issue #467 CLOSED** by the merge.
- The full gate ladder (forbidPaths, fmt, no-stubs, deny, tests, clippy) ran green
  in the lane by construction — the merge node only integrates behind witnessed
  gates, and the merged PR is the receipt. The arm-time hazard warnings (§9)
  produced zero failures, as predicted for this no-hardening host.
- Zero interventions: no steering comment, no re-trigger, no re-arm, no human word
  on any issue. The supervision record is exactly two read-only polls (dispatch
  detection, completion detection) plus this query evidence.
- Sodimo's campaign took the mutex back immediately after our pass
  (`campaign: held 1` again at 23:02, with the next queued runner behind it) — the
  two campaigns interleaved on the mutex exactly as §6 predicted, no overlap, no
  contact between them.

Master issue #527 remained OPEN until the campaign's terminal continuation pass
(queued behind sodimo's current pass; continuation event written by pass 1 before
exit — `campaign-continuation` drain shape, common.nix:3894) ran and completed at
**23:22:36 CEST**: it posted the machine receipts summary
(`tally:campaign-complete:v1`, "Settled 1 of 1 task(s) against durable
merge/checkpoint facts", worklist `sha256:10a75bd2…` at `13307c67`, merged list =
`flow-render` → PR #528) and **CLOSED #527**. The campaign lifecycle is complete
end to end: create → project → arm → queue (free) → pass 1 (dispatch, gates,
publish, squash-merge) → terminal pass (receipts, close). After the close the
mutex went straight back to sodimo's queue (`campaign: held 1, queued 1` — all
sodimo's from here).

**#455 machine-steering question: STILL OPEN — clean-run outcome.** The campaign
generated no failure weather: no failed node, no diagnosis, no steering thread. Per
the handoff this is a valid outcome, recorded as such; no storm was manufactured.

## Findings from the execution phase (text only, nothing filed)

- **F8** — `tally campaign project` requires task briefs in its worklist input (task
  `body` or goal/deliveredBehaviors/acceptanceCriteria; `campaign.rs:2827-2955`);
  the prepared reference-shaped manifest is the *output* contract, not `project`'s
  input. First attempt exited 2 ("worklist requires a campaign object or
  --campaign-config"); resolved by composing `{schemaVersion, campaign, tasks}` with
  the brief inline. Worth one line in the campaigns doc when the docs round comes.
- **F9** — arm-time gate-argv hazard warnings are static lint, not tier-aware (§9
  above). On a no-hardening host they are pure noise; if a hardening preset ever
  lands on this estate, every nix/cargo gate argv needs in-argv XDG cache redirects
  (campaign-operator §3b's cure).
- **F10** — the module cannot yet express forge:"local" (`common.nix:3619` hardcodes
  github at render). Expected by the plan (authority v3 / 2.9), recorded here so the
  morning review of the ch1 diff has the pointer.

## 12. EXECUTION-PHASE CLOSEOUT — morning triage summary

**What completed tonight** (all witnessed above): the queued-time ruling from code
(§6, path b — no deploy, no restart, no pool change); the fleet-deploy kill
verified effective without touching either timer (§7); the probe retired on live
evidence with its residue run verbatim (§8); campaign #527/#467 armed, queued free,
executed, merged (PR #528 → `13307c67`), receipts posted, closed — one pass, zero
failures, zero steering, zero interventions (§9/§11); the Chapter-1 declaration
ready-diff written, not deployed (§10).

**Rollback events: zero.** No `nixos-rebuild`, no generation change (still 120),
no tally-* unit restarted, no pin movement, no flake.lock change. The operator's
pending five-input lock update (F2) remains untouched and pending.

**#455 machine-steering: still open** — clean-run outcome, no weather (§11). The
question keeps waiting for a storm on a substrate with campaign-hours; tonight
added ~52 minutes of clean forge-native campaign-hours on pin `fxn0jyc`.

**Sodimo/os#19: untouched throughout.** Interactions were limited to reading
shared pool counters and unit listings; no comment, no steer, no query against its
run ids beyond `tally campaign list`. It held/holds the mutex before and after our
window; its weather (if any) is not reported here by instruction.

**GitHub contacts, complete list (all sanctioned mechanism):** issue #527 created
(campaign master, prose only); `campaign project` writes to #527 and adopted #467
(archived first, evidence folded back); native sub-issue relation; `campaign arm`;
the machinery's own PR #528 + squash merge + #467 close + receipts comment + #527
close. Zero narration, zero tracking issues, zero steering comments, zero labels
beyond `project`'s own, zero tags/releases/workflows.

**Untracked artifacts for morning review** (paths, all inside the repo checkout):
`aug12-campaign-prep/ch1-module-declaration.diff` (STEP C ready-diff),
`aug12-campaign-prep/project-worklist-467.json` + `flow-render-brief.md` (the
exact projected input), plus the pre-existing prep artifacts. Local `main` is
deliberately left at `84786f4` (one behind `origin/main` = `13307c67`) — no reason
to mutate the daemon-serving checkout under a live sodimo campaign; a morning
`git pull --ff-only` closes it.

**Registration left armed** — `tally campaign list` still carries
mecattaf/tally.nix#527 alongside sodimo/os#19; the master is closed and settled,
so its poll passes are no-ops. Disarming (or not) is the operator's call at
coffee; nothing depends on it tonight.

---

# DAY RUN — 2026-08-12, full silent-factory chain (operator-authorized)

Operator instruction (morning, post-compact): "run silent-factory chain WITH
TALLY … and supervise the run." Continuation brief: AUG12-DAYRUN-HANDOFF.md.
Chain: ch1 → ch2 → chP → ch3 → ch4 → ch5, one forge-native campaign per
chapter via the proven #527 pattern, strictly sequential, zero intervention
while healthy.

## 13. CHAPTER 1 — ARMED AS #529 (07:21 CEST), QUEUED BEHIND SODIMO/OS#43

- Local `main` fast-forwarded `84786f4` → `13307c67` before composing.
- Master issue **#529** created, prose = plan Part 3 Chapter 1 table verbatim
  + line-drift note + self-hosting notice + scope note
  (`aug12-campaign-prep/master-issue-ch1-prose.md`).
- Projection input `aug12-campaign-prep/project-worklist-ch1.json`: campaign
  object = 467 manifest minus tasks, `name silent-factory-ch1`, `maxTasks 6`,
  `maxParallel 2`; tasks = `silent-factory-worklists/ch1.json` verbatim, with
  one additive fix (below).
- **Finding F11 (extends F8):** `campaign project` requires a brief body for
  EVERY task including checkpoints — `render_project_task_body`
  (campaign.rs:2827-2838) accepts `body` or the goal-trio, and checkpoint
  tasks carry neither in the worklist schema. First attempt exited 2
  ("tasks[5].goal must be a non-empty string"). Fixed by adding a short
  `body` to the checkpoint task in the projection input only; the committed
  ch1.json worklist file untouched.
- `project` → sub-issues **#530–#535** (5 implementation + chapter-gate);
  markers verified paired, prose intact, six checkboxes.
- `arm` 07:21:22 UTC+2 → registration `019ff46b-0733-7eb0-8421-5bdc6b7b4c65`,
  runner `019ff46b-0949-7c33-a74d-869a7f879b88`, `state: "queued"`. The ten
  cache-redirect warnings are the known F9 static lint; advisory on this host.
- Pool at arm: `campaign` 1 held (sodimo/os#43, armed 01:05 UTC) / 2 queued.
  Queue time is free (§6 ruling); the runner waits at zero budget cost.
- Watcher: background poll (120 s) on `tally-job*` user units (fixture units
  excluded per the §11 unsound-journal-filter finding) + #529 state; exits on
  master close.

## 13b. CHAPTER 1 DISARMED BY OPERATOR INSTRUCTION (07:5x CEST, pre-dispatch)

Operator: "disarm the tally campaign. i need to make some major changes to it
at this point." Sequence, all verified:

- `tally campaign disarm .../issues/529` → `{"disarmed": true}`; registry now
  carries only sodimo/os#43.
- The armed runner job survived the disarm still queued; `tally flow cancel`
  was a no-op (affected 0) — the right verb is **`tally queue cancel
  <task_uuid>`** → affected 1, was "queued" (finding F12: disarm does not
  reap the queued runner; queue-cancel by task uuid does).
- Pool after: `campaign` 1 held / 1 queued — both sodimo's. Nothing of ours
  runs or queues. The campaign never dispatched; zero lanes started, zero
  commits, zero merges, zero budget burned (queue time free, §6).
- GitHub state left as-is for the operator's rework: master #529 OPEN with
  projected worklist markers, sub-issues #530–#535 OPEN. A re-arm after
  changes will need a fresh `project` if the worklist changes.
- Chain paused before Chapter 1; nothing else was ever armed.
