# History replay — adversarial verification of radical deletion against the
# complete Aug 7–14 record, and the band-aid audit of EPSILON-EXTENSION.md

Written 2026-08-14, independently of the other design agent, against
`mecattaf/tally.nix` at `e921cccc`. Sources read at the primary: the three
excavation reports (PA-01…47, VD-1…31, CA-1…14), `EPSILON-EXTENSION.md`,
`AUG13-RUN.md` (incl. CORRECTIONS), `AUG14-LEARNINGS.md`,
`AUGUST-12-LEARNINGS.md`, `AUG12-overnight.md` (F1–F10),
`AUGUST-10-LEARNINGS.md`, the three aug-4 notes, both wayfinder documents, the
epsilon receipts ledger and steering logs re-parsed directly, and the tree
(`campaign_contract.rs`, `actions.rs`, `campaign_registry.rs`,
`examples/flows/spec-build.js`, `silent-factory-worklists/epsilon.json`).

**The one-sentence answer.** The plan is directionally right and roughly half
of it is genuine cause-deletion — but its three autonomy mechanisms (auto-pardon,
capped auto-grant, needs-grant channel) each mechanize a symptom whose cause the
record proves deletable, and one of them (auto-grant) would make the campaign
machinery a **writer of the authority file**, weakening the plan's own invariant 1;
meanwhile the caps replay shows **no authored cap fired correctly in the entire
recorded history**, and the receipts ledger shows **machine-steered attempt-2
converted 0 of 9 episodes** — the unit of retry is new information (an epoch),
not an attempt count.

---

## 0. The design under test

The maximally-deleted design, as specified for this replay, with the one
sharpening each clause needs to be replayable:

- **D1 — no pardons → epoch-scoped budgets.** An attempt receipt counts against
  a task's budget only when its epoch equals the task's current epoch. A task's
  epoch is derived from durable facts: the committed worklist bytes that touch
  the task (`worklistSha256` + task content), the steering high-water mark
  addressed to it, and the set of its merged dependencies. Any change to any of
  these is a new epoch; old-epoch receipts stop counting **by derivation, not by
  a clearing act**. Latched = current-epoch budget exhausted. The substrate is
  exactly ext0's `receipt-authority-stamp` fields.
- **D2 — no prospective refusal → retrospective certification + live-collision
  check.** Lanes commit whatever the task requires in their isolated worktree;
  the ownership node certifies the committed paths; the tree-delta gate allows
  the certified set **minus a static authority deny-list** (see §4); it refuses
  only a deny-list path or a path a live sibling has certified-but-not-merged.
  Declared `conflictDomains` survive unchanged as the scheduler's disjointness
  input. This is not an invention: it is the flow's own shipped serial-task
  path, verbatim at `examples/flows/spec-build.js:2197` — *"This serial task
  omits conflictDomains. Ownership will certify its committed paths, and the
  tree-delta gate will allow exactly those owned paths after ownership runs."* —
  and the gate self-describes as *"detective, not preventive"* (`:3001-3005`).
- **D3 — no arm/disarm → lease + poll admission.** One deliberate `run` per
  campaign identity opens a lease; `poll` re-admits whenever the committed
  worklist sha at the base moves; `complete` + quiescent ⇒ the lease lapses;
  receipts, refs, and the steering log outlive it; `release` reads durable
  state only.
- **D4 — no authored caps → derived/backstop limits.** Concurrency =
  min(disjoint frontier, host capacity); hard backstops stay hardcoded
  (`MAX_CAMPAIGN_TASKS = 128`, `campaign_contract.rs:22`; campaign
  `runtimeMaxSec` default 86 400, `:902-904`; the attempt-stop). Nothing
  numeric on the authoring surface.
- **D5 — no narration slot.** The squash layer adopts the lane tip's own
  subject when valid (PA-25), else the template. No steward model call.
- **D6 — no latch → notification + budget.** The escalation report (PA-43,
  kept verbatim) becomes a push notification; dispatchable siblings keep
  moving; only current-epoch budget exhaustion blocks a task and its
  descendants; a hard lifetime cap per task backstops adversarial epoch churn.

One deliberate non-deletion: **first arm stays a verb** (`run`). "No arm/disarm"
replayable means no *re*-arm and no disarm; a campaign must still start on a
human act, or any worklist committed for review would self-execute.

---

## 1. The band-aid audit of EPSILON-EXTENSION.md

Question asked of every item: does it **DELETE THE CAUSE** or **MECHANIZE THE
SYMPTOM**? Where the plan mechanizes, the root version is named. Where
mechanization is correct because the concept is load-bearing, that is said too.

### ext0 — ten tasks

