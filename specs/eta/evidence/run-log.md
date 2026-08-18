# eta run log — per-lane ledger and incident record

Standing consumer: the operator's status checks; the C1 seam sitting; the
P2 judge-tier replay (real spend numbers). Appended by the supervising
orchestrator after every lane. Credit figures use the doc-derived rates
(input ≈800/M, output ≈4,800/M, cache ≈80/M at flagship scale) until S5
lands scraped totals; the window is 10,000 credits, reset ~2026-08-22.

## Ledger

| # | when (UTC) | what | model | turns | in | out | cacheRead | ≈credits | window remaining |
|---|---|---|---|---|---|---|---|---|---|
| 0 | 08-15 13:34 | adapter smoke pi (pre-arm) | flagship | 3 | 4,751 | 258 | 0 | ≈5 | ≈9,995 |
| 1 | 08-15 13:52–13:58 | job-limits-optional attempt 1 | flagship | 24 | 51,501 | 16,276 | 0 | ≈120 | ≈9,875 |
| 2 | 08-15 14:20–14:47 | job-limits-optional attempt 2 | flagship | 37 | 62,739 | 16,302 | 0 | ≈130 | ≈9,745 |
| 3 | 08-15 15:03–16:10 | job-limits-optional attempt 3 — MERGED ffcb9a15 | flagship | 76 | 138,727 | 46,001 | 0 | ≈332 | ≈9,413 |
| 4 | 08-15 16:14–16:57 | oom-legibility attempt 1 — MERGED first try | flagship | 66 | 139,379 | 55,220 | 0 | ≈377 | ≈9,036 |
| 5 | 08-15 17:13–17:55 | excerpt-derivations attempt 1 — MERGED first try | flagship | 101 | 197,959 | 90,205 | 0 | ≈591 | ≈8,445 |
| 6 | 08-15 18:07–18:46 | adapter-relative-policies attempt 1 — gate-refused | flagship | ~125 | ~200,000 | ~85,000 | 0 | ≈570 | ≈7,875 |
| 7 | 08-15 18:55–19:42 | adapter-relative-policies attempt 2 — typed refusal (driver corpus outside domains) | flagship | 187 | — | — | 0 | ≈670 | ≈7,205 |
| 8 | 08-15 19:45–20:20 | substrate-numerals-guard attempt 1 — MERGED first try | flagship | 59 | 92,262 | 35,793 | 0 | ≈246 | ≈6,959 |
| 9 | 08-15 20:16–21:20 | adapter-relative-policies attempt 3 — committed, orphaned (see below) | flagship | 274 | — | — | 0 | — | — |
| 10 | 08-15 21:47–21:56 | adapter-relative-policies attempt 4 — DIED ON THE WALL: 429 quota exhausted | flagship | 69 | — | — | 0 | — | 0 |

## WINDOW EXHAUSTED — 08-15 21:56 UTC. Reset: 08-22 13:34 UTC.

