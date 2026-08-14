# August 14 — the ceremony audit: every verb and ritual in tally's operator
# surface, judged against wayfinder, the aug-4 lessons, and the ultracode baseline

Prepared for **epsilon-extension**, against `mecattaf/tally.nix` at `e921cccc`
(GitHub release `0.0.0+20260814092311.8b45283` "epsilon", published 2026-08-14
09:23Z by tally's own `campaign release` verb). Findings are prefixed **CA-** and
are independent of the F1–F44 series; where they rest on an F-finding they cite
it.

Primary sources read in full: `skills/engineering/wayfinder/SKILL.md` and
`docs/engineering/wayfinder.md` from `~/Downloads/mpocock-skills` (plus its
`README.md` and `CLAUDE.md`); all three files in
`~/mecattaf/notes/aug4-coding-lessons/`; `AUG14-LEARNINGS.md`;
`SILENT-FACTORY-PLAN.md` Parts 1–2 and 7. Machine evidence read directly:
`~/.local/state/tally/campaigns/attempt-receipts/epsilon/attempt-receipts-v1.jsonl`
(34 receipts), the two `campaigns/steering/*/steering-v1.jsonl` logs,
`crates/tally/src/cli/args.rs:180-201`, `crates/tally/src/cli/campaign.rs:4134-4188`
and `:6124-6152`, `examples/flows/spec-build.js:2184-2205` and `:2995-3012`,
`skills/campaign-operator/SKILL.md`, `skills/assign-tally/SKILL.md`, and
`git log -- silent-factory-worklists/epsilon.json`.

---

## The headline

**The ownership gate inverted the rule it was built to enforce.** F22's rule is
*"a task must own every file its change makes false"* — an **authoring**
obligation. What shipped is a runtime refusal: *a task may not touch a file it
does not own*. Under H1 (the brief now carries `conflictDomains`) agents comply
by **not making the change** — they leave the tree in the state where the
assertion is false, the task incomplete, and the campaign latched. An incomplete
ownership declaration, which is an authoring bug, became a **deadlock whose only
release is a frontier-model operator typing two commands at 00:21 and 07:18 and
10:28 local time.**

Every one of epsilon's four grants was a *correct* change being refused. Not one
was a rogue write. The gate's four firings this run have a 4-for-4 false-positive
rate against the work the campaign was authored to do, and `ownership-preflight-warn`
found **zero of four** in advance (F40). Meanwhile tally already ships the
ultracode-shaped alternative *in the same flow file* — retrospective certification
of what a lane actually committed inside its own throwaway worktree
(`examples/flows/spec-build.js:2197`) — and uses it today for serial tasks.

The second-order finding is worse and simpler: **9 of the run's 10 pardons are
mechanically derivable, and tally already implements both primitives needed to
derive them.** It just doesn't wire them together.

---

## Score — the operator surface, verb by verb

| Verb / ritual | Where it lives | Operator acts in epsilon | Verdict |
|---|---|---:|---|
| `campaign arm` (first arm) | `args.rs:183` | 3 | **KEEP** — the doorbell; ultracode's `pipeline()` launch |
| **re-arm** (authority re-admission) | same verb | ≥12 (ε2 alone at `armSerial` 5) | **AUTOMATE** — right invariant, wrong agent (CA-4) |
| `campaign steer` | `args.rs:185` | **2** | **KEEP** — the one honest human channel (CA-10) |
| `campaign resume --reason` (pardon) | `args.rs:187` | **10** | **REPLACE** — retry budget + authority-change auto-pardon (CA-2) |
| auto-pardon at re-arm | `campaign.rs:4134` | machine | **KEEP, widen** — diffs dependencies only (CA-2) |
| conflict-domain **refusal** | `spec-build.js:2202` | 4 grants, ~8 burned attempts | **REPLACE** — worktree + retrospective certification (CA-1, CA-5) |
| conflict-domain **scheduling** | prep/frontier | — | **KEEP** — this is tally's genuine advance over wayfinder (CA-5) |
| **grant** (edit + commit + push + re-arm + pardon) | ritual, no verb | 4 × 5 acts | **AUTOMATE** with a bounded cap (CA-6) |
| escalation / frontier-quiescent **report** | driver `escalate` | 3 | **KEEP** — best artifact in the system |
| escalation **latch** | flow | 3 (each needs a pardon) | **REPLACE** — notify and continue (CA-11) |
| **archive-summary-refs** | ritual, no verb | ≥4, botched 2 | **DELETE** — pure defect ceremony (CA-7) |
| **publish-rebase** | ritual, no verb | 3, every stage, forever | **AUTOMATE** — and it hides a real hole (CA-8) |
| `campaign disarm` | `args.rs:200` | 3 (one premature) | **REPLACE** — lease expiry (CA-9) |
| `campaign release` | `args.rs:189` | 1 (+1 probe) | **KEEP** — the model of a good verb |
| `campaign status` / `list` / `query` | `args.rs:194/196` | continuous | **KEEP** |
| `campaign quiescent` | `args.rs:198` | automated | **KEEP** — it already *deleted* a ceremony |
| `campaign poll` | `args.rs:191` | systemd | **KEEP** |
| per-lane + chapter **gates** | worklist `campaign.gates` | — | **KEEP** — this is D61's "in" (CA-11) |
| **steward narration** | driver `narrate` | 0 successes / 35 merges | **DELETE until proven** (CA-12) |

Nine CLI verbs and five uncodified rituals. Six verbs and zero rituals survive
the audit intact.

---

## Headline measurements — the operator-act ledger

Counted from primary artifacts, not from the run narrative:

- **12 worklist commits** (`git log -- silent-factory-worklists/epsilon.json`),
  timestamps 08-13 15:28 through 08-14 11:10 local. Each one is edit → validate →
  commit → push → **re-arm**, and 8 of the 12 were followed by a pardon.
- **10 pardons** in the receipt ledger (AUG14 counted 9 mid-run; the ε2 close
  added the tenth). **2 steers**, total, across the whole run.
- **3 escalations**, **3 machinery-fault retries** (free — no operator act; see
  CA-2), **18 machine diagnoses**.
- **ε2's registration reached `armSerial` 5** for a single stage of a single
  campaign.
- **3 publish-rebases** (`6fdf108f`, `b4e655c8`, `a8077295`) — in every case the
  proven sha and the published sha are different commits (F31).
- **≥4 summary-ref archive operations**, of which the ε1-close execution was
  incomplete and the flaw recurred live at the ε2 tail (F38).
- **≈40 tasks settled** (ε0 4 + ε1 14 + ε2 19, plus 3 chapter gates).

Rough total: **~55 discrete operator acts for ~40 tasks — 1.4 acts per merged
task**, in a system whose stated destination is a *silent factory*. Roughly
**34 of those 55** are the ones this audit finds mechanically derivable.

---

## CA-1 — The ownership gate reversed F22's rule, and epsilon's record shows it firing only against correct work

F22's rule, from `AUGUST-12-LEARNINGS.md:139`, is an authoring rule:

> **a task must own every file its change makes false**

What the flow enforces is the converse. From `examples/flows/spec-build.js:2202`,
verbatim, as rendered into every agent's brief:

> `The task's conflictDomains ${JSON.stringify(projected)} are the binding write
> boundary: files your change makes false must be inside these prefixes; anything
> else is the operator's to grant.`

"Anything else is the operator's to grant" is the whole finding in seven words.
An authoring omission — a consumer the author didn't predict — is converted at
runtime into a state only a human can leave.

The four grants, verbatim from their commit messages:

| grant | commit | what was refused | who found it |
|---|---|---|---|
| `daemon/tests.rs` → rowversion | `663de5bc` | "a restart-stability test that loads the deleted legacy-no-origin fixture" | machine (cargo-tests diagnosis) |
| `producer_query.rs` → variant-box | `1324eaa4` | "the lane's agent refused to commit across its ownership boundary … naming producer_query.rs:283 as the missing grant" | **agent, verbatim** |
| `Cargo.toml`/`Cargo.lock` → 5 port lanes | `ef0443f8` | "dependency additions update the lockfile **by construction**" | machine |
| `crates/tally`, `crates/tally-flow`, `nix/lib` → delete-python | `05aec25d` | "the complete missing grant … packaging and test references" | **agent, verbatim** |

Each grant commit touches **exactly one file** and adds **3 to 16 lines** to a
JSON array. Two of the four were the *agent* dictating the answer to the operator,
who typed it back in.

**Not one of the four was a rogue write.** I looked for a counter-example in the
epsilon record and found none: no receipt, pardon or learnings entry describes the
ownership gate stopping a change that would have corrupted a sibling lane. The
gate's own designers already know this class exists only as a *detective* concern —
`spec-build.js:3002` calls the tree-delta gate "detective, not preventive."

Against the aug-4 rule this is textbook: *"A process artifact may exist only when
it is a hard gate for a named feature or capability."* The named capability is
"parallel lanes don't collide." The refusal does not deliver it — worktree
isolation and merge-conflict detection do. What the refusal delivers is
**pattern 9, spec-editing as progress**, inverted: the campaign cannot proceed
until the operator edits the spec.

Verdict: **REPLACE** the refusal. Keep the declaration (CA-5).

## CA-2 — The pardon is a manual re-implementation of two primitives tally already ships

All ten pardons, classified by what human judgement they actually carried
(reasons quoted from the receipt ledger):

| # | verbatim reason (excerpt) | judgement content |
|---:|---|---|
| 4 | "Re-armed graph added dependency `gate-local-audit` to escalated task `chapter-gate`" | **none** — this is the machine's own auto-pardon text |
| 7 | "after steering it to commit before verification; two prior attempts died between completing the patch and committing it, with the flooding taskdb suite the likely session killer" | **real** |
| 10 | "Woke the resting frontier … (the F23 shape whose fix this campaign carries but the deployed pin predates)" | none — machinery defect |
| 15 | "the prior rejection came from an in-flight pass holding the pre-grant snapshot" | none — race (F37) |
| 18 | "all fourteen implementation lanes are merged, fleet-gate is proven green on this tree" | none — three facts the machine holds |
| 19 | "the previous pass snapshotted state before the pardon landed" | none — race |
| 20 | "Cleared the stage 0 summary refs that collided with stage 1 … a D73 single-identity flaw for the record" | none — defect (CA-7) |
| 27 | "both burned attempts predate the lockfile grant" | none — derivable |
| 30 | "its burned attempts predate the full consumer-set grant" | none — derivable |
| 34 | "both burned attempts predate the schema-example lint fix, which is now merged" | none — derivable |

**One of ten carried judgement a machine could not have produced** — receipt 7,
and even that was a *steer*; the pardon merely released the latch the steer had
already answered.

Four of the remaining nine (27, 30, 34, and 4) say the same sentence in different
words: *the attempts that burned the budget were run under an authority that no
longer exists.* Tally already has a verb for that thought. From
`crates/tally/src/cli/campaign.rs:4134-4188`, `amendment_pardon_plan` computes
exactly this — but it diffs **only `dependencies`**:

```rust
let added_dependencies = task
    .dependencies
    .iter()
    .filter(|dependency| !previous.contains(dependency.as_str()))
```

A grant changes `conflictDomains`, not `dependencies`, so every grant fell
through the auto-pardon and out the warning path, which prints (`:4183`):

> `format!("task {task_id} remains escalated; run tally campaign resume to unblock")`

**The machine computes the correct conclusion and then instructs the human to
type it.** That is the ceremony, exactly, in one line of Rust.

And the other primitive already exists too. The ε2 escalation body carries a
section headed **"Campaign machinery faults that bought a retry"** — three
`result-schema-mismatch` faults on `port-worktrees` and `port-fold-half` bought
free retries with **zero operator involvement**. Tally therefore already has (a) a
no-human retry budget for one fault class and (b) an auto-pardon for one graph-diff
class. The pardon verb is what covers the gap between them.

Verdict: **REPLACE**. Widen `amendment_pardon_plan` to any task-affecting graph
delta; widen the machinery-fault retry into a per-task budget; keep
`resume --reason` as a rarely-used manual override, not the recovery verb the
supervision playbook calls it ("Recovery verb is `resume --reason`, **always**" —
`SILENT-FACTORY-PLAN.md:954`).

## CA-3 — The receipt ledger is missing the one field that would make auto-pardon automatic

`jq -r 'keys|join(",")'` over all 34 epsilon receipts returns exactly four shapes:

```
actor,campaign,issueNumber,kind,nonce,reason,schemaVersion,sequence,tasks     (pardon)
attempt,campaign,diagnosis,issueNumber,kind,redaction,schemaVersion,sequence,taskId
attempt,campaign,issueNumber,kind,reason,redaction,schemaVersion,sequence,taskId
body,campaign,issueNumber,kind,schemaVersion,sequence
```

There is **no timestamp, no `armSerial`, and no worklist digest on any receipt.**
Four pardons say "these attempts predate the grant" — a claim the ledger itself
cannot express, which is why a human had to make it. This is a two-field schema
change (`armSerial`, `worklistSha`) that turns "predates the grant" from an
operator assertion into a `<` comparison.

Aug-4's own rule applies to the ledger: *"the machinery wasn't worthless — it was
unbounded. Keep the checks that catch defects."* The receipts JSONL is the check
that catches defects (F36: 16 diagnoses, zero wrong causes). It is one schema
version away from also closing the pardon loop.

Verdict: **KEEP and extend** — the highest ratio of value to effort in this audit.

## CA-4 — Re-arm is authority admission wearing the start verb's clothes

`arm` does three unrelated things: register the campaign, **admit a worklist
digest as authority**, and start a pass. The middle one is the only reason it was
run ≥12 times. F29 elevates this to doctrine:

> **re-arm, never disarm.** Disarm destroys the auto-pardon baseline (F17);
> re-arm on the same identity records the amendment delta as a durable receipt
> and preserves completed tasks. `armSerial` is now the honest count of how many
> times a stage's authority changed underneath it — ε2's registration sits at
> serial 5.

"The honest count of how many times a stage's authority changed" is a good
property. It does not require a human to produce it. `poll` already walks the
registry on a heartbeat and already knows the base branch and worklist pattern;
detecting *the committed worklist at `origin/main` has a new sha* is one
`git ls-remote` + one blob hash. Admitting it, bumping `armSerial`, and writing
the amendment receipt is the machine's job.

The invariant — **authority is committed bytes at a named revision, never
working-tree bytes** (`skills/assign-tally/SKILL.md:52-56`) — is correct and must
survive; it is the only thing that makes the worklist legible to a second harness.
The *typing* is what should go.

Verdict: **AUTOMATE**.

## CA-5 — Conflict domains are a scheduling input that got promoted to a correctness gate — and tally already ships the alternative

The strongest evidence is in tally's own flow. `examples/flows/spec-build.js:2197`,
the brief text for a task that declares **no** domains:

> `This serial task omits conflictDomains. Ownership will certify its committed
> paths, and the tree-delta gate will allow exactly those owned paths after
> ownership runs.`

And `:3002-3010`:

> `#386: fingerprinted before the agent ran (prep), compared against the
> worktree's content right now -- **detective, not preventive**. Runs after
> ownership so an absent conflictDomains can fall back to
> ownership.result.ownedPaths, the paths the ownership node just certified as
> this task's own committed change-set.`

That is the ultracode model, already implemented, already reachable: **worktree
isolation + retrospective certification of what the lane actually committed.** No
prospective declaration, no refusal, no grant, no pardon. It is disabled the
moment a task declares domains — i.e. always, because
`skills/assign-tally/SKILL.md:31` requires them whenever `maxParallel > 1`.

So what do the declarations actually buy? **Frontier scheduling**: "frontier =
first maxParallel ready tasks with disjoint conflict domains"
(`SILENT-FACTORY-PLAN.md:139`). That is a genuine capability and it is tally's
real advance over wayfinder, which concedes the opposite — `docs/engineering/wayfinder.md:75`:

> *"The frontier is built to show you what is takeable, and blocking edges are
> there so parallel work is safe on paper. **In practice one-at-a-time is the
> safer default.**"*

Tally made parallel actually safe. Keep that. But note what epsilon measured about
its own parallelism (AUG14, operational notes):

> **`maxParallel 3` is honest for ε2 and was dishonest for ε1.** ε1's deletion
> wave is near-serial by domain overlap regardless of the setting.

For an entire stage — 14 lanes, 22,968 lines deleted — the domains bought no
parallelism at all and cost two grants, two pardons and two re-arms.

Verdict: **KEEP** the declaration as a scheduling hint; **REPLACE** the refusal
with the retrospective path that already exists. A lane whose committed change-set
exceeds its declared domains should merge, and the *scheduler* should learn from
it: widen the recorded domain, record a receipt, and refuse only if a **sibling
lane in flight** actually touched the same path — which is a fact the machine can
check and a human cannot.

## CA-6 — The grant ritual has exactly one authority-bearing property, and the machine can produce it

`AUG14-LEARNINGS.md`'s grants glossary is the clearest statement of the current
doctrine, and it is honest about the hole:

> - **The machine can only diagnose.** On a failed attempt it prints the exact
>   paths a task would need. **It has no verb that can act on its own conclusion.
>   This remains the largest single unattended-operation gap in the system.**
> - **The agent can only request.** …
> - **The operator alone grants**, by making the commit.

And, decisively:

> **What a grant is *not*:** it is not a model change, not a permission
> escalation, not a sandbox or capability change, not a credential, not a
> widening of what any agent may run, read, or reach. … The nearest correct
> analogy is editing a `CODEOWNERS` entry, not issuing a token.

If a grant is a `CODEOWNERS` edit and not a token, the case for a human bottleneck
collapses to one property: **auditability** — "a diff you can read." A machine
commit to `silent-factory-worklists/epsilon.json`, signed with a receipt naming
the failed attempt and the diagnosis that produced it, is *more* auditable than
the current record, because it also carries the causal chain. The operator's
commit messages already contain nothing but the machine's or the agent's own
words: "Granted verbatim." (`1324eaa4`, `05aec25d`).

The honest counterweight, and it is real: auto-granting removes the only backstop
against a lane that widens itself to the whole repo. So bound it, per aug-4's
*"bound the machinery, then freeze it"*:

1. Only paths **enumerated by the machine's own diagnosis** or by a **structured
   agent refusal** (F35) — never paths the agent picked freely.
2. Only after a failed attempt, never pre-emptively.
3. Never a directory strictly above a declared domain; never the worklist file,
   `flake.lock`, `.github/`, or the gate definitions.
4. Never a path a sibling lane holds in flight.
5. Always a commit + receipt naming the attempt and diagnosis.

Anything outside that cap escalates to the operator — which is where the operator
belongs, and where the volume would have been **0 of 4** this run.

Verdict: **AUTOMATE**, capped.

## CA-7 — `archive-summary-refs` is a ritual invented to work around a naming defect, and it cannot be executed correctly

Fully diagnosed in F38 and worth restating as ceremony rather than as a bug:

> `git ls-remote origin` today returns exactly four refs in that namespace:
> ```
> summary/archive/eps0-complete
> summary/archive/eps0-quiescent
> summary/archive/eps1-complete      ← only complete was archived
> summary/quiescent                  ← still canonical, still colliding
> ```
> Root cause of the miss, and the ref set says it plainly: **`quiescent` is
> written by the disarm/quiescence act itself**, i.e. *after* the operator
> archives. **Archiving before the terminal operator act cannot work.**

A standing operator step that is impossible to perform in the prescribed order,
executed wrongly at 1 of the 2 boundaries it crossed, whose failure mode is a
driver refusing to reconcile a live stage. Meanwhile AUG14 records that the
**merge refs already do it right** — "each stage's merge refs carry their own
graph digest" — and only the summary digest is stage-invariant.

This is not a restriction that earns its keep or fails to; it is a missing
character in a ref name. AUG14's own ask ("make it one verb —
`tally campaign archive-summary <stage-tag>`") is the *wrong* fix by the no-ceremony
rule: it codifies the ritual instead of deleting its cause.

Verdict: **DELETE**. Put the graph digest (or `armSerial`) in the summary ref
name, exactly as the merge refs do. Zero verbs, zero steps, zero standing
operator notes.

## CA-8 — `publish-rebase` is an unautomated step that also hides an ungated publish

F31 states the mechanism and then, I think, under-prices it:

> The integration branch cuts from the base at arm. Worklist amendments land on
> `main` *after* that cut. The branch never absorbs them, so at ε0's publish the
> proven sha (`914c791f`) and the published sha (`6fdf108f`) are **different
> commits** … **It is not a defect** — the checkpoint ref pins the proven tree
> durably — but it means: … **The publish is an operator act with a rebase in it,
> every stage, forever, as long as amendments are how ownership is granted.**

Two things follow. First, "as long as amendments are how ownership is granted" —
CA-6 removes that clause. Second, and more sharply: **the sha that landed on
`main` was never run through the chapter gate.** The gate proved `914c791f`; the
world got `6fdf108f`. The safety argument is that the rebase is "content-disjoint"
— the worklist commits touch only `silent-factory-worklists/epsilon.json` — but
*nothing mechanically verifies that disjointness*. It is an operator's assertion,
made three times, at 17:20Z, 23:53Z and 08:04Z, unrecorded.

The ceremony is hiding a hole. The ultracode answer is boring: a `publish` verb
that rebases, **re-runs the gate on the rebased head**, records the published sha
beside the proven sha in the completion receipt, and fast-forwards.

Verdict: **AUTOMATE**, and add the re-gate.

## CA-9 — `disarm` destroys state, inverts a precondition, and should be a lease

Three facts from the record:

1. `SILENT-FACTORY-PLAN.md:954`: *"Never disarm-first — disarm destroys the
   auto-pardon baseline (F17)."* A terminal verb that silently destroys the
   forensic baseline is a trap, not a control.
2. F44 (`AUG13-RUN.md:997`): *"the release window requires an ARMED registration,
   so the identity was re-armed `--no-enqueue` after my premature disarm, and the
   integration ref restored under the new registration id."* The terminal act
   is a precondition for the act that follows it.
3. F29: *"campaign reaches `complete` but **STAYS ARMED**; disarm is the
   operator's terminal act."*

Contrast the design lineage tally came from. `~/mecattaf/notes/aug4-coding-lessons/chat-lessons-oldprojects-tally-inspiration.md`
is, in substance, an argument for **RAII leases and crash-fast reclamation**:

> *"Lease Lifecycle & Cleanup: Just like a Mutex guard, when the task finishes
> (or crashes), `pls` reclaims the lease, releasing the VRAM or cloud slot for
> the next process in line."*
>
> *"If an actor panics while processing an impure request, the process crashes,
> its state is discarded, and a supervisor restarts it from a known-good clean
> slate."*

The inspiration document's whole thesis is that reclamation is **automatic and
supervisory**, never a human's terminal act. `SILENT-FACTORY-PLAN.md:103` (D52)
already files exactly this design — *"the backoffice lease (leased daemon-less
control with pid-liveness reclamation)"* — as "filed, not built."