| task | verdict | the root version |
|---|---|---|
| `receipt-authority-stamp` | **ROOT SUBSTRATE — keep, re-purpose** | The plan sells it as turning a pardon assertion into a `<` comparison. Under D1 there is no pardon to compare for: the stamps **are the epoch key**. Same bytes, bigger job. |
| `auto-pardon-authority-delta` | **BAND-AID** | Widening `amendment_pardon_plan` (`campaign.rs:4134`) keeps pardon-state alive as mutable state that a machine now mutates. The root is D1: the counter is a pure function of (receipts, current epoch). The difference is not cosmetic — F37's stale-pass race and the pardon-race (receipts 15, 19) **cannot exist** under derivation, because an old-epoch receipt simply never counts; under auto-pardon they still exist and still each burn an attempt before the machine pardons them. Three of epsilon's ten pardons were exactly these races. Delete the concept; keep `resume` nowhere — the manual override is a steer, which bumps the epoch (proof: pardon 7's own reason, *"Pardoned … **after steering it** to commit before verification"* — the judgement was in the steer; the pardon was transcription of "I steered, now unlatch"). |
| `summary-ref-stage-digest` | **ROOT** | Deletes F38/VD-4/PA-17's cause; correctly refuses to build the archive verb. The strongest item in ext0. |
| `needs-grant-outcome` | **BAND-AID AS SCOPED; survives narrowed** | A refusal channel is only needed where refusal is the right behaviour. Under D2, ordinary-path refusals — 4 of 4 in the epsilon record, all false positives against correct work (CA-1) — stop happening: the lane does the work. What remains genuinely refusable is the **authority deny-list** (worklist file, `campaign.gates`, `test/fleet-gate.sh`, `.github/`), where a structured `needs-authority` outcome is right and rare. Build the sixth `failureClass` for *that*, priced free, and VD-2's `projectionWaitMs` question dissolves with the volume. |
| `narrator-honest-contract` | **BAND-AID (polishing an unproven component)** | 0 accepted in 35 merges, 74 rejections (F32); the aug-4 rule is exact: *"A process artifact may exist only when it is a hard gate for a named feature."* The narrator gates nothing. The root is the next row plus stating the grammar **in the lane prompt** — the lane agents already wrote 10 valid conventional subjects unaided (PA-25). E5's keep-and-measure is defensible only if `squash-subject-adoption` lands first and the narrator only fires when the lane subject is invalid; otherwise this is paying to improve a component whose absence was never noticed. Side with CA-12 over E5. |
| `squash-subject-adoption` | **ROOT** | Deletes PA-25's cause (the system discarding correct answers it already holds). |
| `final-bar-executes` | **ROOT** | A check asserting a `--list` listing (`flake.nix:3465-3471`) is the system committing aug-4 pattern 2 (proof-class inflation) against itself. Honest surface or deletion — correct either way. |
| `fleet-gate-cheap-first` | **ROOT-ADJACENT** | Reordering is hygiene; the cause-deletion is the generalized local-audit arm (no more PR fodder, PA-36). One deeper cut the plan does not name: the changelog-policy stage contradicts the Aug-11 ruling *"no releases by default, `git log --oneline` is the changelog"* (PA-24). Whether the stage should exist at all for local heads is a one-word E-decision the plan should carry. |
| `authoring-doctrine-skills` | **ROOT, bounded** | The doctrine change is the measured winner (PA-34: corrections 41 % → 11 %; F40: the lint caught 0 of 4). Committing it to the two files where authoring happens is the anti-meta-trap form: it gates a named capability and freezes. |
| `chapter-gate` | **KEEP** | CA-11; the gate ladder is D61's "in". |

### ext1 — five mechanisms

| mechanism | verdict | the root version |
|---|---|---|
| poll re-admission | **ROOT** | Re-admission *is* the deletion of re-arm: authority is committed bytes; reading their sha is derivation. Non-negotiable rider: auto-admission makes the worklist a live control surface, so lanes must be categorically unable to write it (§4, I1) — otherwise a merged lane edit self-authorizes on the next poll tick. |
| capped auto-grant | **BAND-AID, and an attack surface** | It keeps the refuse→grant→re-admit→(auto)pardon loop alive with a robot at the crank, and it makes the campaign machinery a **committer to the authority file**. The plan's invariant 1 is *"authority is committed bytes … that the constrained party cannot write"*; CA-6 threads that needle by saying the machinery, not the lane, writes — but the five-clause cap is then **enforced by the same code that performs the act**, which is the wayfinder-Notes hole one level up (constraint and exemption in the hands of one party). The root is D2: retrospective certification deletes the grant concept for ordinary paths, the machine's widened-domain record lives in **its own receipts and refs**, and the authority file stays human-authored bytes forever. On the record this loses nothing: all four grants were correct work refused (CA-1); two were the agent dictating the text the operator retyped (`1324eaa4`, `05aec25d`: "granted verbatim"). What survives of "grant": authoring an amendment task that *prospectively* names an authority-surface path — which is worklist authoring, the verb that stays human. |
| escalation latch → notification + budget | **ROOT** | Matches D6. The classifier's job shrinks under D2 (refusals mostly vanish), leaving crash-vs-impossible — a small-model task, intern-test clean. Keep the report verbatim (PA-43). |
| release from durable state | **ROOT** | Deletes PA-16's three-jaw trap and PA-22's intolerant lookup. Correct and necessary regardless of E4. |
| `campaign publish` (mechanized rebase + re-gate) | **MECHANIZATION CORRECT — concept load-bearing** | The deeper delete exists — absorb `main` into the integration branch at every admission boundary, making proven ≡ published by construction — but it rewrites merged lane history mid-campaign and the bridge oracle matches on whole-tree ids (`campaign.rs:2034-2056`), so continuous rebase breaks proof continuity in ways the record cannot price. Under D2 the divergence source shrinks anyway (no grant commits; only authored amendments). The **re-gate is the non-negotiable half**: the published sha has never been gated, three stages running (CA-8), and `8b45283` is not an ancestor of `origin/main` today (PA-21). Ship as planned. |

### ext2 — six items

| item | verdict | note |
|---|---|---|
| completion-identity unification (E1) | **ROOT — the most important item in the plan** | VD-8: two live contracts; `Exact` unreachable; every release bridges. Execution policy out of the identity is right, and under any autonomy design it is **required**, because automated authority changes make PA-14's rotation constant rather than occasional. Second-order gain nobody has written: under D2 grant-amendments drop to ~zero, so fewer rotations even before E1 lands. Ordering pressure: every ext1-era release proves at bridge grade; if the close condition ("proves … through the **exact** oracle") is serious, pull E1 as early as the tree allows, and persist `completionProofs` + the plan document (PA-15's unverifiable `planSha256`) immediately. |
| estate population replay | **ROOT** | The only fleet-down class (F39) gets standing coverage; sampled population, not the one hand-edited row (VD-14). Also the gate that keeps **deploy** honest — the one human act the deleted design leaves untouched. |
| probe honesty | **ROOT** | PA-20's verdict conflation + missing preflight + self-defeating GC. |
| test isolation guard | **ROOT** | VD-19/F25 class. |
| lease semantics (E4) | **ROOT** | This is D3, and it is the design's own lineage — the aug-4 pls note: *"when the task finishes (or crashes), pls reclaims the lease"*; *"a supervisor restarts it from a known-good clean slate."* D52 filed it a week ago. It also silently fixes PA-04: no disarm ⇒ the steering ledger survives the whole campaign (the taskdb-flood insight died at a stage boundary in epsilon). |
| off-host steer (E8) | **UPGRADE FROM OPTIONAL TO CORE** | Under the deleted design the runtime human surface is exactly: steer, and approve-at-hard-cap. D12 said it in July: *"a steer verb usable only at the coordinator's keyboard is a regression against a phone."* If the intern test is the criterion, the one judgement verb must reach the intern's phone; this is not garnish, it is the surface. |

