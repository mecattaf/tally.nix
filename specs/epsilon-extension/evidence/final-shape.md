# THE FINAL SHAPE — tally derived from first principles, then checked against epsilon

Written 2026-08-14, against `e921cccc` and the three excavation reports
(PA-01…47, VD-1…31, CA-1…14). Method: start from "multi-harness ultracode with
git as the only shared memory," derive what that premise *forces*, and admit
nothing else; then replay epsilon through the derived shape and grade every
surviving human act against the intern test. The intern test, as the operator
set it: **every operator touchpoint either arrives with its full text already
written and needs only approve/deny/escalate, or it must not exist.** A surface
that requires a frontier model to sit resident has failed.

**The one-paragraph conclusion.** Tally has exactly four irreducible additions
to ultracode — committed authority, durable facts, bounded retry with a legible
report, and a human judgment channel — and every ceremony in the epsilon record
traces to two modeling errors layered on top of them: (1) **the attempt budget
is keyed to a task identity that outlives the authority the attempts ran
under**, which manufactures the pardon, the escalation latch, `resume`, and the
"run tally campaign resume to unblock" instruction-to-the-human; and (2) **a
prospective scheduling declaration (`conflictDomains`) is graded as a runtime
correctness boundary**, which manufactures the refusal, the grant ritual, the
needs-grant gap, and four of twelve worklist commits. A third, smaller error —
**the registration modeled as an operator-managed lifecycle instead of a lease**
— manufactures arm/re-arm/disarm, `armSerial` as an operator-facing number, the
disarm-before-release trap (PA-16), and the 86-minute close. Fix the three
models and the verb surface collapses to five (`run`, `steer`, `stop`,
`status/quiescent`, `release`), the authored numbers collapse to zero, no pardon
or grant or auto-grant exists, and epsilon replays at ~16 human acts, none
resident, every standing one phone-shaped. The previous plan's marquee
mechanism — the capped auto-grant — is a band-aid on error (2): it automates the
transcription instead of deleting the gate that demands it.

---

## 1. THE IRREDUCIBLE CORE

### 1.0 The derivation

Ultracode baseline: JSON worklists, `agent()` with schema-forced structured
output, `pipeline()` fan-out in isolated worktrees, plain bounded retries, a
machine-derived concurrency cap, a 1000-agent runaway backstop. No pardons, no
arms, no grants, no blessings. What do tally's three deltas — **multi-harness**,
**overnight-unattended**, **git as the only shared memory** — genuinely force?

- **I1 — Authority is committed bytes at a named revision, written by a party
  the constrained agents cannot be.** Two harnesses share no process; git is
  the only channel; the worklist must therefore be a committed file any lane
  reads out of its own checkout. Proven load-bearing by F34: during ε1, agents
  honored boundaries H1 had not yet delivered *because
  `silent-factory-worklists/epsilon.json` is a committed file in the very tree
  each agent works in* (AUG14-LEARNINGS F34). The author/constrained-party
  separation is the anti-wayfinder invariant — wayfinder's known hole is that
  "the constraint and its exemption live in the same file the constrained party
  owns" (`docs/engineering/wayfinder.md:69`), and tally closed it.
- **I2 — Results are durable facts, not process state.** Ultracode's structured
  output lives in the orchestrating process; overnight-unattended means that
  process dies and a different harness resumes from facts. Refs, trailers,
  receipts, captures. Proven by F44's rescue (trailers outlived the driver
  generation that wrote them) and F31 (checkpoint refs survived a generation
  rollback).
- **I3 — No loop burns unboundedly; exhaustion produces a report a human can
  act on cold.** PA-44: "No loop ever burned more than two attempts before
  stopping and asking" — 9 episodes, 18 diagnoses, zero runaways; "the
  restriction that most clearly earns its keep." PA-43: the escalation report is
  "the best human-facing artifact in the system. Keep verbatim."
- **I4 — A human judgment channel that is rare, cheap, append-only, and never
  in the critical path.** Two steers in 19 hours, both irreplaceable (PA-04).