Attempt 4's stream ended with the verbatim wall: "Your token-plan 1-week
quota has been exhausted. The quota will reset at 08-22 13:34:00 UTC."
(pi exited 0, empty stderr, no envelope — the V-16 shape, now witnessed
on pi; S5's fixture case exists in the wild.) The window's clock started
with the 13:34 UTC adapter smoke.

**Rate reconciliation — the estimate was wrong and this table's credit
column understates ~2.5x.** True window consumption: 11 sessions, 1,019
turns, 1,691,115 input + 556,392 output, zero cache reads = the full
10,000 credits. Implied real rates if the 6x output ratio holds: input
≈2,000 credits/M, output ≈12,000 credits/M — roughly 2.5x the
qwen3.6-plus-scaled guess. Corrected planning number: a merged
small-lane task costs ≈700–1,200 real credits; a weekly window funds
≈8–12 such lanes, or roughly one chapter. The "chapters 1–2 in one
window" projection was wrong; "one chapter per window" is the honest
number. The Aug 7 doctrine held where it mattered: serialized dispatch,
per-lane receipts, and the wall cost one 9-minute attempt instead of
four parallel lanes — every credit of this window bought recorded,
merged work or recorded findings.

## S4 attempt 3 postmortem — committed but orphaned

Attempt 3 DID commit (274 turns; git add -A && git commit verified in
its session) in its prepared worktree at digest 4804c490722c, yet the
driver recorded "agent produced no commit relative to the prepared
base" and the worktree was recycled before salvage. Working hypothesis:
the mid-flight re-arm (amendment 4, queued while S6 ran) re-keyed the
admitted digest, and the attempt's ownership check evaluated against a
fresh checkout — the commit landed in an orphaned universe. Standing
supervisor rule until understood: NEVER re-arm while a lane is mid
attempt; queue amendments for the gap between attempts. Candidate
machinery finding for a later sitting: an attempt whose worktree digest
no longer matches the admitted graph should fail legibly as
digest-mismatch, not as agent-produced-no-commit.

## Chapter 1 finish on claude-code (operator ruling 2026-08-17)

| # | when (UTC) | what | adapter | outcome |
|---|---|---|---|---|
| 11 | 08-17 | adapter-relative-policies attempt 5 | claude-code/host-default | committed; ownership+driver-suite+cargo-tests+clippy green; flake-build-subset caught the fixture outside the nix source filter |
| 12 | 08-17 | adapter-relative-policies attempt 6 — MERGED | claude-code/host-default | crate-local fixture per amendment 6; sandbox proof run pre-commit; witness adapter-relative-policies-fb1861e7fa6a17a5 |

S4 closed after six attempts: two boundary refusals (supervisor
authoring), one orphaned green commit (supervisor re-arm mid-attempt),
one quota wall, one sandbox-filter fixture defect, one merge. Every
failure named itself before touching the base; the class-killer landed
with its portability matrix standing guard.

## Dispatch state

Paused per ETA.md §6 — a planned state, not an incident. Campaign armed,
zero units, S4 blocked on its epoch budget (refreshes at next
amendment), spec-lint chain and doc task pending. Nothing dispatches
until the window resets or the operator rules on the claude-code
fallback (charter §3 names it the standing fallback pool when the
metered window is the constraint; §6 names it contingency-only — the
tension is the operator's to resolve).

## Attempts 6–7 postmortem: the drip stops with a supervisor enumeration

S4's true consumer surface is eighteen files across six crates, the JS
flow, the presets, the python driver corpus, and the final-bar cases —
the cross-adapter literals are baked everywhere, which is what made V-15
the severity it was. The vestige ledger's four-domain list undersized the
blast radius, and each attempt found exactly one more wall (correct
behavior, expensive recon). Amendment 4 grants the grep-enumerated
surface and writes the file list into the goal so attempt 3 spends its
turns editing, not re-deriving. Standing lesson for later sittings:
before arming a delete-a-default task, grep the default's name and put
every hit inside the boundary.

## Attempt 6 — adapter-relative-policies (cargo-tests gate catch; domain gap again)

The lane deleted the three policy consts from the contract but
crates/tally/src/cli/campaign.rs imports and uses all three — and
crates/tally was not in S4's declared domains, so the lane could not
have fixed the consumer it broke. The cargo-tests merge gate refused the
head with the E0432 lines verbatim; no false green. Amendment 3 adds
crates/tally to the domain and names the consumer in the goal. Same
lesson as S1: the vestige ledger's domain lists were authored against
group shapes, not against the consumer graph — each gap surfaces once,
legibly, and becomes goal bytes.

## Calibration verdict (dispatch rule 2, closed)

S1 landed for ≈579 credits across three attempts; the winning
implementation lane alone was ≈332 credits, 76 turns, 67 minutes. pi's
usage records report zero cache reads on the plan rail at these
transcript lengths — the Aug 7 cache-dominance model does not apply at
small-lane shape, and real burn is roughly a tenth of the ≈3,000-credit
pre-calibration estimate. Projection: chapters 1–2 complete inside a
single weekly window with the 15% reserve untouched. Serialization
(maxParallel 1) stays until C1 regardless — the containment is the point,
not the credits. Console cross-check deferred to the C1 seam per the
ledger method note.

## Attempt 2 — job-limits-optional (ownership refusal; implementation done, wrong assertion home)

The lane produced the full implementation commit but placed the
daemonArgv-clean assertion in flake.nix; ownership containment refused
the merge — one path outside declared domains, named exactly. Attempt 1's
recon had already ruled the module-side placement, but that conclusion
lived only in its session; amendment 2 moves it into the goal bytes,
which is where inter-attempt knowledge survives. The steward-diagnosis
admission defect fired again as expected (S4 owns it); supervisor
performed the diagnosis manually per E5 rule 7.

## Attempt 1 — job-limits-optional (typed refusal; both defects legible)

Lane exited 0 with no commit and a typed `needs-authority` envelope naming
six files: the worklist declared `crates/tally/src` where the vestige
ledger's group-1 package said `crates/tally` — the six integration tests
under `crates/tally/tests/` construct `UnitLimits` struct literals and
must change when the fields become `Option`. Supervisor authoring error;
the lane's refusal was correct and cheap (≈120 credits, 6 minutes).
Fix: worklist amendment restores `crates/tally` as the domain (this
commit); the amendment refreshes the task's budget by epoch derivation —
the ext0 machinery working as designed.

Second finding, a live product defect on the ext0 machinery's first
diagnosis dispatch since the boundary deploy: `applyDiagnosisRole`
(examples/flows/spec-build.js near :2525) stamps `sandboxPolicy:
"read-only"` on the steward-bound diagnosis node, while the steward seam
refuses adapters that declare launch policies — so diagnosis-via-steward
can never render against any legal steward. The pass died at admission:
`invalid adapter "narrator": sandboxPolicy value "read-only" is not
authorized by this adapter`. V-15's class, one function beyond the
ledger's citation. Absorbed into adapter-relative-policies by amendment
(this commit). Until that task merges and deploys, failed-lane diagnosis
cannot auto-dispatch; the supervising orchestrator performs diagnosis
manually — which is where E5 rule 7 puts it anyway.

Calibration note: a recon-shaped six-minute lane on the flagship cost
≈120 credits with zero cache reads reported in pi's usage records; the
cache-read term of the Aug 7 model did not appear at this transcript
length. The ≈3,000-credit ext0-shape estimate stands until a full
implementation lane closes.

## Chapter 1 CLOSED — 2026-08-17 12:33 UTC

All six substrate-repair tasks merged and published to main as
cherry-picked squashes (652eb612..3813e769), proven by the full ladder:
language-entry-policy, fleet-gate PASS for 3813e769, final bar 24/24
(the #440 launch-cwd-ordinary-completion case passed without retry).
The eta campaign is disarmed pending the operator's chapter-2 ruling;
the chapter-2 auto-dispatch that exceeded the ruling was stopped at two
minutes. Awaiting: pin-bump confirmation (deploys the substrate repairs;
deletes the 24 GiB drop-in outright per §5).

## Chapters 1+2 CLOSED — C1 green, 2026-08-17 18:05 UTC

Chapter 2 ran end to end on claude-code (operator ruling): spec-lint-core
merged on attempt 4 (three legible refusals first: a sandbox-filter
panic, a dirty-tree ownership refusal, and a background-build wait-loop
deadlock — each now goal-byte doctrine), then resolution, flake-check,
skills-amend, and doc-anchor-regrammar merged first-try. The chapter
gate's first run went RED on the linter's first contact with its own
authors' specs — seven defect lines across specs/eta and specs/zeta —
and the operator-side regeneration turned it green without a single
lane edit to spec bytes: the two-way loop's first full cycle. Final
state: fleet gate PASS 5fe28fe90cea1a3f18e2a1080d95d6f585a59cc9 with
checks.x86_64-linux.spec-lint executing inside the ladder; final bar
24/24; published to main; eta disarmed pending the chapter-3 sitting.
The coverage table above this entry's commit was rendered by
spec-lint --coverage specs/eta — the first tool-rendered close-out;
the hand-rendered table is dead as designed.

## Chapter 3 CLOSED — C2 green, published, deployed. 2026-08-18 05:55 UTC

All seven ext1 verbs merged on claude-code (host-default model), five of
them first-try; X4 took two attempts (fixture domains), X7 two (its gate
inherited a test-isolation race from the lease merge, repaired in the
same lane). The campaign reached `complete` with 20/20 done and the C2
checkpoint witnessed; fleet gate PASS b54ea267 on the published main
head, final bar 24/24. Pin-bump 2 flashed (daemon at as412bk3...,
smoke PASS): the single-line integration model, poll re-admission, the
acceptance-domain lint, the spec deny-list, receipt-derived gate
budgets, the lease, and the inbox are now the deployed contract — this
seam's manual cherry-pick publish was the last one; from the next
armed campaign, main advances only by machine fast-forward of a
gate-proven head, and a worklist push is the arming act.

Carried finding, still open (host catalog): the narrator shim answers
the diagnosis role with its fixed commit-narration schema
(result-schema-mismatch on every diagnosis dispatch); the fix is a
role-aware steward shim in dotfiles/home/tally.nix — queued for the
chapter-4 sitting. Buildout stands at chapters 1–3 of 5; stopped here
per operator instruction.

## Chapter 4 armed — sitting C2, 2026-08-18 06:46 UTC

Adapter claude-code host-default per the operator's chapter-4 ruling
(2026-08-18). The carried shim finding is CLOSED before arming: the
role-aware steward shim deployed (dotfiles de49728b; mechanism — job
units have no stdin, diagnosis briefs arrive as the TALLY_BRIEF file;
the stdin-only shim answered narration schema from an empty read),
smoke-proven against a synthetic brief. Sitting e41edb96 appends the
chapter-4 tasks and retires the four template-gate runtimeMaxSec
guesses per gate-budgets-from-receipts' own schedule; the admission
rehearsal rendered the derivation legibly — all four gates on the
never-fired 3600s floor, because the re-arm opened a fresh receipt
lineage (zero observations); driver-suite gains headroom (900→3600),
nothing tightens. Registry was empty post-disarm, so re-entry used the
arm verb once; poll timer restarted — push-to-re-admit carries
amendments from here. Rehearsal note: `campaign status` refuses with
"invalid campaign task table" while the registration has no reconciled
pass (lastObservation null) — machinery finding if it survives the
first reconciled pass.

### Chapter 4 lane ledger (claude-code, unmetered rail)

| # | when (UTC) | what | outcome |
|---|---|---|---|
| 13 | 08-18 06:46–07:46 | product-split attempt 1 — MERGED first try | integration 1ea314e5; agent ~50 min; whole node chain exit 0; steward commitlint fallback correctly replaced a non-conforming lane-tip subject with the task-id template |
| 14 | 08-18 07:47–09:15 | worklist-scaffold attempt 1 — MERGED first try | integration 2c41af34; node chain clean end to end; the scaffold verb and its bare-repo no-spec-plane property landed under the receipt-derived gate budgets' first live run |
| 15 | 08-18 ~08:50 | baseline-parity-probe attempt 1 — ownership refusal | agent left uncommitted changes (dirty-worktree finish violation, the known class); refused before touching the base, exit=1 named exactly |
| 16 | 08-18 ~08:52 | AUTO-DIAGNOSIS — first live cycle of the role-aware shim | verdict retry, outcome-first diagnosis correctly distinguishing no-commit from boundary-breach; schema-valid on the first real dispatch; zero supervisor involvement — the failure→diagnosis→retry loop closed autonomously for the first time in the campaign's history |
| 17 | 08-18 08:55–09:38 | baseline-parity-probe attempt 2 — MERGED | integration 64576e92; steered by the auto-diagnosis; lane-tip conventional subject accepted by commitlint directly |
| 18 | 08-18 09:39–10:15 | product-docs attempt 1 — MERGED first try | integration c8e7ea5d; all four chapter-4 implementation lanes done, 4 merges in 5 attempts |

## C1 re-witness: one flaky red, machine-recovered; three findings (08-18 ~11:30–12:00 UTC)

The C1 re-witness failed once — `fleet gate: FAIL e091b46a` — then the
next poll pass re-ran it green (final bar 24/24) with no supervisor
verb; C2's re-witness followed. The campaign's first RED checkpoint
recovered autonomously. Findings for the C3 sitting, none blocking:

1. **Red transcript evidence is clobbered.** Fleet-gate transcripts key
   by head sha; the green re-run overwrote the red run's transcript, so
   the failing check's identity is unrecoverable (coredump timeline
   suggests the crash-injection test genre, `release_execute_crash_child`
   — deliberate SIGABRT cores, likely #440-class flake, unproven).
   A red transcript should be preserved, not overwritten.
2. **A steward diagnosis timeout kills its pass.** diagnose-chapter-gate-c1
   hit the 120s steward node budget (2.4s CPU / 2min wall — model
   latency on a checkpoint-sized brief), and the projection timeout
   surfaced as FlowResultError result-schema-mismatch, failing the
   WHOLE pass — V-16's class one seam over: an envelope-less steward
   death should be a typed diagnosis-unavailable outcome (blocked
   escalation), never a flow crash. The 120s budget itself needs a
   ruled, larger number for diagnosis-role dispatches.
3. **Post-flaky-red conduct was correct end to end:** checkpoint-record
   wrote the red fact, cleanup ran, the next pass re-dispatched, and
   the re-run proved the same head. Wall-clock cost only.

Machinery observation for the C3 sitting: after the re-arm, the
reconciler honored durable done-ness for the twenty implementation
tasks but is RE-RUNNING the checkpoint tasks (chapter-gate-c1 started
10:19 UTC on the chapter-4 integration head) — checkpoint proofs are
receipt-lineage-scoped where implementation done-ness is
tree-derivable. Defensible semantics (a checkpoint witnessed against
an older head says nothing about the current one) and each re-run
freshly proves the full ladder over the NEW head, so C1/C2 re-passing
here subsumes most of C3's risk; cost is wall-clock only. Worth a
ruling at the sitting on whether checkpoint receipts should survive
re-arm when the proven head is an ancestor of the current base.

## Chapter 4 CLOSED — C3 green, machine-published, campaign complete. 2026-08-18 ~13:10 UTC

All four chapter-4 lanes merged (4 merges in 5 attempts; the one
retry was the auto-diagnosed parity lane). The checkpoint chain
re-witnessed C1 (one flaky red, machine-recovered, findings above),
C2, and C3 green over head e091b46a — fleet gate PASS, final bar
24/24 at every witness. **The first machine fast-forward publish in
the record**: the machinery rebased the four lane commits
content-disjointly over the supervisor's run-log commits and
fast-forwarded main to exactly the proven sha — proven head ==
published sha e091b46a, no operator verb, the cherry-pick ritual's
grave now witnessed. The campaign reached complete with 25/25 done
and the lease lapsed into the durable completion fact (X6's first
full lifecycle). Supervisor discipline learned mid-chapter: run-log
pushes to main re-key the observation and extend the checkpoint
grind — ledger commits batch to campaign-quiescent windows from now
on. Deployed contract shakedown verdict: X1 (machine publish), X2
(re-admission — exercised by every observation change), X5 (receipt
budgets, floor case), X6 (lease lifecycle), and the role-aware
diagnosis steward all carried live fire; the inbox saw no traffic
(nothing escalated — every failure self-resolved).

Pin-bump 3: dotfiles pin b54ea267 → ab10ac91 (published head + the
run-log commit; code-identical to the proven e091b46a).

Pin-bump 3 flash, first attempt FAILED — two lessons, one new P3 item:
(1) the supervisor repeated the recorded pipefail sin (nixos-rebuild
piped to tail masked exit 1; the run-log's own 08-17 lesson — retyped
here as penance and re-learned); (2) the real failure was the KNOWN
lease test-isolation race surviving its chapter-3 repair:
lease_concurrent_passes_never_double_dispatch_one_frontier panicked on
another test's Held lease for scope night-readmission-6
(campaign.rs:11971) inside the package build's cargo test — the
the-inbox lane's unique-scope fix was incomplete; two tests still
contend under sandbox parallelism. Flaky, not deterministic: the same
code passed the cargo-tests gate and three fleet-gate runs today.
Retried once with the cause named (E6 satisfied); the complete
per-test unique-scope repair is QUEUED UNDER P3 beside the #440
launch-cwd flake — the flaky-test class now has two members.

## Chapter 5 mid-flight: the lease flake becomes a tax; supervisor resequences (08-18 ~18:20 UTC)

Chapter-5 ledger so far: completion-unification MERGED 94198901 (one
gate re-run — the lease flake's second strike plus a second
steward-timeout pass kill); judge-replay-harness MERGED first-try
2e385f38; vestige-excision MERGED first-try 7af2c4cb (the four compat
shims and the W-321 grammar are gone); comment-sweep attempt 1
gate-refused by the SAME flake, fourth strike
(night-readmission-7, campaign.rs:12609), followed by the THIRD
steward-timeout pass kill. The flake now strikes roughly half of all
package-building gate runs and its fix lane (test-isolation-guard)
sat two positions back in authoring order. Supervisor intervention in
a verified-quiet gap (poll timer stopped first — the valve, no
straddle risk): amendment adds test-isolation-guard to the
dependencies of comment-sweep and steward-timeout-legibility, so the
isolation fix lands before any further dice rolls; epoch derivation
refreshes comment-sweep's budget as designed. E6 held: every retry
this chapter carries a named cause and the cause is one recorded
class.

Flash retry SUCCEEDED (exit 0, pipefail-verified). Post-flash
checklist executed: fleet-deploy inactive, both timers stopped, daemon
restarted on 5rd83q51...-tally-0.1.0. Seam verification, all green:
`tally --version` → "tally 0.1.0 (rev ab10ac91...)" — the deployed
binary names its own pin for the first time (D1); adapter smoke
claude-code PASS with commit probe verified; and the FIRST LIVE
baseline-parity probe returned verdict PARITY — bare and laned agents
identical on tempdirWritable, devShmWritable, cpuParallelism=32,
failingCommandStderr, memoryCeiling=absent; zero contained, zero
undocumented divergences. The §2.6 law is a measured property of the
deployed host. Chapter 4 seam closed; chapter 5 next (P1–P4).