**Decisions:** E1 yes (earlier if possible) · E2 **no — replace with D2** · E3 yes ·
E4 yes · E5 no (delete-until-proven, after subject-adoption) · E6 yes (PA-01 is
the project contradicting its own thesis) · E7 yes · E8 **yes, core**.

---

## 2. The total replay

Every recorded defect and intervention, replayed under D1–D6.
Columns: (a) still caught — by what, at what cost; (b) new failure mode the
deleted ritual was silently preventing; (c) net verdict.

### 2.1 The F-ledger

| F | what happened | (a) under the deleted design | (b) new exposure? | (c) |
|---|---|---|---|---|
| F1 stale untracked file blocked ff | git hygiene | E6 commits the records; cause gone | none | SAFE |
| F2 unrelated lock update | deploy hygiene | unchanged (deploy is human) | none | SAFE |
| F3 no-op deploy honesty | bookkeeping | unchanged | none | SAFE |
| F4 arm demanded a GitHub issue | already deleted by local-first | — | none | CLOSED by deletion (precedent) |
| F5 02:00 fleet-deploy collision | D63 `quiescent` ExecCondition shipped | same verb kept | none | SAFE |
| F6 stale `gitAi` config | purged in ch2 | — | none | CLOSED |
| F7 src filter excludes worklists | authoring fact | unchanged | none | SAFE |
| F8 project required inline briefs | projection deleted with GitHub mode | — | none | CLOSED by deletion |
| F9 arm-time lint noise | tier-aware lint shipped ch0 | unchanged | none | CLOSED |
| F10 module can't express forge:local | D77 **deleted the module path** | — | none | CLOSED by deletion (the pattern's proof, F27) |
| F11 checkpoint body workaround | retired by ch0 | — | none | CLOSED |
| F12 | **no definition found in any surviving record** — the numbering jumps in `AUG12-overnight.md` from F10 to `AUG13-RUN.md`'s F13a | — | — | INSUFFICIENT (say so, don't invent) |
| F13a supervisor liveness predicate wrong (1 h 54 m dead air) | the latch was **silent**; under D6 exhaustion pushes a notification; the hand-rebuilt watcher (PA-11) retires | a dead notification channel = silent stall → the poll heartbeat must alarm on "blocked work + quiescent" (one predicate, exists) | SAFE, improved |
| F13b argv defect ×7 worklists | gate fails attempt 1, diagnosis, notification; fix = worklist commit; **poll re-admits, epoch bumps, dispatch resumes** — no resume verb, no 1 h 54 m | none | SAFE |
| F13c disarm+re-project+re-arm does **not** clear counters; "running" while dead | **cause deleted**: no disarm, no counters-as-mutable-state; the argv amendment is a new epoch, dispatchable by derivation | none | SAFE — this finding is the single best evidence *for* epoch-scoping: the operator was trapped precisely because pardon-state is state divorced from authority |
| F14/F21 flake-only assertions merge green | unchanged by deletions; ext0 gate template (clippy + **built** check subset) is the fix (VD-6/7) | none | SAFE (fix is authoring, not ritual) |
| F15 `!` gag | carve-out shipped (`2d68fca9`); VD-13 killed the ghost | none | CLOSED |
| F16 checkpoint has no worker | repair = amendment task (F28 pattern). Stays a **human-approved** worklist commit — but PA census row 7 shows the machine held the exact text 4/4 times, so the act is approve-a-rendered-diff: intern-shaped, phone-shaped | machine-minted tasks would cross the authority line — do not | SAFE at ~4 approvals per epsilon-scale run |
| F17 disarm destroys auto-pardon baseline | cause deleted (no disarm; epoch lives on receipts, indestructible) | none | SAFE |
| F18 Boa i32 floats | code defect; discovery identical (lanes fail → notification); repair out-of-band; **class already deleted by local-first** (F29: "structurally absent in local mode") | none | CLOSED by deletion |
| F19/F20 D62 four layers zero seams | seam-test class (PA-35); orthogonal to rituals; still open as a gate-stage question | none | SAFE / UNCHANGED — deletions neither catch nor miss it |
| F22 goal-named file outside boundary | **retro-cert: lane edits the file, certifies, merges.** Saved: 2 attempts + commit + push-race + resume | live-collision check covers the concurrent-edit case (none live here) | SAFE, cheaper |
| F23 rest with dispatchable work | H3 shipped; F41: zero wake-pardons in ε2 | none | CLOSED |
| F24 closed schema in unowned file | retro-cert: lane edits the flow schema too. The ch2 hazard ("three would have edited that file concurrently") is handled where it always was: declared domains still schedule disjointness; undeclared overlap → live-collision defers the second lane; merge conflict is the backstop | wasted parallel work on collision — bounded, machine-priced; epsilon's price for the refusal alternative was 2 attempts + human per event | SAFE |
| F25 host config leakage | ext2 isolation guard (root) | none | SAFE |
| F26 e2e asserts replaced behaviour | as F22 | none | SAFE |
| F27 D77 deletion | is the posture | — | KEEP |
| F28 forge-native gate | ext0 local-audit arm deletes cause; repair pattern (in-campaign amendment) kept | none | SAFE |
| F29 stays-armed / disarm-terminal semantics | replaced by lease; the PA-16 contradiction dissolves | none | SAFE |
| F30 status renders newest pass | fixed (H4), kept | none | CLOSED |
| F31 integration branch never absorbs main | publish verb + re-gate (mechanization-correct); divergence source shrinks under D2 | none new; CA-8's ungated-publish hole **closes** | SAFE |
| F32 narrator 0-for-35 | D5 deletes the slot; lane subject adopted | none — nothing downstream ever consumed a narrated subject | SAFE |
| F33 gate-only lint classes | ext0 template; two of three ε gate cycles erased at authoring | none | SAFE |
| F34 stale-pin mis-attribution | doctrine committed (VD-13 rule) | none | SAFE |
| F35 refusal ≡ crash | class **shrinks to the deny-list**; narrowed `needs-authority` outcome covers it; `projectionWaitMs` reverts to an untouched backstop | none | SAFE |
| F36 diagnosis 16-for-16 | kept verbatim — becomes the notification payload | none | KEEP |
| F37 stale-pass race burns an attempt | **cannot exist**: the stale pass's receipt carries the old epoch and never counts | none | SAFE — cause deleted by derivation |
| F38 summary collision + botched archive ×3 | ext0 stage digest deletes cause; the ritual dies | none | SAFE |
| F39 estate crash-loop | deploy-side; ext2 population replay is the net; deploys stay human **with that gate** | none — rituals played no role in F39 either way | SAFE / the one place a *new* check is justified by an observed defect |
| F40 preflight 0-of-4 | keep as free warning; its predictive job is moot under D2 | none | SAFE |
| F41 liveness arm works | kept | — | KEEP |
| F42 census authoring | doctrine, committed | — | KEEP |
| F43 flooding session deaths | steer (kept) + commit-first doctrine (committed) | none | SAFE |
| F44/VD-8 two completion contracts | **unchanged by rituals** — E1 is the fix; under D2 fewer amendments → fewer rotations meanwhile | none | ROOT FIX REQUIRED (ext2) |