Everything else in the current system must justify itself as a projection of
I1–I4, or it is ceremony (the last mile was never written), or it is a modeling
error (the concept's existence *creates* the ceremony). The full table:

| concept | verdict | invariant / cause |
|---|---|---|
| worklist-as-authority | **keep** | I1, verbatim |
| registration / arm | **modeling error** → lease | operator-managed lifecycle where pessimistic-safety demands automatic reclamation (§1c) |
| armSerial | demote to derived | byproduct of the lease error; the durable fact is the worklist-sha admission ledger |
| attempts / steering budgets | **keep boundedness (I3); re-key** | budget keyed to task identity instead of attempt input — the pardon-manufacturing error (§1a) |
| pardons / `resume` | **delete** | pure artifact of the budget keying (§1a) |
| grants / conflict-domain refusal | **modeling error** → retrospective certification | prospective declaration graded as runtime boundary (§1b) |
| conflict domains (declaration) | **keep as scheduling hint** | tally's genuine advance over wayfinder's "one-at-a-time is the safer default" (`wayfinder.md:75`) |
| gates | **keep** | D61: "machinery that makes agent output mergeable is in"; F33 priced their absence at 74 and 61 minutes |
| steward narration | **delete** | 0 for 35, 74 rejections, and the correct subject already exists on the lane tip 10/37 times (PA-25) |
| escalation report | **keep verbatim** | I3 (PA-43) |
| escalation latch | **delete** | enforcement arm of the budget-keying error (CA-11) |
| integration branch | **keep, restructure** | one line of development; amendments ride it; `main` moves by fast-forward only (§4) |
| publish | mechanize | last-mile ceremony — "the integration branch *is* the answer" (PA census row 11); re-gate the head (CA-8) |
| checkpoint / summary refs | **keep** | I2; summary names gain the graph digest (CA-7) and the archive ritual's cause is gone |
| trailers / completion identity | **keep; unify** | I2; VD-8's second contract was a modeling error inside the projection — execution policy leaves the identity (§4) |
| receipts | **keep; extend** | I2/I3; the automation's honesty substrate; must record *all* actors (PA-03: today 17% coverage) |
| release | **keep the verb; re-source** | its preconditions leak the lease error — it must read durable facts, not an armed registration (VD-16, PA-16) |
| probe | keep, fix verdict | PA-20: `releaseComplete` is the answer; teardown is a separate field; scopes preflighted |
| disarm | **delete** | lease lapse; `stop` for deliberate abandonment |
| steer | **keep** | I4; campaign-scoped, off-host, and budget-granting (§3) |

### 1a. Pardons — the concept does not survive re-keying the budget

The question as posed: if the attempt budget were scoped to the
(task, authority-epoch) pair, does the pardon concept simply not exist?

**Yes — and the correct key is slightly wider than the epoch: the attempt's
*input identity*.** An attempt runs against exactly three inputs: the task's
bytes in the admitted worklist (sha), the steering high-water mark addressed to
it, and the gate set. Key the two-attempt budget to
`(task, hash(task bytes, gates, steering seq))`. New input ⇒ fresh budget, no
verb, no actor. Same input twice-failed ⇒ the task is blocked *until its input
changes*, which is the honest statement of what a retry is for. The receipts
already almost carry this — CA-3: the ledger lacks only `worklistSha`,
`armSerial` (now: the input hash) and a timestamp, "a two-field schema change
that turns 'predates the grant' from an operator assertion into a `<`
comparison."

Replay all ten epsilon pardons under that key (reasons verbatim in PA-05):

- **4, 27, 30, 34** — "burned attempts predate the amendment/grant/fix": dead
  epoch, budget fresh under the new sha. *Gone.*
- **15, 19** — stale-pass races ("the previous pass snapshotted state before
  the pardon landed"): the receipt's input stamp is the old snapshot's; the
  attempt never counted against the live input. *Gone.*
- **10** — the F23 wake defect, already fixed by `poll-liveness-arm` (F41:
  zero wake-pardons in ε2). *Gone.*
- **20** — the D73 summary-ref collision, deleted by digest-in-the-ref-name
  (CA-7). *Gone.*
- **18** — "all fourteen implementation lanes are merged, fleet-gate is proven
  green": releasing a latch the machine's own facts said to release. Under
  notify-don't-latch there is nothing to release. *Gone.*
- **7** — follows the commit-first steer. A steer addressed to a task *is* new
  input, so it grants fresh budget by the key itself. *Subsumed by steer.*

Nine mechanical, one subsumed. The machine already computes half of this:
`amendment_pardon_plan` (`crates/tally/src/cli/campaign.rs:4134-4188`) diffs
the graph — but only `dependencies` — and when a grant falls outside that
single case it prints, at `:4183`, `"task {task_id} remains escalated; run
tally campaign resume to unblock"`. That line is the entire ceremony: **the
machine computes the conclusion and instructs the human to type it.**

**What breaks, honestly:**

1. *Churn risk* — an amend-fail-amend loop mints fresh budgets forever. Answer:
   a lifetime per-task attempt backstop, a generous system constant in the
   spirit of ultracode's 1000-agent cap (~10 lifetime attempts, then a latched
   escalation a human must answer). I3 survives intact: two attempts per input,
   ten per life, report at every boundary.
2. *Trivial-delta freshness* — a worklist commit touching an unrelated task
   must not refresh every budget. Answer: task-*affecting* delta only, which is
   exactly the computation `amendment_pardon_plan` already does for
   dependencies; widen it to the task's own bytes, domains, and gates (CA-2's
   ~20-line widening).