Verdict: **REPLACE** with lease semantics: a registration is a lease with a TTL;
`complete` + no dispatchable work + no live units ⇒ the lease lapses on its own,
receipts and refs outlive it, and the release window keys off the **completion
receipt**, not off an armed registration. Keep an explicit `stop` for deliberate
abandonment, as `campaign-operator/SKILL.md:93` already says: *"Do not use disarm
as failure recovery."*

## CA-10 — `steer` earns its keep, and it is the only verb that clearly does

Two steers in the entire run. Both logs, in full, are worth reading; the ε1 one
(`campaigns/steering/019ffc34…/steering-v1.jsonl`) is the single most valuable
operator act of epsilon:

> "Adopted a mechanical order for this lane after two attempts died between
> finishing the patch and committing it. **Commit FIRST, verify second** … Run
> the taskdb suite with its output tamed — for example
> `cargo test -p tally-core taskdb 2>&1 | tail -30` — because the raw suite
> floods hundreds of kilobytes of expected daemon-event JSON and that flood is
> the likely killer of the two prior sessions."

That is a human noticing a cross-cutting failure mode from *outside* the loop and
changing the shape of the work — the thing no diagnosis produced across 18
attempts. F43 promotes it to authoring guidance ("commit, then verify, then
amend"). It cost one command.

The other steer was a deliberate shakedown exercise. Two uses, 35 merges: this is
what a verb that earns its keep looks like — rare, cheap, decisive, append-only,
and never in the critical path.

The unpaid debt is D12 (`SILENT-FACTORY-PLAN.md:36`): *"a steer verb usable only
at the coordinator's keyboard is a regression against a phone."* Still deferred
by D75. In a system where the operator's remaining job is *judgement*, the
judgement channel is the one that should be reachable from a phone; the ritual
verbs are the ones that shouldn't exist at all.

Verdict: **KEEP**, and pay D12.

## CA-11 — The gates earn their keep; the escalation *report* earns its keep; the escalation *latch* does not

This is where I part company with the "it's all ceremony" reading, and the record
is unambiguous.

**The gates are D61's "in".** `SILENT-FACTORY-PLAN.md:575` states the doctrine
that resolves this whole audit:

> **D61 — The reconciling sentence:** *ceremony aimed at an imagined public
> audience is out; machinery that makes agent output mergeable is in.*

Gates make agent output mergeable. "Fails, then passes" is now 5-for-5 across
closed chapters. F33 prices the *absence* of a gate exactly: a gate-only lint
class cost **74 minutes** (clippy) and **61 minutes** (stale final bar), each a
full chapter-gate cycle plus an amendment task plus a re-arm. Ultracode's plain
`pipeline()` would have merged all of it green and discovered it later, in the
worst case in a release. **Keep the gates; move clippy and the final bar into the
per-lane set** — which, post-D77, is one worklist commit, not a deploy.

**The escalation report is the best artifact in the system.** From receipt 26's
body: directly-blocked tasks, descendant count, every accumulated diagnosis, the
machinery faults that bought retries, reconciler warnings, and links to archived
captures. AUG14: *"readable and honest at 3am."* Ultracode has no equivalent and
would be worse for it.

**The latch is the ceremony.** "Frontier quiescent" ends the pass and requires an
operator verb to release — three times this run, and 9 of the 10 releases carried
no judgement (CA-2). Under a retry budget the report is a *notification*; work
that is still dispatchable elsewhere in the graph keeps moving; only genuine
budget exhaustion on a task blocks that task's descendants.

The nearest external precedent, from the operator's own notes
(`~/mecattaf/notes/aug1-tally-iteration/main-chat.md:530-600` on Claude Code's
`/goal`), is exactly this shape and is worth stealing wholesale:

> "a typed verdict from a model that didn't do the work, standing between the
> worker and the exit, whose 'no' carries a steering reason back into a bounded
> retry loop, with an adversarially-guarded escape hatch for 'genuinely
> impossible.'"
>
> "a hard cap of 8 consecutive Stop-hook blocks ends the turn with a warning"

A cheap model deciding *"is this a boundary refusal, a crash, or genuine
impossibility?"* — a distinction F35 shows costs two burned attempts and an
escalation today because *"a refusal and a crash are the same signal"* — plus a
hard cap, is the whole pardon verb, automated, with the human reserved for the
cap.

Verdict: **KEEP** gates and report; **REPLACE** the latch.

## CA-12 — Steward narration is process porn with a model attached

0 successes in 35 merges. 70 validator rejections. The dominant class is not even
a quality problem — F32:

| rejection reason | count | share |
|---|---:|---:|
| final message is not valid JSON | **38** | 54% |
| leading sentence must end with a period | 11 | 16% |
| body wraps past 100 columns | 11 | 16% |
| header over the 72 cap | 6 | 9% |
| contains an exclamation mark | 2 | 3% |
| must open with a past-tense verb | 2 | 3% |

Two attempts, both spent, template proceeds. The template is valid; the commit
grammar holds; nothing downstream noticed. Across the largest-throughput run of
the ladder this seam produced **zero bytes of value and ~70 model calls**, and its
`!` ban re-gagged the steward on a rule F15 already won a carve-out for on the
diagnosis path.

D45 kept exactly one prose slot in the closing procedure and said the model was
"architecturally optional." Epsilon proved it optional by running the whole
release without it.

Verdict: **DELETE until proven.** Remove the slot; keep `template_narration`.
Reinstate only behind a shim that validates its own JSON locally and retries
before spending a campaign slot (F32 ask 1 — which alone recovers 54%). A seam
that has never once delivered is not a polish item; it is the aug-4 "process
artifact that gates nothing."

## CA-13 — What the ritual caught that ultracode's model would have missed

Being honest in the other direction. Four things in epsilon's record would have
gone wrong under a plain `agent()`/`pipeline()` fan-out with worktrees and
retries, and they are the parts of the design worth defending hardest.

1. **The committed worklist as cross-harness memory.** F34's correction is the
   most under-appreciated fact of the run: during ε1, agents honored boundaries
   H1 had not yet delivered, and the reason was *"`silent-factory-worklists/epsilon.json`
   is a committed file in the very tree each agent works in."* A JSON file at a
   named revision is legible to Codex, to Claude, and to a human, with no adapter
   and no protocol. That is precisely what "multi-harness ultracode" needs and
   what an in-process `pipeline()` cannot provide.
2. **The chapter gate plus in-campaign repair.** F28's pattern — *"when the
   gate's own contract is wrong, the repair is a new worklist task the campaign
   runs, not an operator hand-edit"* — held for all three gate cycles. Zero
   orchestrator hand-edits to tally source across the entire run. Ultracode's
   retry loop has no analogue; a wrong gate under plain retries is an infinite
   loop or a disabled gate (aug-4 pattern 1, gate self-weakening).
3. **The ownership *brief* (as distinct from the gate).** ε2's two refusals both
   named the **complete** missing set on the first refusal — five files across
   three unowned trees for `delete-python-driver` (receipt 29). That precision is
   what H1 bought, and it is what makes CA-6's automation safe. Keep the brief;
   it is the input to the auto-grant.
4. **Staged authoring against an observed tree.** F42 is the run's best idea and
   it is wayfinder's fog-of-war rule in tally's clothes: author ε2 only after ε1
   merged. Result: ownership corrections fell from **9 of 34 tasks (26%)** to
   **4 of 36 (11%)**, and to **2 of 18** for ε2 alone. Wayfinder names the
   failure this avoids in its own docs — *"I charted 27 tickets, and by the time
   I got to the thirteenth, the rest no longer made sense"* — and prescribes the
   same medicine: *"Wayfinder is 'prototypemaxxing', not 'planmaxxing'."* Tally's
   staged identity is a genuine, independent rediscovery. Keep it.

And one thing **no** amount of process caught, which the audit should not pretend
otherwise: F39's estate-bytes gap. A green chapter, a crash-looping fleet, and
*"nothing in the campaign could have seen it"* — in-lane gates and the chapter
gate both test freshly-constructed bytes. That is a **missing test class**, not a
missing ritual, and no verb in this audit would have helped.

## CA-14 — Where wayfinder is worse than tally, and where tally imported wayfinder's bug

Worth recording because epsilon-extension will be tempted to borrow from
wayfinder.

**Tally is better on one axis that matters enormously.** `docs/engineering/wayfinder.md:69`
names wayfinder's structural hole:

> "Wayfinder's 'plan, don't do' default can be overridden in the map's **Notes** —
> but the Notes are written by the agent, so **the constraint and its exemption
> live in the same file the constrained party owns.** One user watched an agent
> write 'this map carries execution' into its own Notes and then read it back in
> later sessions as its own licence, building on a live server."

Tally closed exactly this hole: *"It cannot widen its own boundary; the ownership
gate is what stops it, and **the gate reads committed bytes it cannot write**"*
(AUG14 grants glossary). That separation is the load-bearing invariant, and
CA-6's automation must preserve it — the auto-grant is performed by the **campaign
machinery**, from the machine's own diagnosis, not by the lane agent that wants it.

**Tally imported wayfinder's other bug.** Wayfinder's `task` type is described as
"the one type that *does* rather than decides," and its docs say *"This is the type
that goes wrong most often in practice: agents interpret it as an implementation
step and start writing product code inside the map."* Tally's worklist is 100%
implementation tickets — which is fine, tally is a build system, not a planner —
but the *plan document* (`SILENT-FACTORY-PLAN.md`, 996 lines, 77 numbered
decisions, seven parts, three supersession layers) is a wayfinder map that never
handed off. Aug-4's meta-trap warning is the exact diagnosis:

> **Meta-work is maximally seductive to sophisticated agents.** … Expect this
> failure mode *especially* when the assignment is itself about process quality.
> … **Bound the machinery, then freeze it.**

D53 already rules all documentation out of scope. The plan document should be
frozen at epsilon's close and epsilon-extension should get a **new, short**
document, not a Part 8.

---

## The minimum viable verb set — "tally as multi-harness ultracode"

Six verbs. No rituals. No standing operator steps.

```
tally campaign run     OWNER/REPO WORKLIST   # lease: register, admit, dispatch,
                                             # re-admit on authority change, retry
                                             # within budget, auto-grant within cap,
                                             # gate, merge, publish, lapse
tally campaign steer   OWNER/REPO WORKLIST [--task ID] --message-file -
tally campaign status  OWNER/REPO WORKLIST [--json]     # + tally query / rebuild
tally campaign release OWNER/REPO WORKLIST [--plan|--probe]
tally campaign quiescent                                # predicate for automation
tally campaign stop    OWNER/REPO WORKLIST              # deliberate abandonment only
```

What each deleted verb becomes:

| deleted | becomes |
|---|---|
| re-arm | `run` re-admits when the committed worklist sha at the base moves; `armSerial` bumps itself |
| `resume --reason` | per-task retry budget + auto-pardon on any authority change (CA-2/CA-3) |
| grant ritual | capped auto-grant from the machine's diagnosis or the agent's structured `needs-grant`, committed with a receipt (CA-6) |
| archive-summary-refs | stage digest in the ref name (CA-7) |
| publish-rebase | `run`'s publish stage, with a re-gate of the rebased head (CA-8) |
| `disarm` | lease lapse at completion (CA-9); `stop` for the deliberate case |
| escalation latch | notification + continue; only budget exhaustion latches (CA-11) |

**Epsilon under this surface.** The operator's total act list would have been:

1. Three stage authorings (`1953bb49`, `3f3f8525`, `4309acc1`) — real work, kept.
2. Four amendment-task commits (`19bd53af`, `482ff524`, `c848d491`, `aa9f6213`) —
   real work: each mints a *new task* the campaign then runs. Kept.
3. `run` × 3 (one per stage) — or once, if stages are three worklists.
4. **Two steers.**
5. Two deploys, one rollback, three out-of-band Codex repairs (PRs #604/#605/#606)
   — host and source work, outside the campaign surface either way.
6. One `release`, one probe.

**≈16 acts instead of ≈55.** The 39 removed acts are precisely the ones performed
at 00:09, 00:21, 01:24, 03:38, 07:18, 08:36 and 10:28 local time — the ones that
required a frontier model to be awake, and none of which, on this record, carried
a judgement a machine could not have made.

Throughput would not have changed. Every removed act sat in the *latency* path
(a lane blocked, waiting for a human to transcribe the machine's own conclusion),
not the *work* path.

## What must survive the cut — git/GitHub is the only shared memory between harnesses

The durable substrate is non-negotiable precisely because tally's premise is that
a Codex lane, a Claude lane, and a human all read the same facts without an
adapter. These are load-bearing, and this audit deletes none of them:

1. **The committed worklist at a named base revision** — authority, task graph,
   and the boundary the agent reads out of its own checkout (F34). Working-tree
   bytes are never authority (`assign-tally/SKILL.md:52-56`).
2. **Checkpoint and merge refs** pinning proven trees
   (`refs/tally/spec-build/v1/<digest>/…`) — they survived a generation rollback
   and made "proven sha ≠ published sha" recoverable rather than lost (F31).
   Fix the summary-ref naming (CA-7); keep everything else.
3. **The trailer block on every merged commit** (`Tally-Task`, `Tally-Revision`,
   `Tally-Receipt`, `Assisted-by`) — F44 proved this is load-bearing *across
   generations*: the Python driver wrote the trailers, the Rust verb verified
   them, and the release could only be rescued because the bytes were durable and
   inspectable. This is the single strongest argument in the whole record for
   git-as-shared-memory.
4. **The attempt-receipts JSONL** — the diagnosis ledger (16 diagnoses, zero wrong
   causes, F36) and the audit trail for every automated act this audit proposes.
   **Must gain `armSerial` + `worklistSha` + a timestamp** (CA-3) or the
   automation cannot be honest.
5. **The local integration branch** as the completion oracle (D14/D15).
6. **The archived captures** under `~/.local/state/tally/capture/archive/` — every
   gate diagnosis links its own; first-line forensics.

Three things must be **added** for the cut to be safe:

- **A structured `needs-grant` outcome** in the agent's final message (F35 ask 1).
  Today a refusal and a crash are the same signal, which is why the machine spends
  two attempts before the real cause surfaces. This is the input the auto-grant
  reads.
- **Authority stamping on receipts** (CA-3).
- **Publish re-gating** (CA-8).

---

## Asks / decisions

Blunt, ordered by ratio of ceremony removed to work required.

1. **Stamp `armSerial`, `worklistSha` and a timestamp on every attempt receipt.**
   One schema bump. Unlocks 4 of 10 pardons immediately and makes every later item
   auditable. *(CA-3)*
2. **Widen `amendment_pardon_plan` (`campaign.rs:4134`) from `dependencies` to any
   task-affecting graph delta.** ~20 lines. Removes the pardon after every grant,
   and deletes the `"run tally campaign resume to unblock"` warning that is the
   ceremony in one line of code. *(CA-2)*
3. **Put the stage digest in the summary ref name and delete the standing archive
   step entirely.** Do *not* build `tally campaign archive-summary` — that
   codifies the ritual. *(CA-7)*
4. **Ship `needs-grant` as a first-class agent outcome, then auto-grant within the
   five-clause cap.** This is AUG14's decision 5 plus the missing half: the agents
   already produce the content, the machine already produces the enumeration, and
   the operator adds nothing but latency. Keep the "gate reads bytes it cannot
   write" invariant — the machinery grants, never the lane. *(CA-6, CA-14)*
5. **Make re-admission the poller's job; stop re-arming by hand.** *(CA-4)*
6. **Turn the escalation latch into a notification plus a per-task retry budget,**
   with a cheap model classifying refusal-vs-crash-vs-impossible at the exit, and
   a hard cap that escalates to the human. Steal `/goal`'s shape verbatim. *(CA-11)*
7. **Add clippy and the final bar to the per-lane gate set** — one worklist commit
   post-D77; would have erased two of this run's three gate cycles (135 minutes).
   *(F33, CA-11)*
8. **Fold the publish rebase into the machine and re-gate the rebased head.** The
   published sha has never been gated, three stages running. *(CA-8)*
9. **Delete the steward narration slot** until the shim validates its own JSON
   locally. 0 for 35. *(CA-12)*
10. **Replace `disarm` with lease lapse; key the release window off the completion
    receipt, not off an armed registration.** D52 already filed the design.
    *(CA-9)*
11. **Pay D12 (off-host steer).** If the operator's remaining job is judgement,
    the judgement verb is the one that must reach a phone. *(CA-10)*
12. **Freeze `SILENT-FACTORY-PLAN.md` at epsilon's close.** Epsilon-extension gets
    a new short document, not a Part 8. Aug-4's meta-trap: *"bound the machinery,
    then freeze it."* *(CA-14)*

**The one-line verdict.** Tally's *machinery* — worklist-as-authority, gates,
receipts, refs, trailers, staged authoring against observed trees — is sound and
is the part that makes multi-harness orchestration possible at all. Tally's
*ceremony* — arm/re-arm as re-admission, pardon, grant, archive, publish-rebase,
disarm — is the machine's own conclusions being routed through a human keyboard
because the last mile was never written. Nine of ten pardons, four of four grants,
and every re-arm are transcription, not judgement. Cut them and epsilon runs the
same, unattended, at ~16 operator acts instead of ~55.