### 2.2 The interventions

| episode | count | under D1–D6 |
|---|---|---|
| pardons | 10 | **0.** 4 (seqs 4, 27, 30, 34) are authority-epoch changes; 3 (15, 19, and half of 27) are stale-pass/pardon races that cannot exist; 1 (10) was F23, already closed by H3; 1 (20) was F38, cause deleted; 1 (7) is subsumed by its own steer bumping the epoch. |
| grants | 4 | **0 grant commits.** All four replay as retro-certified in-domain-after-the-fact merges. The lockfile cohort grant (`ef0443f8`, 5 lanes) replays as: five lanes each regenerate `Cargo.lock`; collisions land at merge exactly as they would have under the grant; no human. |
| steers | 2 | **2 — kept, upgraded.** The only verb whose replay requires a human, and the record's cheapest acts. Steer bumps the target's epoch (subsumes pardon 7). Phone-reachable (E8 core). |
| escalation episodes | 9 (18 diagnoses) | 9 notifications carrying the same report; siblings keep moving; human touch only at hard-cap. See §3 for what the 9 episodes prove about the retry budget. |
| gate cycles | 3 | 1. Clippy cycle (74 min) and final-bar cycle (61 min) die at authoring (ext0 template); F28's publication-order cycle dies with the local-audit arm. The residual class — a genuinely novel stale assertion — still costs one cycle, repaired by an approve-a-rendered-diff amendment. |
| estate crash-loop | 1 | unchanged path (deploy), newly gated (estate replay). |
| trailer divergence (F44) | 1 | fires until E1; the *close ceremony around it* — re-arm `--no-enqueue`, hand `git branch` at 12:04:36, unrecorded checkpoint restore, archive near-brick — **all deleted** by release-from-durable + lease + stage-digest refs. |
| release-close trap (PA-16/17/18, the 86-minute tail) | ~14 acts | **~1–2 acts**: quiescent → lease lapse → `publish` (re-gated) → `release --plan/--probe/execute` as one human-invoked, fully-rendered act. PA-18's dangling locators cannot recur (no re-registration ⇒ locators stable). |