3. *"One more try, nothing changed"* — the human override `resume` covered
   this. It does not deserve to exist: a retry with byte-identical input is a
   coin flip, and if the operator has *anything* new to say, that is a steer —
   which grants the budget. Delete `resume` entirely rather than keeping it as
   a vestigial override; the record contains zero uses that were not either
   mechanical or a steer's shadow.

### 1b. Grants — prospective refusal is the error; and the planned auto-grant is a band-aid on it

The record could not be cleaner. CA-1: the ownership gate fired four times in
epsilon, **4-for-4 against correct work, zero rogue writes on record, ever**;
`ownership-preflight-warn` caught 0 of 4 in advance (F40), and F40's `Cargo.lock`
row proves a whole correction class *no* prospective lint can see (the file is
regenerated by the toolchain, unnamed by any task text). Meanwhile the
retrospective alternative ships in the same flow file and runs today for serial
tasks — `examples/flows/spec-build.js:2197`: *"Ownership will certify its
committed paths, and the tree-delta gate will allow exactly those owned paths
after ownership runs"* — and the tree-delta gate calls itself *"detective, not
preventive"* (`spec-build.js:3002`).

**So: yes, prospective refusal is a modeling error.** Worktrees already isolate
the work (ultracode's own answer); git already detects collisions at merge; the
gates already judge correctness. Grading a scheduling *prediction* as a
runtime *boundary* converts an authoring imprecision — a consumer the author
did not foresee — into a latched campaign that only a human keyboard can
release at 00:21, 07:18 and 10:28 (CA headline).

The final shape:

- **Declaration stays, as a scheduling hint.** Disjoint-domain frontier
  construction is tally's real advance over wayfinder ("in practice
  one-at-a-time is the safer default," `wayfinder.md:75` — tally made parallel
  actually safe). The brief still carries the declared footprint as guidance.
  A wrong hint costs parallelism (lanes serialize, merges defer — the deferral
  behavior already exists: "Merges defer while a sibling agent holds the base,"
  AUG14 operational notes), never correctness, never a human.
- **Certification is retrospective.** Ownership certifies the committed paths;
  the recorded domain widens to the observed set with a receipt; the scheduler
  learns.
- **Refusal survives for exactly two things, both machine-checkable facts, not
  predictions:** (i) a **live-sibling collision** — the committed set
  intersects paths a dispatched sibling holds; the machinery defers or
  serializes, retry priced as machinery, not as the agent's fault; (ii) the
  **protected set** — the worklist file and the gate definitions, the bytes
  that *are* the authority. This is I1's real content, and it is a campaign
  constant, not a per-task declaration. (This keeps CA-14's separation: the
  constrained party still cannot write its constraint.)

Under this, the grant concept has nothing left to govern. There is no boundary
to widen for correctness, so there is nothing to grant, so there is nothing to
automate. **This is where EPSILON-EXTENSION is not at the level:** its ext1
centerpiece, the capped auto-grant — five clauses, machinery-authored worklist
commits, receipts naming attempt and diagnosis — is an automated bureaucrat
operating a gate that should not exist. It automates the transcription (the
aug-4 meta-trap in mechanism form: governance apparatus built to serve
governance) instead of deleting the concept that demands transcription. The
same judgment applies to `needs-grant` as a sixth failure class (VD-1): with no
grants, there is nothing to need. What survives of VD-1 is only a structured
final-message outcome envelope so a *deliberate stop* is distinguishable from a
crash — worth having for protected-set refusals and genuine impossibility, and
it deletes `projectionWaitMs` tuning (VD-2) as a side effect.

What genuinely requires declaration-in-advance in a multi-harness world:
**nothing that grades correctness.** Only the concurrency choice (who may run
together) needs a prediction, and predictions may only cost throughput.

### 1c. Arm/disarm — a lease, exactly as the lineage says

