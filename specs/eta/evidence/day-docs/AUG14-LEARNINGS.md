# August 13–14 learnings — chapter 3-epsilon: the local factory, the H1 boundary era,
# and the estate the tests never see

Context: the silent-factory ladder continued in `mecattaf/tally.nix` as the staged
**chapter 3-epsilon** pass — one campaign identity (`silent-factory-worklists/epsilon.json`,
D73), three stages, two deploys — from 2026-08-13 13:28Z (Part 7 authored at
`6927848c`, on the chapter 2 close at `52eff4db`) through 2026-08-14 late
morning. **Result at the time of writing: ε0 CLOSED (4 lanes + gate, published
`6fdf108f`), ε1 CLOSED (14 lanes + gate, published `b4e655c8`), ε2 at 17 of 18
lanes merged with `delete-python-driver` granted, re-armed at 08:36:50Z
(`armSerial` 5) and running, and the chapter gate blocked behind it. 35 campaign
merges, 2 out-of-band Codex repairs (PR #604, #605), 4 ownership grants, 3
in-campaign amendment tasks, 9 pardons, 2 escalations.** Findings are numbered
continuously with `AUG13-RUN.md` and `AUGUST-12-LEARNINGS.md` (F27–F43) so they
stay linkable; F1–F26 are taken.

**The headline is not the throughput,** though it is the largest throughput of the
ladder: 22,968 lines deleted in ε1 and 24,603 added in ε2 inside fourteen hours.
It is that **F22's fix — a worker that can see its own write boundary — changed
the failure mode of the whole system from silent violation to explicit refusal,
and the refusal has no first-class signal.** Every remaining friction in this run
is either that gap, or a class of state the tests do not look at.

## Score

| Layer | Verdict |
|---|---|
| local-mode campaign (D77 arm, steer, correction cycle, completion) | **sound on first exposure.** Every shakedown item passed; the two frictions found (F30, F31) are both in the operator's read/publish path, not in dispatch, ownership or merge |
| tally diagnosis layer | still the best component. 16 diagnoses this run; 14 named cause *and* correct remedy, 2 named the state correctly and the remedy late |
| worklist ownership contract | **cost collapsed** — 4 corrections in 36 tasks vs 9 in 34 last run (F42) |
| ownership boundary enforcement (H1) | working, but its refusal is indistinguishable from a crash (F34/F35) |
| steward narration | **0 for 35.** Every merge fell back to the template (F32) |
| chapter gate | 3 failure cycles, 3 in-campaign repairs, 2 passes; ε2's gate has not run. "Fails, then passes" now 5-for-5 closed chapters |
| durable-estate compatibility (D33) | **1 fleet-down defect** (F39): deletions merged green and crash-looped the daemon |
| campaign identity reuse (D73) | **1 structural flaw** (F38), and it has now fired at both stage boundaries |
| Codex as campaign agent | fast on small lanes, dies on flooding ones, no adapter-level defect (F43) |

## Headline measurements

- **36 implementation tasks authored, 35 merged** (ε0 4/4, ε1 14/14, ε2 17/18),
  3 chapter gates, across ~17 wall-clock hours of campaign time (ε0 armed
  15:23Z on 08-13, ε2's last merge 08:04Z on 08-14).
- **ε1 deleted 22,968 lines against 2,851 added** across 80 files, 18:17Z→23:53Z.
  One lane (`delete-gh-inbound-core`) deleted **14,741 lines in 32 files** and
  merged inside 75 minutes.
- **ε2 has added 24,603 lines against 2,943 deleted** across 81 files in 17 lanes
  between 02:10Z and 08:04Z — **~21 min per lane at `maxParallel 3`**.
- **11 worklist commits**: 3 stage authorings, 1 policy section (D77), **4
  ownership grants**, 3 new amendment tasks minted from gate diagnoses.
- **30 attempt receipts**: 16 machine diagnoses (8 episodes × 2 attempts),
  2 escalations, 9 pardons, 3 machinery-fault retries.
- **2 out-of-band repairs**, both Codex, both through the full fleet gate and a
  PR: `e8720ef6`+`7c2b4954` (D77, PR #604) and `40957154` (D33 decode tolerance,
  PR #605). **Zero orchestrator hand-edits to tally source** — every non-lane
  commit on `main` this run is a worklist or plan document.
- **3 system generations**: 125 (deploy-1 + D77, 15:21Z) → 126 (deploy-2,
  crash-looped, 00:49Z) → 125 (rollback) → 127 (deploy-2 retry, 01:35Z, live).
- **70 steward-narration rejections across 35 merges.** Zero narrated subjects.

---

## F27 — D77: the worklist owns campaign policy; per-campaign host declaration is forbidden

Part 7 was authored with a `services.tally.campaigns.epsilon` dotfiles
declaration prepared and ready. The operator rejected it outright ("remove that
roundabout way"). The mechanism was not worked around — it was **deleted**:
`local_campaign_declaration_from_document`, the config-document scan that
discovered a campaign by finding an enabled `kind:"gh"` producer named
`campaign-<name>` whose `enqueue.brief` carried the worklist path.

`tally campaign arm <owner/repo> <worklist>` is now self-contained: policy lives
in the worklist's closed `campaign` section (name, maxTasks, maxParallel,
mergeMethod, runtimes, agent, steward, and 1–16 gates), adapter names resolve
against the host adapter catalog, flow and driver default to the packaged assets
beside the binary, the mutex is the reserved minted `campaign/<owner>/<repo>`
pool, and `--checkout`/cwd binds the repository into registration authority v4.

Two consequences that paid immediately:

1. **Changing a gate is a worklist commit, never a deploy** (D74 as amended).
   ε1 and ε2 both re-armed with amended gate sets without touching the host.
2. **Dotfiles never carries campaign config.** The three-stage identity is a
   single committed file; each stage's exact bytes are pinned by its completion
   receipt hash and recoverable from git history.

The load-bearing observation: this was the single most consequential decision of
the run and it was made by *deleting* a mechanism two independent design agents
had both identified as central. The cheapest campaign mechanism is the one that
does not exist.

## F28 — the fleet gate is forge-native by construction; a local-first head fails it three ways

D77's own repair PR found it first: `test/fleet-gate.sh` exits 2 on a commit not
resolvable through `gh api`, and its changelog-policy stage fails any head that
is neither a pull-request head nor the tip of `main`. D77 got through by opening
PR #604 and re-running — three fleet-gate runs to land one commit.

ε0's chapter gate then hit the identical wall from inside a campaign, and the
machine predicted it exactly on both attempts (receipts 1–2):

> "a publication-order failure: `test/fleet-gate.sh` queried GitHub for the local
> HEAD before running any ladder stage, but that commit was not available
> remotely… No source fix is indicated; the final conformance bar never ran
> because the fleet gate exited first."

Repaired **in-campaign**, not out-of-band: the first live worklist amendment of
the run (`19bd53af`) minted `gate-local-audit` (owns `test/fleet-gate.sh`,
depends on `gate-keep-going`), which classifies a local integration head as a
local audit. The chapter gate gained a dependency on it and passed (`6fdf108f`).

**The pattern to keep:** when the gate's *own contract* is wrong, the repair is a
new worklist task the campaign runs, not an operator hand-edit. All three gate
cycles this run were repaired this way; only the estate regression (F39) needed
an out-of-band worker.

## F29 — the ε0 shakedown ledger: local-mode semantics, settled before they were needed in anger

Three disjoint tasks at `maxParallel 3`, armed 15:23Z, closed ~17:20Z, plus one
amendment task. Every checklist item was exercised deliberately:

| item | verdict |
|---|---|
| **local arm** — self-contained, packaged assets resolved, registration v4 | ✓ |
| **task-addressed steer** — recorded seq 1, dispatch fence honored | ✓ |
| **worklist-correction cycle** — edit → validate → push → **RE-ARM same identity, never disarm**; autoPardons recorded the amendment delta with a durable receipt; 3 completed tasks preserved | ✓ (settles OQ3) |
| **completion semantics** — campaign reaches `complete` but **STAYS ARMED**; disarm is the operator's terminal act; base advance is the operator's publish (machine pushes only the checkpoint ref) | ✓ |
| **F18 large-id regression** | structurally absent in local mode — steering ids are small sequences. Noted, not pinned |

The correction cycle is the one to internalize, because it ran **six more times**
before the run was over — every mid-stage worklist amendment (`663de5bc`,
`482ff524`, `1324eaa4`, `c848d491`, `ef0443f8`, `05aec25d`) is one turn of it:
**re-arm, never disarm.** Disarm destroys the auto-pardon baseline (F17); re-arm
on the same identity records the amendment delta as a durable receipt and
preserves completed tasks. `armSerial` is now the honest count of how many times
a stage's authority changed underneath it — ε2's registration sits at serial 5.

## F30 — `campaign status` renders the newest pass, not the newest truth

ε0's sharpest usability finding. After a steer or a re-arm, the latest pass is a
queued, un-reconciled one, and `status` renders *that*: empty table, zero counts,
placeholder name "Campaign campaign". Truth lived in `tally query run <pass-id>`.
Together with the two blind spots recorded last run, this made the single most
frequently consulted verb the least trustworthy one at exactly the moments an
operator consults it.

Fixed by ε1's `status-renders-reconciled-truth` (H4, `ac38c4bd`), live since
deploy-2 (gen 127). **Confirmed working:** mid-escalation at the ε2 tail, with
`delete-python-driver` blocked on its two boundary refusals and the chapter gate
pending behind it, `status` rendered the real reconciled table — `17 done,
0 running, 1 blocked, 1 pending`, every task named — minutes before the stage's
fifth arm (registration `armedAt` 2026-08-14T08:36:50Z). That output was the
primary forensic surface for F38's recurrence.

## F31 — the integration branch does not absorb operator commits pushed mid-campaign

The integration branch cuts from the base at arm. Worklist amendments land on
`main` *after* that cut. The branch never absorbs them, so at ε0's publish the
proven sha (`914c791f`) and the published sha (`6fdf108f`) are **different
commits**, and the publish required a content-disjoint rebase onto the main tip.

Both later stages needed the same manoeuvre (ε1 published `b4e655c8` from
integration head `6afee3aa`). It is not a defect — the checkpoint ref pins the
proven tree durably — but it means:

- **Never assume the checkpoint sha is what lands.** Record both.
- The publish is an operator act with a rebase in it, every stage, forever, as
  long as amendments are how ownership is granted.

## F32 — the steward narrated nothing: 35 of 35 merges fell back to the template, 70 validator rejections

ε0 finding 2 opened as "narrator shim likely failing headless". Two probes killed
that theory (the shim's model call works headless *and* under a scrubbed
unit-like env). The seam works end to end. **The proposals are being refused by
the deterministic commit validator, twice per merge, at which point the slot is
spent and the task-id template proceeds.**

The reasons are recorded verbatim in every merged commit body — genuinely
excellent observability, and the reason this is countable at all. Counted across
all 35 epsilon merges (every one of which carries a
`Rejected 2 steward narration proposal(s)` line):

| rejection reason | count | share |
|---|---:|---:|
| final message is not valid JSON | **38** | 54% |
| proposal body leading sentence must end with a period | 11 | 16% |
| body wraps past 100 columns | 11 | 16% |
| header is N characters, over the 72 cap | 6 | 9% |
| proposal body contains an exclamation mark | 2 | 3% |
| proposal body must open with a past-tense verb | 2 | 3% |

**The mid-run diagnosis was wrong about the dominant class.** It named the header
cap and unwrapped bodies — together 17 of 70, 24%. The actual leader is
**malformed JSON in the final message, 38 of 70**, which no amount of prompt
budgeting fixes: the model is failing to emit the envelope, not failing to write
a good subject.

Two more things fall out:

- **F15's bang rule still gags the steward.** ε0's `steering-grammar-negation`
  (`2d68fca9`) permitted `!` inside inline code for *machine diagnoses*; the
  steward narration validator still rejects it outright. Two rejections this run.
- The remedy that "rides deploy-2" (dotfiles narrator-shim hardening) **did not
  ride deploy-2** — deploy-2 was a tally pin bump; the shim lives in dotfiles and
  is still unhardened. ε2's most recent merge, at 08:04Z, still fell back.

Asks:
1. Fix the envelope first — the shim should validate its own JSON and retry
   locally before spending a slot. That alone recovers 54% of the rejections.
2. Give the narrator more than two attempts, or make a rejection not consume the
   slot; the current budget guarantees fallback the moment two independent rules
   fire.
3. Prompt the model with the *real* budget (a `type(scope):` prefix against a
   72-char cap is ~48 chars of subject), pre-fold bodies at 100 columns, and lift
   the `!` ban into the same inline-code carve-out F15 already won.

## F33 — gate-only lint classes: what the lane gates never run, the chapter gate finds at full price

D74's per-lane gate set is `driver-suite`, `cargo-tests`, `flake-eval`. **None of
them runs clippy.** ε1's deletions left a 744-byte `Calendar` variant beside a
32-byte `EventsDir` variant in `producers/config.rs`, and the chapter gate failed
two attempts on `clippy::large_enum_variant` under `-D warnings`. The same
structural gap produced ε1's second gate cycle: fleet-gate passed whole while
**12 of 24 final-bar cases** asserted pre-deletion contracts.

The machine named the exact prescription both times, down to the constructor
sites (`producers/tests.rs` and `producer_query.rs`) and the four final-bar
repairs. Both were adopted **verbatim** as new amendment tasks
(`producers-config-variant-box`, `final-bar-stage1-reseat`) with the chapter gate
made to depend on them.

The economics, measured from the archived checkpoint captures: a gate-only lint
class costs one full chapter-gate cycle (2 attempts) plus one amendment task plus
a re-arm — **74 minutes for the clippy cycle (22:06Z → 23:20Z) and 61 for the
final-bar cycle (23:20Z → 00:21Z)**. Two fired this run; the run's third gate
cycle was F28's publication-order defect, a different cause.

Asks:
1. Add `cargo clippy --workspace --all-targets -- -D warnings` to the per-lane
   gate set. It is a worklist commit now (F27), not a deploy.
2. The final bar's *only* flake attribute runs `--list` and executes no case.
   That is how it stayed broken across an entire chapter. Either run it, or stop
   pretending it has coverage.

## F34 — the H1 era: boundary refusals replace silent violations — and H1 was not live when it was credited

The night's central observation, and it needs one correction.

**The observation, which holds:** from partway through ε1, every attempt that
"died without committing" (`squash-rowversion-ladder` ×3, the variant-box fix
lane, later `port-fold-half` and `delete-python-driver`) turns out to have been
**an agent honoring its ownership boundary**. When completing a task required an
out-of-domain edit, agents left valid in-domain work uncommitted and said so,
rather than committing a violation. Two of the four grants this run were
**agent-requested and adopted verbatim**, in the operator's own commit messages:

- `1324eaa4` — "the lane's agent refused to commit across its ownership boundary
  … naming `producer_query.rs:283` as the missing grant. First boundary refusal
  of the H1 era; granted verbatim."
- `05aec25d` — "The lane refused across its boundary and named the complete
  missing grant: `crates/tally`, `crates/tally-flow`, and `nix/lib`…"

This is F22's fix proving itself: silent boundary violations, the dominant cost
of chapters 0–2, no longer happen.

**The correction:** the run record credits this to `brief-carries-conflict-domains`
(H1, `9da2539d`) merging as ε1's third task. Mechanically that cannot be the
cause. The driver is resolved from the deployed store path, and every ε1 lane ran
under generation 125 (created 15:21Z; ε1's last merge was 23:53Z, gen 126 not
until 00:49Z), whose driver is
`/nix/store/ck6c4b86…-spec-build-driver` → `bw30aqfd…-tally-campaign-drivers`.
Diffed against the post-deploy-2 driver (`cvkk5gpp…`, reached through
`6cpg864n…-spec-build-driver`), the ε1-era driver has **none** of H1's brief
rendering: `grep 'workspace\["conflictDomains"\]|identity\["conflictDomains"\]|result\["conflictDomains"\]'`
returns nothing in `bw30aqfd`, and three sites in `cvkk5gpp` (`:916`, `:5034`,
`:5059`). H1 landed on the integration branch, not in the running driver.

So during ε1 the boundary awareness came from somewhere else — almost certainly
that `silent-factory-worklists/epsilon.json` is a committed file in the very tree
each agent works in, plus the ownership gate's rejection naming the offending
path. **ε2 is the first stage where H1 is genuinely live** (gen 127 = `40957154`,
which git confirms carries all of `b4e655c8`), and ε2's two refusals — both
naming their *complete* missing grant on the first refusal — are the first real
evidence of what the brief buys.

Ask: re-read the ε2 refusals as the H1 baseline, not the ε1 ones. If agents can
already find their boundary by reading the worklist in their checkout, the brief's
marginal value is precision (naming the *whole* missing set at once), which is
exactly what ε2 shows and ε1 did not.

## F35 — a refusal and a crash are the same signal

The polish gap the H1 era exposes. A boundary refusal surfaces as:

1. an attempt that produced no commit (HEAD at base revision), and
2. `result-projection-timeout` — "configured finalMessage capture for adapter
   `spec-build-driver` was not projected within 10000 ms" — because the agent
   exits without the envelope the publish stage waits for.

Three of the 30 receipts are exactly this fault shape, bought as retries
(receipts 21–23: `port-worktrees` ×1, `port-fold-half` ×2). It is
indistinguishable at the machinery level from an agent that crashed mid-edit, and
the first diagnosis in each episode reads it as the latter — *"the completed,
in-scope changes remained uncommitted while HEAD stayed at the base revision…
commit all intended edits"* (receipts 5, 6, 13, 28). Correct description, wrong
remedy; the real blocker only surfaces on the second escalation.

Asks:
1. **Make "needs-grant" a first-class outcome.** The agent already knows it is
   refusing and already names the path. A structured refusal in the final message
   would convert two burned attempts plus an escalation into one clean signal.
2. `projectionWaitMs` is 10000 on this registration (confirmed in the live
   authority-v4 record). A refusing agent times out before it can narrate;
   consider a longer window for the no-commit path so the refusal reaches the
   receipt rather than the fault.

## F36 — machine diagnosis, counted exactly: 16 diagnoses, 8 episodes, zero wrong causes

Every escalation this run, from the epsilon receipt ledger
(`~/.local/state/tally/campaigns/attempt-receipts/epsilon/attempt-receipts-v1.jsonl`,
30 receipts):

| episode | task | attempts | verdict |
|---|---|---:|---|
| 1 | `chapter-gate` (ε0) — fleet-gate queries an unpublished HEAD | 2 | correct cause, correct remedy, correctly said *no source fix is indicated* |
| 2 | `squash-rowversion-ladder` — no commit, HEAD at base | 2 | correct **state**, remedy premature (the real blocker was out-of-domain) |
| 3 | `squash-rowversion-ladder` — `daemon/tests.rs` loads the deleted fixture | 2 | correct; named the exact test, the exact fixture, and *"First expand `conflictDomains`"* |
| 4 | `chapter-gate` (ε1) — clippy `large_enum_variant` | 2 | correct; named both constructor sites |
| 5 | `producers-config-variant-box` — `producer_query.rs` out of bounds | 2 | correct; *"Add that exact path to the task's `conflictDomains`"* |
| 6 | `chapter-gate` (ε1) — 12/24 final-bar cases stale | 2 | correct; enumerated all four repairs |
| 7 | `port-fold-half` — `Cargo.lock` regenerated outside the grant | 2 | correct, **and warned against the wrong fix**: *"Do not merely restore `Cargo.lock`; the gate will regenerate the change"* |
| 8 | `delete-python-driver` — full consumer set outside the boundary | 2 | correct; enumerated 5 files across 3 unowned trees |

**14 of 16 named cause and correct remedy; 2 (episode 2) named the state
correctly and the remedy one escalation early.** No diagnosis this run named a
wrong file or a wrong fix. The mid-run rolling counts recorded in `AUG13-RUN.md`
(15-for-15 at ε0 close, 18-for-18 at the ε1 night) roll chapters 0–2 in; the
epsilon ledger alone holds these 16.

The recurring high-value behavior is **naming the fix to avoid**, now three
chapters running. Episode 7 is the cleanest instance: an operator restoring
`Cargo.lock` by hand would have burned another attempt.

One receipt (episode 3, attempt 1) was **grammar-gagged and still fully legible** —
rejected for a 240-char leading sentence, redacted, and the excerpt preserved the
whole prescription including the fixture path and the domain-expansion
instruction. F15's fix is doing its job on the diagnosis path even when the
length rule fires.

## F37 — the stale-pass race after a re-arm burns exactly one attempt and self-heals

Every re-arm has an in-flight pass holding the pre-amendment snapshot. That pass
validates the newly-authorized commit against the **old** boundary and refuses
it. Observed three times; recorded in the pardons verbatim:

> "the prior rejection came from an in-flight pass holding the pre-grant snapshot,
> so the refused commit is correct under the current boundary" (receipt 15)
>
> "the previous pass snapshotted state before the pardon landed" (receipt 19)
>
> "both burned attempts predate the lockfile grant (one real ownership refusal,
> one stale-pass race)" (receipt 27)

Cost: one attempt per re-arm. Self-healing — the next pass reads the new graph.
Not worth fixing, but **it must be pre-briefed**, because the failure text after
a grant looks exactly like the grant not working. The operator tell is comparing
the rejection's timestamp against the amendment commit — the same discipline as
last run's stale-diagnosis note.

## F38 — D73's single identity collides on durable summary refs, and the archive step is incomplete

Found at the ε1 open, and it recurred live at the ε2 tail.

One campaign identity across three stages means one durable summary namespace:
`refs/tally/spec-build/v1/34af00568cc43499aa8bcc35/summary/{complete,quiescent}`
(the summary digest is stable across stages even though each stage's merge refs
carry their own graph digest). ε0's `complete` and `quiescent` refs, still on
origin, made the ε1 driver refuse to reconcile — *"local campaign summary
disagrees with this outcome"* — killing sweep nodes with projection timeouts.
Remedy: archive to `summary/archive/eps0-*`, delete the canonical names, resume
(receipt 20). A standing operator step was written into Part 7: **at each stage
close, archive the summary refs before re-arming the next stage.**

**It was executed incompletely at the ε1 close, and the flaw came back.** `git
ls-remote origin` today returns exactly four refs in that namespace:

```
summary/archive/eps0-complete
summary/archive/eps0-quiescent
summary/archive/eps1-complete      ← only complete was archived
summary/quiescent                  ← still canonical, still colliding
```

At the ε2 tail, the escalate node was failing with
`local campaign summary 'refs/.../summary/quiescent' disagrees with this outcome` —
the identical defect, one ref narrower.

Root cause of the miss, and the ref set says it plainly: **`quiescent` is written
by the disarm/quiescence act itself**, i.e. *after* the operator archives.
Archiving before the terminal operator act cannot work.

Asks:
1. Restate the standing step: **archive after disarm, and archive both names.**
   Better, make it one verb — `tally campaign archive-summary <stage-tag>` — so
   it cannot be executed half-way.
2. Reconsider D73. The single identity buys a stable receipt ledger and costs a
   manual ref dance that has now gone wrong at **both** stage boundaries it has
   crossed. The ledger's own reconciler already emits six dropped-diagnosis
   warnings (four for `squash-rowversion-ladder`, two for
   `producers-config-variant-box`) of the form *"the worklist no longer names
   that task"* (receipt 26) — cross-stage receipts accumulating under one name is
   a second-order cost of the same decision.

## F39 — the estate-bytes coverage gap: a green chapter, a crash-looping fleet

**A new test class, and the most severe defect of the run.**

Deploy-2 put the ε1 head (`b4e655c8`, generation 126) on the coordinator and the
daemon crash-looped: `unknown field ghOrigin`, refusing to decode the durable task
database.

The reasoning that produced it was correct at every step and wrong in aggregate.
`delete-gh-origin-durable`'s census — recorded in Part 7 §7.4 and in the task
goal, per the F22 doctrine — measured **0 gh-sourced events of 3,859 and 0 of
1,775 rows carrying `ghOrigin`**, which justified deleting the payload-acceptance
path while keeping the `EnqueueSource::Gh` string-decode arm (D33). But **the
census counted `source:"gh"` EVENTS; the pre-deletion writer stamped
explicit-null `ghOrigin` / `ghTriggerActor` / `ghSelfActor` FIELDS on every row it
ever wrote** — 4,272 event files in the operator's state directory carried them at
rollback time, on `source:"calendar"` rows among others. `deny_unknown_fields`
turned history into a boot failure.

**Nothing in the campaign could have seen it.** In-lane gates and the chapter gate
both test freshly-constructed bytes. Neither reads the operator's durable estate.
Call it what it is: an **estate-bytes coverage gap** — a distinct class from F21's
flake-only assertions and from F22's ownership class.

Recovery, in order:

1. **Rollback.** `nixos-rebuild --rollback` is **broken on this flake host**
   (it takes the legacy `NIX_PATH` route). The working route is
   `nix-env --profile /nix/var/nix/profiles/system --switch-generation 125` +
   `switch-to-configuration switch`. Daemon active, estate healthy, quiescent —
   in minutes. **Write this down; it is the only rollback that works here.**
2. **Forward fix by a Codex worker** in an isolated worktree
   (`/home/tom/mecattaf/tally-codex-runs/2026-08-14-ghorigin-decode-tolerance/`):
   a named `DiscardedLegacyGhField` D33 legacy sink beside the retained
   `EnqueueSource::Gh` arm, consuming values through `serde::de::IgnoredAny` so
   nothing is retained or re-emitted; `RowSeed` and `EnqueuePayload` deserialize
   through strict private helpers admitting **only** the three historical members;
   fixtures built from two real captured rows (`legacy-gh-origin.enqueue.json`,
   `legacy-gh-fields.payload.json`); regression tests proving the fields decode,
   vanish from reserialization, and that a `genuinelyUnknown` key still fails
   loudly. Strictness otherwise unchanged — 5 files, +263/−5.
3. `40957154` (PR #605), fleet-gate PASS, generation 127 switched at 01:35Z.
   Daemon active against the full historical estate, quiescent, pools GO.

Asks:
1. **ε2's R4 rebuild verb should replay a REAL estate sample**, not a synthetic
   one. This is already the plan note; it is now the highest-value line in it.
2. Make the census rule explicit in worklist authoring: *count the fields the
   writer emitted, not the values the reader cares about.* A zero-row census is
   evidence about semantics, never about bytes.
3. Any task deleting a serde field on a `deny_unknown_fields` struct must carry
   a named legacy accept-and-discard arm as a delivered behavior, not as a
   follow-up.

## F40 — H2's ownership preflight cannot see the corrections that actually happen (0 of 4)

`ownership-preflight-warn` (H2, `8066a1d1`) does a textual pass over goal and
acceptance-criteria path tokens against declared domains at arm time, and warns.
Live since deploy-2. Against this run's four grants, checked against the
pre-grant task text:

| grant | the missing path | named in the task text? |
|---|---|---|
| `daemon/tests.rs` → rowversion (`663de5bc`) | a stale restart-stability test loading a deleted fixture | no — semantic |
| `producer_query.rs` → variant-box (`1324eaa4`) | a second `Box::new` constructor site | no — semantic |
| `Cargo.toml`/`Cargo.lock` → 5 port lanes (`ef0443f8`) | the lockfile a dependency addition regenerates **by construction** | no — mechanical |
| `crates/tally`, `crates/tally-flow`, `nix/lib` → delete-python (`05aec25d`) | packaging and test references to the deleted driver | no — semantic |

**Zero of four were textually findable.** This is exactly what last run predicted
("a preflight lint catches only the textual third… it should warn, not gate"),
now measured rather than argued. H2 is not wrong and should stay — it costs
nothing and catches the F22 class — but it is not the answer to the ownership
contract, and the run should stop treating it as one.

The `Cargo.lock` row is the interesting one: a whole *class* of correction that
no lint of any kind can find, because the file is not referenced by the task at
all — it is regenerated by the toolchain. That grant fixed the class in one
amendment by granting the lockfiles to **all five remaining lanes that can touch
dependencies** (`ef0443f8`, whose own message says so). Generalize the move: when
a correction is mechanical, grant it to the whole cohort at once rather than lane
by lane.

## F41 — H3's liveness arm works: zero wake-resumes in ε2

F23's shape — a campaign at rest with dispatchable work and zero job units,
refusing to wake because the forge-observation digest is stable precisely
*because* nothing is running — bit once more in ε1, on the deploy-1 pin that
predates the fix. The pardon says so in as many words:

> "Woke the resting frontier to dispatch the amended squash-rowversion-ladder
> attempt; the pass ended without dispatching pending work (the F23 shape whose
> fix this campaign carries but the deployed pin predates)." (receipt 10)

`poll-liveness-arm` (H3, `fe8e3661`) went live at deploy-2. **In ε2's 17 merged
lanes, two escalation episodes and two pardons (receipts 27 and 30) there is not
one wake-pardon.** Every pardon in the ε2 range is a real ownership or race
event. F23 is closed.

## F42 — ownership-correction economics: the census-authoring bet paid

The decisive design argument for the epsilon structure was: *author ε2 only after
ε1 has merged, against the observed tree, converting predicted consumer sets into
observed ones* — projected to take ~11 expected ownership corrections down to ~4.

| | chapters 0–2 | chapter 3-epsilon |
|---|---|---|
| implementation tasks | 34 | 36 |
| worklist-authority commits | 14 | 11 |
| **of which ownership corrections** | **9 (26% of tasks)** | **4 (11% of tasks)** |
| ε2 alone (authored against the observed tree) | — | **2 of 18 (11%)** |
| how discovered | machine-enumerated, operator-mediated | 2 machine-enumerated, **2 agent-requested and adopted verbatim** |
| cost each | 2 burned attempts + **re-project** + resume (~30–40 min) | 2 burned attempts + worklist commit + **re-arm** (auto-pardon) |
| operator diagnosis time | read diagnosis, re-derive the set | zero on the agent-requested pair — the grant text *is* the commit message |

Three compounding causes, in order of contribution:

1. **Authoring against an observed tree, not a predicted one.** The ε2 census
   (authoring artifact, kept in the session scratchpad as `EPS2-CENSUS.md`, not
   committed) counted the real driver at 7,492 LOC and 17 actions, the real
   driver suite at 84 tests, and the real flow at 3,022 LOC with 56
   `additionalProperties` sites — all five figures reproduce exactly against
   `b4e655c8` — before a single task was written. The two corrections that still
   happened were the two things a census cannot see: a toolchain-regenerated
   lockfile and cross-tree packaging references.
2. **Re-arm replaces re-project.** Local mode has no projection to redo. The
   correction cycle is edit → validate → push → re-arm, and the auto-pardon
   records the delta itself.
3. **The agent asks.** Half the corrections arrived pre-diagnosed, verbatim, from
   the lane that needed them (F34).

The rule from last run still stands and was followed every time: **take the
machine's list verbatim, do not re-derive a narrower one.** `delete-python-driver`
is the proof — the machine enumerated `crates/tally/src/cli/campaign.rs`,
`crates/tally/tests/flow_live.rs`,
`crates/tally-flow/tests/spec_build_failed_agent_gate.rs`,
`nix/lib/campaign-drivers.nix` and `nix/lib/spec-build-driver.nix` across three
unowned trees (receipt 29). All five were granted as written.

## F43 — Codex as the campaign agent (D76): fast on small lanes, dies on flooding ones

First full run with Codex as both campaign adapter and out-of-band repair worker.

**Throughput.** ε2 merged 17 lanes in 5h54m at `maxParallel 3` — ~21 min/lane
wall clock — including `port-effect-half` (7,853 insertions / 3,172 deletions),
`port-fold-half` (3,757 insertions) and `port-worktrees` (3,338 insertions).
ε1 merged 14 lanes in 5h36m including a 14,741-line deletion inside 75 minutes.
Small, well-bounded lanes land five minutes apart
(`commit-validator-lint-history` 03:30Z → `node-role-typing` 03:35Z →
`port-worktrees` 03:40Z is a representative chain).

**Death mode.** The one repeated failure is the heavy lane. From the pardon on
`squash-rowversion-ladder`:

> "two prior attempts died between completing the patch and committing it, with
> **the flooding taskdb suite the likely session killer**" (receipt 7)

The agent completed correct work and lost the session between finishing and
committing, twice, on a lane whose acceptance criterion floods the transcript.
The steer that fixed it was *commit before verification* — invert the order so a
session death costs the verification, not the work.

**No adapter-level defect** was observed across the run's ~50 attempts.

Asks:
1. Write the ordering into the authoring guidance: **commit, then verify,
   then amend.** Any acceptance criterion that emits thousands of lines is a
   session-death risk, and the fix is free.
2. Prefer `2>&1 | tail -N` in acceptance argv for suite runs. The machine's own
   diagnoses already prescribe this shape (receipt 9); make it the default.

---

## Grants glossary — what "granted" means, and who held what

The word "granted" appears in four commit messages and several receipts this run.
It is worth one unambiguous paragraph, because it looks like a security term and
is not one.

**A conflict-domain grant is an expansion of a task's declared write ownership,
made by committing a change to the worklist file.** Nothing else. A worklist task
declares `conflictDomains` — the set of paths that task is permitted to modify.
The campaign's ownership gate refuses any commit touching a path outside that
set. When a task genuinely cannot be completed inside its declared set — because
a test in another crate asserts the behavior being deleted, because a dependency
addition regenerates a lockfile, because packaging references live in a different
tree — the boundary must widen, and the only way to widen it is to **edit
`silent-factory-worklists/epsilon.json`, commit it, push it, and re-arm the
campaign on the same identity.** The re-arm records the amendment delta as a
durable auto-pardon receipt naming what changed and why.

**Who holds what authority, at each step:**

- **The machine can only diagnose.** On a failed attempt it prints the exact
  paths a task would need. It has no verb that can act on its own conclusion.
  This remains the largest single unattended-operation gap in the system.
- **The agent can only request.** Since H1, a lane that hits its boundary leaves
  its in-domain work uncommitted, names the missing paths, and stops. It cannot
  widen its own boundary; the ownership gate is what stops it, and the gate reads
  committed bytes it cannot write.
- **The operator alone grants**, by making the commit. Every one of this run's
  four grants is a commit in the tree with a message stating what was granted and
  why — `663de5bc`, `1324eaa4`, `ef0443f8`, `05aec25d`. Each one changes
  **exactly one file**, `silent-factory-worklists/epsilon.json`, and the diffs
  are nothing but added strings in a `conflictDomains` array plus a sentence of
  goal text saying so. That is the complete and auditable record of every
  authority change in the run.

**What a grant is *not*:** it is not a model change, not a permission escalation,
not a sandbox or capability change, not a credential, not a widening of what any
agent may run, read, or reach. The agent's tooling, model, network access and
process privileges come from the host adapter catalog (D77), which no worklist
commit touches; the dispatched unit's argv is identical before and after a grant.
The *only* thing that changes is which file paths one named task in one committed
document is allowed to modify inside its own throwaway worktree — and the change
is a diff you can read. The nearest correct analogy is editing a `CODEOWNERS`
entry, not issuing a token.

## Operational notes that are not defects but cost real time

- **Merges defer while a sibling agent holds the base.** No mid-attempt rebase;
  lanes park "pending" after their gates until the last agent exits. A free slot
  beside pending tasks is normal, not a stall (as with `conflictDomains` overlap
  last run).
- **`maxParallel 3` is honest for ε2 and was dishonest for ε1.** ε1's deletion
  wave is near-serial by domain overlap regardless of the setting. Estimate
  deletion chapters as chains, build chapters as fans.
- **The escalation shape is good.** "Frontier quiescent" with the directly-blocked
  task list, the descendant count, the accumulated diagnoses, the machinery faults
  that bought retries, and the reconciler warnings — readable and honest at 3am
  (receipts 3 and 26).
- **Captures under `~/.local/state/tally/capture/archive/`** are first-line
  forensics and every gate diagnosis links its own. Eight epsilon-era chapter-gate
  captures were archived this run (22 in the archive overall, going back to ch1).
- **`tally campaign quiescent` exits 0 and prints the registration** including
  `armSerial`, `flow`, `driver`, `checkout` and `lastObservation` — the single
  most useful one-line health check in local mode, and the way to confirm which
  store paths a campaign is actually running.
- **Deploy branches stay local.** Deploy-1's `b2c61c0f` and deploy-2's `60afa885`
  both ride dotfiles PR #225 whenever it merges; dotfiles `main` still pins
  `78dd4871` in its `flake.lock`. This is fine but it means the running fleet is
  ahead of the declared fleet, and has been for two days.
- **The deploy-skip ceremony is over.** The dated drop-ins under
  `tally-producer-nightly-fleet-deploy.service.d/` are gone, and the unit's
  `ExecCondition` is now D63's `tally campaign quiescent`. The nightly deploy
  guards itself; nothing needs re-stamping.

## Decisions waiting for you

1. **The narrator (F32).** Zero for 35 is not a polish item any more. Fix the
   JSON envelope in the shim before anything else; it is over half the failures.
2. **Add clippy to the per-lane gate set (F33).** One worklist commit. It would
   have erased one of this run's three gate cycles outright; putting the final
   bar in the lane set would have taken a second.
3. **Make the archive step one verb, after disarm (F38).** The manual version has
   now gone wrong at both stage boundaries it has crossed, and the second failure
   is live at the ε2 tail.
4. **R4 must replay a real estate sample (F39).** The deploy-2 regression is the
   only fleet-down event of the ladder and the only defect class with no coverage
   at all.
5. **Make "needs-grant" a first-class agent outcome (F35).** The agents already
   produce the content; only the channel is missing. This, plus (3), is what
   would make the next campaign genuinely unattended.
6. **Decide whether D73's single identity survives ε2** (F38) — it has now cost
   two ref collisions and a reconciler warning stream, against the benefit of one
   stable receipt ledger.
7. Still outstanding: `delete-python-driver` is running under the granted graph
   and the ε2 chapter gate is queued behind it; the ε2 close still owes a real
   `tally-probe-*` run and the self-release (§7.2 item 6); and dotfiles PR #225
   is the last unwound thread from the deploy era.