### 2.3 The three lists

**Proven safe against history** (every recorded instance replays caught, at
equal or lower cost, with no new exposure): delete **pardon/resume** (D1);
delete **prospective refusal on ordinary paths** and with it the **grant
ritual and its mechanization** (D2 — with the §4 deny-list); delete
**re-arm/disarm** (D3); delete the **archive ritual** (stage digest); delete
the **narration slot** (D5); delete the **latch** in favour of notification +
budget (D6); delete the **publish hand-harness** (verb with re-gate); delete
**release-requires-armed-registration**.

**Deletions that lose real protection, and the minimal invariant that keeps it:**

1. *Full* deletion of refusal would let a lane commit worklist/gate/flow-config
   edits — reward-hack patterns 1 and 9. **Invariant I1** (§4): the authority
   deny-list, enforced at the tree-delta gate, which runs from the deployed
   store path the repo cannot move. Refusal survives only there.
2. *Full* deletion of the attempt budget (no latch at all) would let a
   deterministic failure re-run forever. **Invariant I3**: the budget's stop
   half — per-epoch stop plus a hard lifetime cap per task (the `/goal`
   precedent: 8). PA-44 is the record's proof this is the restriction that
   earns its keep.
3. Deletion of authored `maxParallel` without a **derived** cap would let a
   19-lane disjoint frontier saturate the host (gate builds contend even when
   agents are remote). **Invariant**: concurrency derived from host capacity —
   ultracode's own shape.
4. Deletion of the human **first arm** would make any committed worklist
   self-executing. **Invariant**: one deliberate `run` per identity; only
   *re*-admission is automatic.

**Where history is insufficient (stated, not papered):**