The pessimistic-safety note that inspired tally is explicit about whose job
reclamation is: *"Just like a Mutex guard, when the task finishes (or crashes),
`pls` reclaims the lease, releasing the VRAM or cloud slot for the next process
in line"*; *"the process crashes, its state is discarded, and a supervisor
restarts it from a known-good clean slate"*
(`aug4-coding-lessons/chat-lessons-oldprojects-tally-inspiration.md`). D52
already filed the design ("the backoffice lease… pid-liveness reclamation") as
"filed, not built."

The current registration is the opposite: an operator-managed object whose
terminal act (`disarm`) is documented as mandatory, destroys the auto-pardon
baseline (F17), and is a *precondition violation* for the act that follows it —
`campaign_release_plan` requires an armed registration, an
integration ref *named after the current registration id*
(`stable_publish_branch`, `campaign_folds.rs:188`), checkpoint refs, and a live
closing summary; PA-16 shows three of five preconditions destroyed by the
documented close ceremony and bridged by a hand-typed `git branch` at 12:04:36.

Final shape: **a registration is a lease binding one committed worklist to one
host's resources (checkout, adapter catalog, agent-slot pool).** Acquired by
`run`, renewed by liveness, reclaimed automatically at `complete`+quiescent or
at crash; receipts and refs outlive it, and nothing durable is ever named after
it (the registration-id-in-refname of PA-22 goes; refs key on campaign + graph
digest, which the merge refs already do). `release` reads durable facts alone.
What remains of the verb surface: `run` — the deliberate doorbell (register +
admit the current committed sha + dispatch); the poll re-admits any *new*
committed sha on the same identity (CA-4: one `ls-remote` plus one blob hash;
`armSerial` bumps itself as a derived counter); `stop` for deliberate
abandonment, which `campaign-operator/SKILL.md:93` already half-says ("Do not
use disarm as failure recovery"). `disarm` and re-arm-as-a-human-act delete.

Whether *activation itself* should be a committed fact (a worklist present
under a designated path ⇒ eligible, `run` reduced to a convenience) is left
open — it is the fully-derived endpoint of I1, but the doorbell is cheap,
deliberate, and the record shows all four arms were deliberate design moments
(PA census row 1: "n/a — deliberate"). Recommend keeping the doorbell.

---

## 2. THE CAPS, FROM FIRST PRINCIPLES

The design goal: **an author should almost never type a number.** Grading each
against the four categories (host-derived lease / generous system constant /
authored judgment / knob standing in for a missing mechanism):

| number | today | category | final shape |
|---|---|---|---|
| `maxTasks` | authored per worklist (24 in epsilon); default 64 (`campaign_contract.rs:21`); hard `MAX_CAMPAIGN_TASKS: usize = 128` (`:22`) | **(ii)** | **delete the authored field.** Its only failure mode is an error message instructing you to raise it — `"campaign contains {} tasks but manifest maxTasks is {} — raise \"maxTasks\""` (`campaign_contract.rs:359-362`, VD-30). A number whose failure instructs you to change the number checks nothing. The runaway backstop is the un-authored constant 128, exactly ultracode's 1000-agent cap. |
| `maxParallel` | authored (3 in epsilon); default 1 (`:894`) | **(i)** | **host-derived.** How many concurrent agents a machine bears is a property of the machine and the subscription, not of the worklist — the pls lineage: agent slots are a pool the host declares once and campaigns lease from. The record shows the authored number was already epiphenomenal: "maxParallel 3 is honest for ε2 and was dishonest for ε1 — ε1's deletion wave is near-serial by domain overlap regardless of the setting" (AUG14). Effective parallelism = min(host pool, disjoint-domain frontier). No per-worklist number. |
| `runtimeMaxSec` (agent) | default 14 400 (`campaign_contract.rs:24`), rarely authored | **(iv)→(ii)** | the knob stands in for a missing **output-liveness watchdog**. A wall clock kills healthy long lanes and spares chatty dead ones; the real predicate is "no forward progress for N minutes" (the F43 session deaths *between finishing and committing* are exactly what a liveness watchdog sees and a wall clock does not). Ship the watchdog with a constant window; keep a generous wall-clock backstop as an un-authored constant. |
| `runtimeMaxSec` (per gate) | authored per gate (900/3600/1800/10800 in `epsilon.json`) | **(i)-shaped** | derive from observed duration × slack — the receipts and captures already hold every historical gate duration; before history exists, one generous constant. An authored override stays *legal* for the odd 3-hour gate, never *required*. |
| `NARRATION_ATTEMPTS = 2` | hardcoded (`actions.rs:31`) | — | deletes with the narrator (§4). The mechanism that replaces it (adopt the lane's own valid subject; deterministic formatting) has no attempt count at all. |
| two-attempt steering budget | baked into flow schemas (`spec-build.js:1174` et al., `maximum: 2`) | **(ii)** | **keep, as the per-input constant** — PA-44 shows it earning its keep. Plus the new lifetime backstop (~10, constant). Neither is authored. |
| `projectionWaitMs` | 10 000 default, per-registration scalar (`campaign_registry.rs:29-30`) | **(iv)** | exists because a refusal has no channel (VD-2's own verdict: "the honest fix is VD-1 — a refusal that emits a structured envelope needs no extra window at all"). With structured outcomes it demotes to an adapter plumbing constant and leaves the registration surface. |

**Numbers that survive anywhere:** five system constants nobody authors
(128 tasks; 2 attempts per input; ~10 lifetime attempts; liveness window; wall
backstop) and one host declaration made once (agent-slot pool capacity). The
worklist's required numeric surface is **zero**. The `campaign` section keeps
only judgment: gates (argv), agent/steward names resolving against the host
catalog, merge method.

---

## 3. THE OPERATOR SURFACE UNDER THE INTERN TEST

**There is no standing supervisor.** The resident components are the poll (a
systemd timer), the notifier, and an inbox. The frontier model is summoned,
never parked — the inversion the operator asked for: the smart model does
episodic design; the standing surface is intern-shaped or absent.

What the human sees and does across a campaign's life:

1. **Authoring** (episodic; frontier model and/or human). Write the stage
   worklist against the observed tree (F42 — staged authoring is the week's
   best idea and stays), commit. This is design work and is exempt from the
   intern test by the operator's own framing.
2. **Start**: `tally campaign run owner/repo worklist`. A doorbell. Intern.
3. **While running — the only standing channel is the notification inbox**,
   and every item on it must satisfy the intern test by construction:
   - *Task blocked (budget exhausted on current input).* The notification IS
     the escalation report, kept verbatim (PA-43), **plus the machine's
     prepared next action when it has one** — and the record says it almost
     always does: diagnosis accuracy 15-for-15 / 18-for-18 / 16-for-16
     (PA-32), and of epsilon's 12 worklist commits, 8 were "verbatim from a
     diagnosis" (PA census row 5). A prepared worklist diff (an amendment task
     minted from a gate diagnosis, a gate-set fix) rides in the report as a
     ready diff. The reply set is exactly **approve** (machinery commits the
     prepared diff to the integration branch; fresh budget follows from the
     input key), **deny** (task stays blocked), or **steer** (free text — the
     one act where a human with taste beats the machine, 2-for-2 on the
     record). No new artifact class: the diff lives in the report, the reply
     in the steering log, the commit in git.
   - *Stage complete / published.* Informational; names the published sha and
     the gate proof. The follow-on ("author the next stage") is scheduled
     design work, not supervision.
   - *Lifetime backstop hit / protected-set breach / incident.* The escalate
     arm: summon engineering (frontier or human). Epsilon had 3 such events in
     19 hours (the out-of-band repairs), each a genuine engineering session
     ending in an ordinary gated change. Episodic by nature.
4. **Release** (episodic): the machine renders the full plan (persisted, PA-15
   fixed); preflight has already verified `gh` scopes (PA-20) and printed any
   remedy verbatim (`gh auth refresh -h github.com -s delete_repo` — an
   intern-typable line). Reply: approve/deny.

**Where the frontier model genuinely belongs:** worklist authoring at stage
boundaries (scheduled, ~3 sessions per campaign) and incident response (rare,
summoned). Both episodic. It never watches.

**Steer without a coordinator keyboard (D12).** Steer is the one verb whose
absence from a phone is a real regression (CA-10). Final shape: the steering
log becomes **campaign-scoped** (PA-04's registration-scoped log silently
discarded the taskdb-flooding insight at a stage boundary — that class of loss
ends), and the poll ingests steers/replies from a designated inbox the phone
can reach (an authenticated drop — a spool file fed by the notification
channel's reply path, or a pushed git ref; implementation detail, one
requirement: **append-only, authenticated, ingested by the poll, durable in
the campaign's ledger once ingested**). The approve/deny replies of (3) ride
the same channel — one inbox, one log, no second apparatus.

The wager this surface makes, stated plainly: epsilon proved the machine's
conclusions were correct essentially every time a human transcribed them
(PA-05 nine of ten, CA-2 nine of ten, PA-32's diagnosis streaks, PA-25's ten
discarded-but-valid subjects). The final shape stops routing those conclusions
through a keyboard, and reserves the human for the two verbs whose judgment the
record shows are real: authoring, and steer.

---

## 4. THE FINAL VERB SURFACE, STATE MODEL, AND THE EPSILON REPLAY

### Verbs (five, plus read-only)

```
tally campaign run     OWNER/REPO WORKLIST    # acquire lease, admit committed sha, dispatch;
                                              # poll re-admits new shas and re-acquires after crash
tally campaign steer   … [--task ID] [--approve PROPOSAL]   # the judgment channel; grants fresh input-budget
tally campaign stop    …                      # deliberate abandonment only
tally campaign status  … [--json] [--wait-for CONDITION]    # + query/rebuild; quiescent stays as the predicate
tally campaign release … [--plan|--probe]     # reads durable facts only; no registration required
```

Deleted outright: `resume` (§1a), `disarm` (§1c), the grant ritual and its
planned automation (§1b), the archive ritual (digest-named summaries), the
publish ritual (below), re-arm as a human act, the narrator (below), the
escalation latch (notify + input-keyed budgets + lifetime backstop).

### Durable facts (CA's must-survive list, amended)

1. **The committed worklist** — now living on the campaign's integration
   branch (see below), still committed bytes at a named revision the lanes
   cannot write; each admitted sha is an epoch in the receipts.
2. **Checkpoint and merge refs** — unchanged; **summary refs gain the graph
   digest in the name** (CA-7), which deletes the archive step and D73's
   collision at the root.
3. **Trailers, with one completion contract.** VD-8's two-tuple split resolves
   in the writer's favor: identity =
   `{contractVersion, repository, source, task}` — the *work*, not the
   *policy*. Execution policy (`gates`, `agent`, `steward`, `mergeMethod`)
   leaves the identity, otherwise every amendment rotates every merged proof
   (PA-14) and D74's "changing a gate is a worklist commit" stays reversed.
   The exact oracle comes back to life; the bridge demotes to the legacy path
   it was named for; `completionProofs` and the plan document persist into the
   release record (PA-15's unverifiable `planSha256` becomes verifiable).
4. **The receipts JSONL** — campaign-scoped, append-only, now stamped with
   `worklistSha`/input-hash + timestamp + actor, and now recording **every**
   act: admissions, dispatches, merges, publishes, steers, approvals, lease
   transitions, releases. PA-03's 17% operator-act coverage goes to 100% — the
   ledger becomes the answer to "what happened to this campaign, in order,
   including what the human did" (PA-42's missing index).
5. **The steering log** — campaign-scoped, survives stages, ingests off-host.
6. **The integration branch — restructured to the only line of development.**
   Today the campaign merges to integration while operator amendments land on
   `main`, so the proven and published shas diverge every stage (F31), the
   release names a commit unreachable from `main` (PA-21), grants race live
   lanes (F22's push hazard), and a hand rebase with `shas.txt`/`bodies.txt`/
   `@@@END@@@` bridges the gap (PA-08). Final shape: **worklist amendments are
   commits on the integration branch** (written by the coordinator machinery
   on approval, or by the operator; never by lanes — the protected set holds);
   lanes branch from and merge to it; the chapter gate proves its head; and
   `main` advances **only by fast-forward of a gate-proven head**, performed
   by the machine, with the publish receipt recording the sha (one sha now,
   not a pair). PA-21, F31, PA-08, and CA-8's "the published sha has never
   been gated" all close structurally. Ultracode precedent: merges on green
   are the approval; the gates are the bar. (If the operator wants a human
   click before `main` moves, that is one more intern-shaped approval per
   stage on the same inbox — a policy choice, not architecture; recommend
   auto.)
7. **Archived captures** — unchanged, first-line forensics.

Commit subjects: the narrator deletes. The squash layer validates the lane
tip's own subject and adopts it when it parses (PA-25: agents wrote 10 valid
conventional subjects in 37 and the machine discarded all 10); otherwise the
template. Deterministic formatting replaces the reject-and-fallback validator
(PA-26: 74 rejections, 100% machine-repairable). No model call in the merge
path at all.

### Lifecycle

commit worklist → `run` (lease) → [poll: admit new shas; dispatch frontier by
host pool ∩ disjoint hints; lanes commit; ownership certifies retrospectively;
merges defer on live-sibling collision; gates prove; input-keyed budgets bound
retries; blocked ⇒ notification with prepared diff] → gate-proven head → machine
fast-forwards `main` + publish receipt → `complete` ⇒ lease lapses → facts
remain → `release` from facts, whenever, by anyone.

### EPSILON REPLAYED — every human act that remains, graded

Same 40 tasks, same 8 escalation-worthy episodes (F36's table), same 3
out-of-band repairs, same release. Machine work identical; only the human
column changes.

| # | event (as it happened) | act that remains in the final shape | intern test |
|---|---|---|---|
| 1 | 3 stage authorings (`1953bb49`, `3f3f8525`, `4309acc1`) | 3 authoring sessions at boundaries | design — exempt (episodic frontier) |
| 2 | D77 policy ruling ("remove that roundabout way") | 1 design ruling | design — exempt |
| 3 | `run` per stage | 3 doorbell commands (or 1, if stages are three worklists) | **pass** — a typed command, no judgment |
| 4 | ε0 gate fails on forge-native fleet-gate; machine drafts `gate-local-audit` | notification + prepared amendment diff → **approve** | **pass** — full text arrives; approve/deny |
| 5 | `squash-rowversion-ladder` session deaths ×2 | notification (budget exhausted, same input) → **steer** ("commit first, verify second"); budget refreshes by the key | **pass** — steer is the reserved judgment verb; phone-reachable |
| 6 | grant #1 (`daemon/tests.rs`) | *nothing* — lane edits the file, ownership certifies, merges | — |
| 7 | ε1 gate clippy; machine drafts variant-box amendment verbatim | notification + prepared diff → **approve** | **pass** |
| 8 | grant #2 (`producer_query.rs`, agent-requested) | *nothing* | — |
| 9 | ε1 gate final-bar stale; machine enumerated all four repairs | notification + prepared diff → **approve** | **pass** |
| 10 | grant #3 (`Cargo.toml`/`Cargo.lock` cohort) | *nothing* — lockfile is ordinary work, certified retrospectively | — |
| 11 | grant #4 (`delete-python-driver`, 5 files enumerated) | *nothing* | — |
| 12 | ε0 shakedown steer | 1 **steer** | **pass** |
| 13 | 10 pardons | *nothing* (§1a) | — |
| 14 | ~9 re-arms, 4 arms' admission half | *nothing* — poll admits committed shas | — |
| 15 | 3 publish-rebases + `shas.txt`/`bodies.txt` harness | *nothing* — machine ff after re-gate; notification | — |
| 16 | 12 summary-ref archive writes/deletes, 1 integration-ref hand-restore, ≥1 checkpoint-ref restore | *nothing* — digest-named refs; release reads durable facts | — |
| 17 | 4 disarms (1 premature) | *nothing* — lease lapse | — |
| 18 | monitor loop rebuilt twice; 0-byte `jobs.json` | *nothing* — `status --wait`/notifications | — |
| 19 | F39 fleet-down: rollback + ghorigin repair | 1 incident → engineering session | **escalate arm** — episodic by design |
| 20 | D77 arm-self-contained repair; F44 bridge repair | 2 incidents → engineering sessions | **escalate arm** — episodic |
| 21 | 4 hand-run fleet gates; PRs #604–606 as gate fodder | *nothing* — repairs ride gated campaigns/PRs; local-audit arm kills the throwaway-PR class (PA-36) | — |
| 22 | 2 deploys + 1 rollback (host generations) | 2 machine-prompted deploys at quiescence (D63's ExecCondition exists); the rollback belongs to incident #19 | **pass** — prompted, quiescence-guarded |
| 23 | probe 403 at teardown | preflight before any repo exists prints `gh auth refresh -h github.com -s delete_repo` → operator types it once | **pass** — exact text arrives |
| 24 | release: plan → probe → execute | notification with full rendered plan → **approve** | **pass** |
| 25 | the 86-minute close (PA-29: ~14 acts, zero machine work) | *does not exist* — items 15–17, 23–24 absorb it | — |

**Residual human acts: ~16** (4 design, 2 steers, ~4 approvals, 3 doorbells,
2 prompted deploys, 1 typed auth line — plus 3 episodic engineering
escalations). **Zero acts require a frontier model resident.** Every standing
touchpoint arrives with its full text prepared and takes approve/deny/steer.
The count matches CA's independent ~16 estimate while deleting the four
grant-commits CA still charged to the operator — the difference is §1b's
deletion of the refusal rather than its automation.

The iteration demanded by the assignment ("if any act still needs a frontier
model resident, the design is not done") terminates here: rows 19–20 are the
only frontier work left outside authoring, and they are summoned engineering
with a report in hand, not supervision. The design is done by that criterion.

---

## 5. WHAT THE MIGRATION COSTS

**Survives unchanged (the large majority of the mature codebase):** the folds,
the gate machinery and captures, the refs plumbing, the receipts ledger
(schema bump only), the driver's diagnosis/redaction/escalation machinery (the
best component, F36), the daemon/pools/lease substrate (D62 — the lease model
*extends* it), the adapters (PA-40/41: the interchangeable-adapter thesis is
the proven part), the release rendering, `status`/`quiescent`/`query`, the
flow's node graph in bulk.

**Deletions and reworks, LOC-scale guesses:**

| change | scale |
|---|---|
| refusal branch out of the ownership node; retrospective certification becomes the only path (it already exists for serial tasks); live-sibling check added | ~-300/+150 in `spec-build.js` + driver |
| `resume`/pardon verb + latch; budgets re-keyed to input hash; lifetime backstop | ~-400/+200 across `campaign.rs`, flow, driver |
| `disarm` → lease lapse; `stop`; release reads durable facts; registration id out of ref names | ~-200/+400 (D52's filed design; the largest single build item) |
| narrator + `NARRATION_ATTEMPTS` + reject-validator → subject adoption + formatter | ~-600/+150 |
| publish mechanized (re-gate + ff) on the single-line integration model | ~+300, deletes the scratchpad harness and the F31/PA-21 class |
| summary digest in ref name | ~10 lines |
| receipts stamps + full-actor coverage | ~+200 |
| completion-identity unification (writer's tuple; policy out) | ~-100 net; the bridge demotes |
| `maxTasks`/`maxParallel`/`projectionWaitMs` off the authored surface; liveness watchdog | ~-100/+250 |
| notification/inbox + off-host steer ingestion (D12) | ~+300, the one genuinely new component |

Net: the system gets smaller. Every addition gates a named unattended-operation
capability; none is process apparatus.

**EPSILON-EXTENSION under ratification** (what changes in the staged plan):
ext0 survives nearly whole — `receipt-authority-stamp`,
`summary-ref-stage-digest`, `final-bar-executes`, `fleet-gate-cheap-first`,
`squash-subject-adoption`, `authoring-doctrine-skills` are all load-bearing
here; `narrator-honest-contract` is **replaced** by narrator deletion (E5's
keep-and-measure re-answered: the proof was 0-for-35 and the valid subject
already exists on the tip); `needs-grant-outcome` shrinks to the structured
final-message envelope (no grant plumbing). ext1 is **re-founded**: poll
re-admission and release-from-durable stay; the capped auto-grant is replaced
by refusal-deletion; the latch change becomes the input-keyed budget; publish
lands on the single-line integration model. ext2 stays as planned (identity
unification, estate-population replay, probe honesty, lease semantics = the
§1c build, test isolation). E-decisions re-answered: E1 yes (writer's tuple);
E2 **no — delete the refusal instead**; E3 yes, via re-keying; E4 yes, and it
is ext1-adjacent, not deferrable to ext2, because release-from-durable and
lease-lapse are the same model; E5 delete; E6/E7 yes and overdue (PA-01: the
record of the machine's best week is one `git clean` from gone); E8 yes — D12
stops being optional the moment the latch is gone, because the inbox *is* the
operator surface.

**Open questions honestly held** (rulings, not designs, all Tom's):

1. Auto-ff of `main` on gate-proven heads vs. one approval per stage (§4 item
   6). Recommend auto; the record shows publish carried zero judgment.
2. Activation-as-committed-fact vs. the `run` doorbell (§1c). Recommend the
   doorbell now; the lease model leaves the door open.
3. The protected set's exact contents beyond {worklist, gate definitions} —
   e.g. `.github/` — one line in the campaign section, authored once.
4. VD-18 (a `forge:"local"` campaign still requires a remote push to arm)
   interacts with the single-line integration model — the authority fetch
   should accept the integration branch of the local checkout's own remote;
   needs one deliberate ruling on what "local" promises.

**The one-line verdict.** Tally's machinery was never the problem and its
ceremony was never the cause — the cause is three wrong models (identity-keyed
budgets, prospective boundaries, operator-managed registration), and once they
are corrected the ceremonies do not need automating, absorbing, or supervising,
because they do not exist. The supervisor is not demoted to an intern; the
supervisor is deleted, and what remains for the human is the work only the
human was ever actually doing: authoring, steering, and reading the close.