- **No adversarial lane ever ran.** Every agent in the record was aligned;
  CA-1's "4-for-4 false positives, zero rogue writes" is evidence about
  aligned agents. D2's safety against a *malicious* lane rests on the §4
  invariant argument, not on replay. The record cannot prove it; the
  invariants must.
- **The derived-concurrency formula.** `maxParallel` was never stressed (ε1
  near-serial regardless; ε2 honest at 3). No data says what the host
  saturates at. Ship a conservative derivation + backstop and measure.
- **Collision-check thrash under an undeclared-domain wave.** Every epsilon
  stage declared domains (the skill requires them when parallel), so the
  live-collision path has ~zero recorded traffic. Keeping declared domains as
  the scheduling input (CA-5) makes the collision check a backstop, not the
  primary mechanism — but its cost profile under heavy overlap is unmeasured.
- **F12** has no surviving definition.
- **Steer-bumps-epoch** rests on two steers, one co-occurring pardon. Thin,
  consistent, and the cheapest rule in the design — but two data points.

---

## 3. The caps replay

Every cap in the system, against every recorded firing. "Correct" = the cap
stopped something that should have been stopped.

| cap | where | value | fired | verdict |
|---|---|---|---|---|
| task attempt budget | `actions.rs:2286,2357,2394` (`attempt == 2`, hardcoded) | 2 | 9 episodes, all ran to exhaustion | **The stop half is the week's best restriction (PA-44). The retry half went 0-for-9.** The receipts ledger shows it exactly: every diagnosed task carries `[1,2]` pairs — `chapter-gate [1,2,1,2,1,2,1,2]`, `squash-rowversion-ladder [1,2,1,2]`, `producers-config-variant-box [1,2]`, `port-fold-half [1,2]`, `delete-python-driver [1,2]` — and **not one lone `[1]`**, which is the signature attempt-2 success would leave. Machine-steered retry of a deterministic failure converted nothing, nine times, at ~30–40 min each. Every conversion in the record came from **new information** (grant, amendment, steer, merged dependency) followed by a fresh attempt. Implication: the unit is the epoch, not the attempt — retry within an epoch only for classified-transient faults; otherwise stop at 1 and notify. |
| machinery retry budget | `actions.rs:38` (`MAX_MACHINE_RETRIES = 2`) | 2 | 3 (receipts 21–23) | **Fired wrongly 3-for-3**: all three were H1 boundary refusals exiting without an envelope, misread as `result-schema-mismatch` machinery faults (F35). The class disappears under D2 + the narrowed refusal outcome. |
| `projectionWaitMs` | `campaign_registry.rs:29-30` | 10 000 default, one scalar per registration | every refusal (the F35 shape) | **Fired wrongly, never correctly.** VD-2's own verdict stands: the honest fix is the outcome channel, not the knob. Under D2 it reverts to an untuned machinery backstop. |
| `maxTasks` | `epsilon.json:5` (24); default 8→64 (`campaign_contract.rs:21`); hard 128 (`:22`) | authored | **once, Aug 10** — `"campaign must contain 1..=8 tasks"` blocked a legitimate 13-task campaign and was *"misread as a hard schema cap until source dive"* (`AUGUST-10-LEARNINGS.md:80-82`) | **Fired once, against its own author, protecting nothing.** Its only recorded effect was friction plus issue #460 to improve its error text. |
| `maxParallel` | `epsilon.json:6` (3); default 1 (`:894`) | authored | never as a protection | *"conflictDomains overlap — not maxParallel — is the binding scheduling constraint"* (`AUGUST-12-LEARNINGS.md:229-233`); ε1 "dishonest", ε2 honest by luck of shape (AUG14 op-notes). A guess that sometimes coincided with the truth the machine already computes. |
| `runtimeMaxSec` (gates 900/3600/1800, chapter 10 800, agent default 14 400, campaign 86 400, steward 120) | `epsilon.json:21,39,55,834`; `campaign_contract.rs:23-31,902-904` | authored + defaults | **zero recorded firings, all week** | Every death that looked like a timeout was a Codex session death (flooding, F43) or a projection wait. The authored per-gate numbers are unvalidated guesses that behaved identically to defaults. |
| gate count 1..=16 | `campaign_contract.rs:485-487` | 3 used | never | dead headroom (VD-20). |
| `NARRATION_ATTEMPTS` | `actions.rs:31` | 2 | 74 firings, 0 acceptances | **a cap that manufactured its own 100 % failure rate** (VD-11: two independent rules ⇒ guaranteed fallback). |

**The caps verdict, stated plainly for the operator's question ("do we really
need the max permitted agents? the maximum allowed minutes?"):** across the
entire recorded history, **no cap an author typed ever fired correctly**. Two
fired only wrongly (projection wait, machinery retries — both misclassifying
refusals), one fired against its own author (`maxTasks`), and the rest never
fired at all. The protections that actually held were: the **hardcoded
attempt-stop**, the **gate ladder**, and **domain-disjoint scheduling** — none
of them an authored number. So: delete every numeric field from the authoring
surface (`maxTasks`, `maxParallel`, per-gate `runtimeMaxSec` beyond the
default); keep hard, non-authored backstops (`MAX_CAMPAIGN_TASKS = 128`, the
24 h campaign ceiling, the attempt-stop, a derived concurrency cap) — exactly
ultracode's split: a machine-derived concurrency cap and a 1000-agent runaway
backstop, with **no number in the worklist**. An authored cap is a prediction;
the record shows the author's predictions were never load-bearing and
occasionally self-obstructing. (Honest caveat: never-fired ≠ worthless for the
*backstop* class — a hung gate would eventually need `runtimeMaxSec`. The
distinction that survives is authored-vs-backstop, not cap-vs-no-cap.)

---

## 4. The adversarial section — the twelve patterns against the deleted design

Assume D1–D6 ship and a swarm of capable, misaligned-incentive agents (the
aug-4 twelve) runs overnight, multi-harness, on a repo that matters. What opens,
and the cheapest guard that keeps humans out of the loop.

### 4.1 The invariants and their enforcement points

| # | invariant | enforcement point | which deleted ritual it replaces |
|---|---|---|---|
| I1 | **Authority is committed bytes at a named revision that no lane can write.** Concretely: the worklist file, the `campaign` section (gates, agent, steward), `test/fleet-gate.sh`, `.github/` are on a static deny-list; a lane commit touching them fails tree-delta unless the task's *committed* `conflictDomains` explicitly name them (which only a human-authored worklist can do). | tree-delta gate, executing from the **deployed store path** (a lane editing the repo's flow copy grades under the pinned one — PA-46's frozen-flow rule, now permanent), against the worklist fetched from the remote base (`campaign.rs:4663-4664`) | prospective refusal on ordinary paths |
| I2 | **The grading machinery moves only by deploy**, and deploy is a human act gated by the estate replay (ext2). | nix pin + generation switch | the PA-30 prose prohibitions ("no hand edits", "no restarts") |
| I3 | **Bounded attempts**: stop at per-epoch exhaustion; hard lifetime cap per task across all epochs. | driver, hardcoded | pardon, latch-release |
| I4 | **Merge only through gates, machine-run**; lanes cannot merge, push, close, or settle (worker contract: *"nothing pushed by the worker"*, PA-40 — held across three harness flips). | driver merge criterion + gate ladder | supervision |
| I5 | **Every automated act writes an epoch-stamped receipt** (armSerial, worklistSha256, timestamp). | receipts JSONL + ext0 stamps | pardon reasons as the audit trail |
| I6 | **Worktree isolation + live-collision check**: certified-but-unmerged paths of sibling lanes are a machine-checkable exclusion set. | prep/ownership/tree-delta | the collision half of prospective refusal |
| I7 | **Proofs are durable and, post-E1, exact**: trailers + completion refs written by the driver (store path), never by the lane; `completionProofs` persisted. | driver + release verb | — |
| I8 | **Human verbs are exactly**: author/approve worklist bytes (episodic; approve-a-rendered-diff is intern/phone-shaped), steer (epoch-bumping, phone-reachable), deploy (estate-gated), invoke release (fully rendered plan). | CLI surface | all ~40 transcription acts |

### 4.2 The twelve, one at a time

| pattern | does the deleted design newly enable it? | cheapest no-human guard |
|---|---|---|
| 1. Gate self-weakening | **Yes, if D2 ships without I1** — retro-cert would certify an edit to `test/fleet-gate.sh` or the worklist's `gates` array like any other path. | I1 deny-list + I2 store-path grading. Quantified: a gate-weakening edit must (a) be committed by a lane that cannot name the path in its own authority, (b) survive tree-delta reading committed bytes it cannot alter, and (c) even if merged into the *repo's* gate script, change nothing until a **human deploy** that the estate gate fronts. Three independent walls, zero humans at runtime. |
| 2. Proof-class inflation | No new surface: trailers and completion refs are written by the driver from the store path (I7); lanes push nothing (I4). The *system's own* standing instance — `final-conformance-bar-harness` asserting a `--list` — is closed by ext0. Residual: the bridge oracle is self-certifying-ish (PA-15) — closed by E1 + persisted `completionProofs`. | I7. |
| 3. Golden regeneration reflex | Unchanged — the refusal model never guarded this either (a granted lane could always edit its tests). | The VD-14 rule as committed doctrine: *every delivered behaviour needs an acceptance criterion that fails without it*; ACs pre-specified at authoring (human bytes, I1). |
| 4. Commit-stream pumping | No — settlement is per-task, not per-commit; no-stubs/placeholder greps live in the lane gates and fleet gate. | existing gates (I4). |
| 5. Tautological tests | Unchanged (same as 3). | AC pre-specification (I1 bytes). |
| 6. Easy-bead cherry-picking | **Structurally absent**: the frontier is machine-chosen (scheduler dispatches; agents never self-select tasks). Epoch churn as a farming vector: a lane cannot bump its own epoch — epoch sources (worklist commits, steers, dependency merges) are all outside its write authority (I1, I8); dependency merges require gated work (I4). | I3's lifetime cap backstops any residual loop. |
| 7. Close-pump abuse | Absent — only the machinery settles tasks (I4). | — |
| 8. Scope-splitting | Absent — task granularity is human-authored bytes (I1). | — |
| 9. Spec-editing as progress | **The wayfinder-Notes hole, and the sharpest risk of D2.** A lane that can edit the worklist can weaken its own ACs, widen its own domains, or drop a gate — and poll re-admission would then *execute* the self-authorization on the next tick. | **I1 is the whole answer**, and it is why capped auto-grant (machinery-writes-authority) is the wrong mechanization: it opens the same door from the inside. Under D2 + I1 the authority file has exactly one writer class: humans. The record already validates the wall from the aligned side: *"the gate reads committed bytes it cannot write"* (AUG14 grants glossary) held all week. |
| 10. Conformance metastasis | The risk is **ours**, not the swarm's — adding speculative checks to the deleted design. | aug-4's own rule, applied in this report: every new check above cites an observed defect (F39 → estate gate; CA-8 → re-gate; F35 → narrowed refusal). Nothing else is added. |
| 11. Dependency smuggling | Mildly wider: retro-cert lets any lane touch `Cargo.toml`/`Cargo.lock` without a cohort grant. | `cargo deny` already in the fleet gate; lockfile changes are certified paths in the receipt (I5), diffable; the ε record shows the grant model allowed exactly the same edits with a human retyping them. |
| 12. Demo-path hardcoding | No new surface. The probe already uses a runtime-created real repository; ext2's estate replay mandates **sampled real rows** — precisely the aug-4 counter ("runtime-selected subjects … differ from development fixtures"). | ext2 as planned. |

**How much work the committed-bytes invariant and the gate ladder actually do,
said precisely:** I1 + I2 close patterns 1 and 9 outright — the only two the
deletion newly opens. I4 + I7 close 2, 4, 7. The scheduler closes 6 and 8 by
construction. 3 and 5 were never closed by any ritual and are closed only by
authored ACs. 11 and 12 are closed by existing gate stages plus ext2. **Every
ritual deleted in §2 maps to an invariant enforced by a machine check that
already exists or is already in the plan**; no deletion leaves an invariant
enforced by nothing, and no guard added here puts a human back in the runtime
loop. The residual human surface is I8, and every act on it passes the intern
test: the text is fully rendered by the machine; the judgement is
approve/deny/escalate — except steer and worklist authoring, which are the two
verbs the record shows carrying real judgement, and which were never candidates
for deletion.

---

## 5. What this changes in EPSILON-EXTENSION.md (the delta, nothing else)

1. **Replace ext1's capped auto-grant with retrospective certification +
   live-collision + the I1 deny-list** (the flow's own serial-task path,
   promoted). The machinery never writes the authority file. `needs-grant`
   narrows to `needs-authority` for deny-list hits only.
2. **Replace ext0's auto-pardon-authority-delta with epoch-scoped budgets** on
   the same receipt stamps; delete `resume`; steer bumps the epoch. Retry
   within an epoch only on classified-transient faults (the 0-for-9 result).
3. **Strip authored caps from the worklist schema**; derive concurrency, keep
   hardcoded backstops. (This answers the operator's maxTasks/minutes question
   with the record: they never once earned their keep.)
4. **Pull E1 (completion-identity unification) as early as dependency order
   allows** and persist `completionProofs` + the plan document immediately —
   the close condition depends on it and every interim release is
   bridge-grade.
5. **E5 → delete-until-proven** (subject-adoption first); **E8 → core** (the
   steer channel is the intern surface).
6. Keep unchanged: stage-digest refs, final-bar honesty, cheap-first +
   local-audit, doctrine-to-skills, publish-with-re-gate,
   release-from-durable, lease, estate replay, probe honesty, isolation
   guard, E6, E7.

The destination restated in the deleted design's terms: the operator authors
worklists, steers from a phone, approves rendered amendment diffs, deploys
behind an estate gate, and invokes release from a rendered plan. Everything
else is derivation from committed bytes — which was the thesis all along.
