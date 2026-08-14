# August 7–14 process archaeology — the ceremony census, the tail the printed
# report missed, and what the week's habits cost

Written 2026-08-14. Findings are prefixed **PA-** and numbered continuously;
they do not reuse F1–F44, which are taken by `AUG13-RUN.md`,
`AUGUST-12-LEARNINGS.md` and `AUG14-LEARNINGS.md`.

**What this document is.** Not a record of what tally built. A record of *how
the work was done* — every act a human or a frontier model had to type, what
the system already knew at the moment it was typed, and whether tally could
have typed it itself. The question behind it is the operator's: which
restrictions earn their keep, and which are ceremony that reversed the rule
they were meant to serve.

**The one-sentence answer.** Epsilon shipped 37 merged lanes, 3 chapter gates
and a self-published GitHub release in ~19 wall-clock hours, and it did so
while a frontier model sat at the keyboard performing **roughly 60 discrete
orchestration acts, of which 9 have a tally verb and ~51 do not** — and the
single most expensive stretch of the whole run was the **86 minutes after the
last lane merged**, an unbroken chain of hand-typed ceremony that the printed
report does not contain a single line of.

---

## 0. Sources and method

Read at source, nothing taken from a summary where the primary was on disk:

| Source | What it settles |
|---|---|
| `/home/tom/mecattaf/tally.nix/AUG13-RUN.md` (1033 lines, mtime 08-14 12:49) | the only record of the post-17/18 tail |
| `/home/tom/mecattaf/tally.nix/AUG14-LEARNINGS.md` (mtime 08-14 10:54) | F27–F43; **froze before the tail** |
| `…/scratchpad/epsilon-print.md` (830 lines, mtime 08-14 10:57) | the printed report; **froze before the tail** |
| `~/.local/state/tally/campaigns/attempt-receipts/epsilon/attempt-receipts-v1.jsonl` | 34 receipts, every operator pardon verbatim |
| `~/.local/state/tally/campaigns/steering/*/steering-v1.jsonl` | 4 registrations, **2 steers total** |
| `~/.local/state/tally/campaigns/releases/019fffba-…/` | release record, notes, artifacts, probe receipt |
| `/home/tom/mecattaf/tally-codex-runs/2026-08-1{3,4}-*` | 3 out-of-band repairs: briefs, prompts, reports, fleet-gate transcripts |
| `git log`, `git for-each-ref`, `.git/logs/refs/heads/tally/**` | the acts that left no receipt |
| `crates/tally/src/cli/{args.rs,campaign.rs}`, `crates/tally-core/src/campaign_contract.rs` | what the verbs actually are, and what `release` actually requires |
| `AUG12-*.md`, `AUGUST-1{0,1,2}-*.md`, `aug10-midday-session.md`, `AUG11-evening-pass.md` | the week before epsilon |

Two structural facts about the sources themselves are findings in their own
right and appear below as **PA-01** and **PA-02**.

---

## 1. PA-01 — the week's forensic record is not in git

```
JULY31-LEARNINGS.md              TRACKED
AUGUST-07-LEARNINGS.md           TRACKED
AUGUST-10-LEARNINGS.md           TRACKED
AUG11-evening-pass.md            TRACKED
SILENT-FACTORY-PLAN.md           TRACKED
aug10-midday-session.md          UNTRACKED
AUGUST-11-OVERNIGHT.md           UNTRACKED
AUG12-HANDOFF.md                 UNTRACKED
AUG12-DAYRUN-HANDOFF.md          UNTRACKED
AUG12-overnight.md               UNTRACKED
AUGUST-12-LEARNINGS.md           UNTRACKED
AUG13-RUN.md                     UNTRACKED
AUG14-LEARNINGS.md               UNTRACKED
```

Seven files — **every record from Aug 10 midday through the epsilon close,
the highest-throughput stretch in the project's history** — exist only in the
working tree. `AUG13-RUN.md` is the sole record of the F44 discovery, the
premature disarm, the ref restoration, the probe 403 and the release. One
`git clean -fdx` and the entire causal history of the epsilon ladder is gone.

The cause is traceable: `AUG11-evening-pass.md` records issue **#523**
("ruling request: lineage docs under merge control") and reports it "cleared
by the operator" — but the *direction* of the clearing was never written into
a rule, and the practice diverged from it four days ago and never came back.
`AUGUST-11-OVERNIGHT.md` closes with "Working material still held uncommitted
by your ruling: `skills/*.md` edits, `AUGUST-10-LEARNINGS.md`,
`aug10-midday-session.md`, and this report."

**PA-01 verdict: ceremony that reversed its own rule.** The prohibition
existed to stop narration commits. It is now stopping the only forensic record
of the machine's behaviour from being durable, in a project whose entire thesis
is durable local state.

## 2. PA-02 — the printed report and the learnings file both froze mid-run

`AUG14-LEARNINGS.md` (10:54) and `epsilon-print.md` (10:57) were both written
while ε2 stood at **17 of 18 lanes** with `delete-python-driver` in flight.
Their closing line is:

> 7. Still outstanding: `delete-python-driver` is running under the granted
>    graph and the ε2 chapter gate is queued behind it; the ε2 close still owes
>    a real `tally-probe-*` run and the self-release (§7.2 item 6)…

Everything in §4 of this document happened after that sentence was written.
The reporting cadence is tied to the frontier session's context budget, not to
the run's own terminal state — so the most defect-dense hour of the run
(§4) is the hour with the thinnest record.

---

## 3. PART A — THE CEREMONY CENSUS

### 3.0 The verb surface, from source

`crates/tally/src/cli/args.rs:181-200` — the complete campaign vocabulary:

| verb | doc comment (verbatim) |
|---|---|
| `Arm` | "Register a repository/worklist campaign and admit its current reconcile pass." |
| `Steer` | "Append human steering to an armed campaign's local ordered log." |
| `Resume` | "Pardon an escalated campaign's counters, record why, and re-arm it." |
| `Release` | "Render or execute the release represented by a completed campaign." |
| `Poll` | "Reconcile changed armed campaigns into fresh bounded flow passes." |
| `Status` | "Show the current or latest durable pass for one campaign…" |
| `List` | "Print local campaign identities, admitted digests, and authority bindings." |
| `Quiescent` | "Exit successfully only when the local campaign registry is empty." |
| `Disarm` | "Remove a local campaign registration." |

Nine verbs. `grep -n "publish\|advance" crates/tally/src/cli/args.rs` returns
exactly one hit — a doc-comment about a git remote. **There is no publish verb,
no archive verb, no grant verb, no deploy verb, no probe-teardown verb, and no
verb that separates "pardon" from "re-arm".**

### 3.1 The census table

Counted for the epsilon run only (armed 2026-08-13 15:23Z, release executed
2026-08-14 10:49Z). "Knew the text?" = did tally already hold the exact correct
action in a receipt, diagnosis, error string or ref name at the moment the
human typed it.

| # | act | count | evidence | knew the text? | could tally have executed it? |
|---|---|---|---|---|---|
| 1 | `arm` (fresh registration) | **4** | 4 registration ids: `019ffbb8`, `019ffc34`, `019ffdeb`, `019fffba` (steering dirs + ref namespaces) | n/a — deliberate | n/a |
| 2 | re-arm on same identity (armSerial++) | **~9** | 12 authority commits − 3 stage authorings; `AUG14-LEARNINGS` records ε1 armSerial 5, ε2 armSerial 5 at 08:36:50Z | **yes** — every one followed a diagnosis naming the change | **yes**, trivially: the amendment is a committed blob; a poll could re-approve it |
| 3 | `steer` | **2** | `steering-v1.jsonl` totals: `019ffbb8`=1, `019ffc34`=1, `019ffdeb`=0, `019fffba`=0 | partly | no — these were genuine judgement |
| 4 | `resume`/pardon | **10** | 10 receipts of `kind:"pardon"`, all `actor:"uid:1000"` | **yes, 10/10** — see 3.3 | **8 of 10** mechanically |
| 5 | worklist authority commit + push | **12** | git log `docs(worklists): epsilon*` ×12 | 8 of 12 verbatim from a diagnosis | **8 of 12** |
| 6 | └ of which ownership grants | **4** | `663de5bc`, `1324eaa4`, `ef0443f8`, `05aec25d` | **yes** — machine printed the exact paths | **yes** |
| 7 | └ of which new amendment tasks | **4** | `19bd53af`, `482ff524`, `c848d491`, `aa9f6213` | **yes** — each minted from a gate diagnosis | mostly |
| 8 | └ of which stage authorings | **3** | `1953bb49`, `3f3f8525`, `4309acc1` | no — human design | no |
| 9 | └ policy section (D77) | **1** | `6a7c841a` | no | no |
| 10 | `disarm` | **4** | 3 stage boundaries + 1 premature pre-release | mixed | at stage boundaries, yes |
| 11 | rebase-publish (advance `main`) | **3** | `6fdf108f`, `b4e655c8`, `a8077295`; hand tooling `shas.txt` (40 lines) + `bodies.txt` + `@@@END@@@` delimiter in the scratchpad | **yes** — the integration branch *is* the answer | **yes**; no verb exists |
| 12 | summary-ref archive | **3 stage closes × 2 refs = 6 writes + 6 deletes** | `summary/archive/eps{0,1,2}-{complete,quiescent}` | **yes** — the collision is deterministic | **yes**; no verb exists |
| 13 | integration-ref hand-restore | **1** | `.git/logs/refs/heads/tally/epsilon-campaign-019fffba-…/integration`: `0000…000 8b45283 mecattaf … branch: Created from 8b45283…` at epoch 1786701876 = **12:04:36 CEST** | **yes** — the name is `stable_publish_branch()` output | **yes** |
| 14 | checkpoint-ref restore | **≥1** | codex report 12:28: plan failed with `completed campaign has no local checkpoint refs below refs/tally/…/checkpoint/`; the release at 12:49 consumed one. **This act appears in no record at all.** | yes | yes |
| 15 | deploy / generation switch | **4** | gen 125 → 126 (crash-loop) → 125 (rollback) → 127 | partly | D63's `quiescent` ExecCondition already exists |
| 16 | out-of-band repair orchestration | **3** | `2026-08-13-arm-self-contained`, `2026-08-14-ghorigin-decode-tolerance`, `2026-08-14-release-trailer-bridge` — each = brief + prompt + worktree + run + report | no — real engineering | no |
| 17 | hand-run `test/fleet-gate.sh` | **4** | 2 logs for D77 (14:26:46Z FAIL, 14:44:52Z PASS), 1 ghorigin (01:07:18Z→01:24:00Z), 1 bridge (10:30:04Z→10:48:00Z) | n/a | **yes** — it is a checkpoint argv |
| 18 | open a PR *solely* to satisfy a gate stage | **3** | #604, #605, #606; `FAIL CHANGELOG.md exists but no open pull request contains this head SHA` | **yes** | yes |
| 19 | `campaign release --plan` | **≥3** | pre-F44, post-bridge (codex), pre-execute | n/a | n/a |
| 20 | `test/release-probe.sh` | **1** | `probe-receipt-v1.json`, 10:48:54Z→10:49:04Z | n/a | it is the verb |
| 21 | probe teardown | **0 of 1 — failed** | `teardownComplete: false`, HTTP 403 | **yes** — printed the exact `gh auth refresh` command | no (scope), but **could have refused at preflight**) |
| 22 | `campaign release` (execute) | **1** | 10:49:29Z | n/a | n/a |
| 23 | monitor plumbing | continuous | scratchpad `jobs.json` (0 bytes, 10:34), stall-predicate supervisor rewritten twice in `AUG13-RUN.md` | **yes** — `quiescent` + `status` exist | **yes** |
| 24 | deploy-skip drop-in stamping | inherited, retired mid-run | `skip-2026-08-13.conf`, `skip-ladder-through-2026-08-17.conf` | yes | D63 shipped the fix; it took a chapter to land |

**Total discrete orchestration acts: ~60.** Nine have a verb. Roughly 51 do
not, and of the ~40 that were *reactive* (rows 2, 4, 5–7, 10–14, 17–18, 21,
23), **the system had already printed or computed the exact correct action in
every single case.**

### 3.2 PA-03 — the receipts ledger records 10 of ~60 operator acts

`attempt-receipts-v1.jsonl` carries 34 records: 18 diagnoses, 10 pardons, 3
escalations, 3 machinery retries. `Counter({None: 24, 'uid:1000': 10})` on the
`actor` field — **the only human act tally durably records is a pardon.**

Arms, re-arms, steers, disarms, publishes, ref archives, ref restorations,
deploys, gate runs and the release itself leave nothing in the campaign's own
append-only ledger. The steering log is the only other operator-visible ledger,
and see PA-04. The one place a future auditor would look for "what did the
human do to this campaign" answers the question at 17% coverage.

### 3.3 PA-04 — steering history does not survive a stage; and only 2 steers happened

Four registrations, four separate `steering-v1.jsonl` files. The two steers
issued in the whole run:

- `019ffbb8` seq 1, `final-bar-local-forge-repair`, 2026-08-13T15:32:23Z —
  "Chose the narrow repair as binding guidance: delete the dead
  `--allow-test-local-forge` arguments rather than reintroducing any
  compatibility flag in crates."
- `019ffc34` seq 1, `squash-rowversion-ladder`, 2026-08-13T20:01:59Z —
  "Commit FIRST, verify second… Run the taskdb suite with its output tamed —
  for example `cargo test -p tally-core taskdb 2>&1 | tail -30` — because the
  raw suite floods hundreds of kilobytes of expected daemon-event JSON and that
  flood is the likely killer of the two prior sessions."

Both are excellent, both are irreplaceable human judgement, and both are the
*cheapest* acts in the census. The steering log for `019ffdeb` (the 19-lane ε2
wave) and `019fffba` are **empty**: `highWater: 0`.

Two consequences:

1. **The verb that most needs a frontier model was used twice in 19 hours.**
   Everything else the frontier model did was clerical.
2. **A disarm→arm boundary silently discards the steering ledger.** The second
   steer's insight ("the taskdb suite floods and kills the session") is
   registration-scoped and unreachable from ε2, which contains the same suite.

### 3.4 PA-05 — 10 of 10 pardons were mechanically derivable

Verbatim `reason` fields, all `actor: uid:1000`:

| seq | reason (truncated) | derivable? |
|---|---|---|
| 4 | "Re-armed graph added dependency `gate-local-audit` to escalated task `chapter-gate`; the amendment is the operator's structural steering response." | **yes** — this is literally R11's auto-pardon condition |
| 7 | "Pardoned … after steering it to commit before verification" | no — follows a human steer |
| 10 | "Woke the resting frontier to dispatch the amended … lane; the pass ended without dispatching pending work (the F23 shape whose fix this campaign carries but the deployed pin lacks)" | **yes** — F23, fixed by `poll-liveness-arm` *in this very campaign* |
| 15 | "the prior rejection came from an in-flight pass holding the pre-grant snapshot" | **yes** — the stale-pass race, F37 |
| 18 | "all fourteen implementation lanes are merged, fleet-gate is proven green on this tree" | **yes** |
| 19 | "Re-dispatched after the pardon-race: the previous pass snapshotted state before the pardon landed" | **yes** — pure race, self-evident |
| 20 | "Cleared the stage 0 summary refs that collided with stage 1 under the shared campaign identity" | **yes** — D73, deterministic |
| 27 | "both burned attempts predate the lockfile grant (one real ownership refusal, one stale-pass race)" | **yes** |
| 30 | "its burned attempts predate the full consumer-set grant; the amended task owns … per its own boundary refusal" | **yes** |
| 34 | "both burned attempts predate the schema-example lint fix, which is now merged" | **yes** |

**Nine of ten reduce to one predicate: "the attempts that burned this counter
predate the amendment that addresses them."** That predicate is computable —
the amendment's commit timestamp and the receipt sequence numbers are both in
durable local state. `AUG13-RUN.md`'s F17 already names the mechanism
(`campaign.rs:1040-1092`, auto-pardon on gained dependency) and its narrowness:
it fires only for a *gained dependency*, not for a gained conflict domain, a
changed goal, a new sibling task, or a merged out-of-band fix.

**PA-05 verdict: `resume --reason` is the single highest-volume ceremony verb
in the system and 90% of its uses are a one-line predicate away from being
automatic.**

### 3.5 PA-06 — `resume` conflates pardon with re-arm, and the doctrine contradicts itself

`Resume` = "Pardon an escalated campaign's counters, record why, and re-arm
it." One verb, two effects. The plan's own supervision playbook (§7.6) says:

> Recovery verb is `resume --reason`, always. Never disarm-first — disarm
> destroys the auto-pardon baseline (F17).

But D73 requires the *content* of `epsilon.json` to be replaced between stages
"only at registry quiescence" — i.e. **disarm** — and the run did exactly that
three times. So the doctrine's absolute ("never disarm-first") and the
identity model (D73) are in direct conflict, and the identity model won every
time. The cost of each disarm is a fresh registration id, which is baked into
`stable_publish_branch()` (`crates/tally-core/src/campaign_folds.rs:188`:
`format!("tally/{campaign}-campaign-{campaign_id}/{task_id}{suffix}")`) — see
PA-22.

### 3.6 PA-07 — the ownership grant is a five-step manual transaction with no verb

From `AUG14-LEARNINGS.md`'s own glossary, verbatim:

> the boundary must widen, and the only way to widen it is to **edit
> `silent-factory-worklists/epsilon.json`, commit it, push it, and re-arm the
> campaign on the same identity.**

and

> - **The machine can only diagnose.** On a failed attempt it prints the exact
>   paths a task would need. It has no verb that can act on its own conclusion.
>   This remains the largest single unattended-operation gap in the system.
> - **The agent can only request.** … It cannot widen its own boundary.
> - **The operator alone grants**, by making the commit.

Four grants in epsilon (down from nine in ch0–ch2 — the census-authoring bet
of F42 genuinely paid). But each still costs: read diagnosis → edit JSON →
validate → `git commit` → `git push` (racing live lanes) → re-arm → pardon.
Seven steps, one of which (`git push` mid-run) is a known hazard recorded as
early as F22 in `AUG13-RUN.md`:

> the timing hazard observed here: `git push` of the authority fix raced two
> lanes merging.

### 3.7 PA-08 — publishing is a hand-built harness with a bespoke file format

There is no publish verb. The evidence of what filled the gap is in the
scratchpad: `shas.txt` (40 commit shas), `bodies.txt` (16 KB of commit bodies
separated by a literal `@@@END@@@` sentinel), and two throwaway git worktrees
`eps1-publish/` and `eps2-publish/`. The purpose was to carry `Tally-Task:` /
`Tally-Revision:` trailers across a rebase.

The irony is exact: **the trailers this ad-hoc harness existed to preserve are
the same trailers whose canonical bytes then failed the release verb's oracle
(F44).** The operator hand-built infrastructure to protect a proof the tool
then refused to accept.

### 3.8 PA-09 — three of four hand-run fleet gates were spent on the gate's own contract

`test/fleet-gate.sh` stage order, from the transcript headers, is:
`cargo fmt → cargo test → cargo clippy → cargo deny → nix flake check →
VM check inventory → no Rust placeholders → no GitHub workflows → changelog
policy`.

The D77 repair's first run:

```
started-at: 2026-08-13T14:26:46Z
…
==> changelog policy
FAIL CHANGELOG.md exists but no open pull request contains this head SHA
fleet gate: FAIL 7c2b4954471d5772bcbaf77e6f185899cf8b8e57
```

~17 minutes of compute, then a failure on a **pure metadata predicate** that
could have been evaluated in 200 ms at stage 0. The second run, with PR #604
open and every derivation cached, took 5m49s (14:44:52Z→14:50:41Z).

This directly contradicts the doctrine `AUGUST-12-LEARNINGS.md` records as a
win: *"Gate ordering (cheap-fails-first) paid off — most failures surfaced in
`fmt`/`tests` long before the expensive stages."* The per-lane gates obey it;
the fleet gate inverts it.

**PA-09 verdict: ceremony that reversed its own rule.** Cost this week: one
full gate cycle on D77, and it is the same shape as F28 (the gate is
forge-native) which cost ε0 its first chapter-gate cycle.

### 3.9 PA-10 — the out-of-band repair loop is 6 steps and ~45 minutes, every time

Measured from the three codex run directories:

| repair | brief written | worker done | gate | outcome |
|---|---|---|---|---|
| D77 self-contained arm | 08-13 15:38 | 16:24 (46 min) | 2 runs, 23 min total | PR #604, 3 gate runs to land 1 commit |
| D33 ghorigin tolerance | 08-14 02:52 | 03:06 (14 min) | 01:07:18Z→01:24:00Z (17 min) | PR #605 |
| F44 trailer bridge | 08-14 12:07 | 12:28 (21 min) | 10:30:04Z→10:48:00Z (18 min) | PR #606 |

Every one required, by hand: author a brief, create an isolated worktree,
launch the worker, read the report, push a branch, **open a PR only because the
gate demands one**, run the gate, merge fast-forward, rebuild, and (twice)
re-deploy. The workers themselves were excellent — all three delivered clean,
tested, reported work and none pushed anything.

Two friction items visible in `run.stderr`:
- D77: `Rejected("rm -f style commands are not permitted")` — the codex sandbox
  refused `rm -rf drivers/__pycache__`, a build artifact.
- D33: one `apply_patch verification failed` retry.

### 3.10 PA-11 — the monitoring loop was rebuilt by hand at least twice

`AUG13-RUN.md`, on ch0:

> The supervisor logged `jobs=0` from iter 65 onward but did not treat it as
> trouble: my exit conditions were *master closed* or *registration vanished*,
> and neither held. **Finding F13a — the supervisor's liveness predicate was
> wrong.**

then:

> Supervisor rewritten with a stall predicate: exit 2 on `reg=1 && jobs=0`
> sustained four ticks (720 s). It fired correctly at 16:11:25 CEST on the very
> next stall — the predicate is proven, not just intended.

The predicate was then written into the *plan* (§7.6) rather than into the
tool. Epsilon's monitoring residue is a 0-byte `jobs.json` in the scratchpad.
`tally campaign quiescent` and `tally campaign status` both exist and both were
used, but neither blocks-until-condition, so the loop is still shell.

### 3.11 PA-12 — the census's honest summary

Sorting the ~60 acts by what they actually required:

| class | acts | share |
|---|---|---|
| genuine human judgement (2 steers, 3 stage authorings, D77 ruling, 3 repair briefs, deploy decisions) | ~11 | 18% |
| **mechanically derivable from state tally already held** | **~40** | **67%** |
| forced by an external constraint (gh scopes, PR-for-changelog) | ~6 | 10% |
| pure bookkeeping (ref archive, ref restore, publish) | ~6 | 10% |

The frontier model was needed for eleven acts across nineteen hours. It was
*present* for all sixty because the eleven are not separable from the forty
without a verb.

---

## 4. PART B — THE TAIL: WHAT THE PRINTED REPORT MISSED

Timeline of the 86 minutes after the last lane merged. Every item below is
absent from `epsilon-print.md` and from `AUG14-LEARNINGS.md`.

| CEST | act |
|---|---|
| 11:23:11 | ε2 gate checkpoint ref pushed; `a8077295` published to `main` (rebase) |
| ~11:2x | **disarm** (premature) |
| ~11:3x | both ε2 summary refs archived (`eps2-*`), F38 ordering applied |
| ~11:4x | first `campaign release --plan` → **F44**, trailer oracle failure |
| 12:04:36 | integration branch hand-created under new registration `019fffba` |
| 12:07 | F44 brief written to `tally-codex-runs/2026-08-14-release-trailer-bridge/` |
| 12:28 | worker done: `e921cccc`; live plan now fails on **missing checkpoint refs** |
| 12:30–12:48 | PR #606, fleet gate `10:30:04Z→10:48:00Z` PASS, merge, rebuild |
| 12:48:54–12:49:04 | real probe: `releaseComplete: true`, **`teardownComplete: false`, HTTP 403** |
| 12:49:29 | release `0.0.0+20260814092311.8b45283` executed |
| ~12:5x | final disarm; summary refs archived (claimed) |

### PA-13 — F44's real shape: the root cause *was* found, and it is not a bug

The `2026-08-14-release-trailer-bridge/brief.md` framed it as a canonicalization
divergence:

> the trailers were written by the now-deleted PYTHON driver's
> `file_task_completion_revision`; the verifier is Rust
> `task_completion_revision`; their canonical bytes disagree somewhere.

The worker's report answers it exactly, and the answer is stronger than the
question. Both implementations compact and recursively sort keys identically.
**They hash different documents.**

```
Python file_task_completion_revision:
  {contractVersion, repository, source:{repository,path}, task}
  python_bytes=1562  python_hash=sha256:b68c64f98eab7219…

Rust task_completion_revision:
  {contractVersion, campaign, repository, mergeMethod, agent, steward,
   gates, task, content}
  rust_bytes=2671    rust_hash=sha256:c1a6a166737bdebf…
```

Verified independently at source — `crates/tally-core/src/campaign_contract.rs:729-751`:

```rust
struct CompletionPolicy<'a> {
    contract_version: u32,
    campaign: &'a str,
    repository: &'a CampaignRepository,
    merge_method: &'a str,
    agent: &'a CampaignAgent,
    steward: &'a Option<CampaignSteward>,
    gates: &'a [CampaignGate],
    task: &'a CampaignTaskReference,
    content: &'a CanonicalCampaignTaskV1,
}
```

**PA-13 verdict: this is not a canonicalization bug and the bridge is not
papering over one.** A contract was deliberately widened during the Rust port
with no migration path for existing proofs. The root cause was found, recorded
with byte counts and reproduced hashes, and correctly left unfixed.

### PA-14 — the generalized F44, which nobody has written down: **amending the worklist invalidates already-merged proofs**

This is the finding with the longest reach and it is nowhere in the record.

The Rust completion identity hashes `gates`, `agent`, `steward`, `merge_method`
and the task's `content` (title/body). D74-as-amended-by-D77 states the design
intent explicitly:

> declared in the worklist's `campaign` section, **so changing gates is a
> worklist commit, never a deploy**

and `AUG14-LEARNINGS.md`'s grants glossary states that every grant is

> nothing but added strings in a `conflictDomains` array **plus a sentence of
> goal text saying so**.

Therefore: **every gate-set change and every ownership grant rotates the
completion revision of every task in the campaign — including tasks that
already merged and already wrote their trailer.** Release computes revisions
from the *final* approved graph (`read_approved_graph_snapshot` at
`campaign.rs:1488`) and compares them to trailers written at *merge* time.

Epsilon amended `epsilon.json` **eight times mid-run**. So even in an
all-Rust future, the exact oracle will only ever prove the tasks that merged
after the last amendment.

The bridge already absorbs this — `release_completion_bridge_ref`
(`campaign.rs:2034-2056`) builds the expected ref leaf from the **claimed**
(trailer) revision, not the expected one, and matches on tree equality — so it
was, without anyone saying so, a fix for two distinct problems.

**Consequence: the exact oracle is effectively dead for any campaign that
amends its own authority, which epsilon's own doctrine encourages.**

### PA-15 — the bridge oracle is a materially weaker proof, and nothing durable says it was used

The exact oracle proves *the merged commit's declared identity equals the
identity the campaign authority computes*. The bridge proves only *some ref
under `refs/heads/tally/<campaign>-campaign-*/` — a ref the driver itself
wrote — points at a commit with the same tree*. It is self-certifying by
construction.

All **19 of 19** epsilon proofs went through it (`AUG13-RUN.md`: "proved all 19
completion proofs via the bridge oracle, correctly labeled").

And the label is not persisted:

```
$ python3 -c "print(sorted(json.load(open('release-artifacts-v1.json')).keys()))"
['artifacts', 'campaign', 'closingSummary', 'gateProof', 'repository',
 'revision', 'schemaVersion', 'version']
```

No `completionProofs`. The `exact`/`bridge` labelling exists only in the
transient `--plan` output. `release-record-v1.json` carries
`"planSha256": "sha256:c78fc039…"` — **a commitment to a document that is
stored nowhere**, so the hash is unverifiable after the fact.

### PA-16 — the disarm-before-release trap, from source

`campaign_release_plan` (`crates/tally/src/cli/campaign.rs:1487-1521`) requires
**five** durable inputs, in order:

1. `read_release_registration` — the armed registration file. Error text:
   `"cannot read campaign registration {}; arm the campaign before rendering
   its release"` (`:1659`).
2. `read_approved_graph_snapshot` — error:
   `"…has no approved graph snapshot; re-arm it before rendering a release"`.
3. `release_required_ref(&refs, &integration_ref, "commit")` where
   `integration_ref = stable_publish_branch(campaign, &registration.registration_id, "integration", None)`
   — **named after the current registration id**.
4. checkpoint refs under `refs/tally/spec-build/v1/<identity>/checkpoint/`.
5. `release_closing_summary` — a `complete` summary whose `source` matches the
   gate checkpoint.

**Three of these five are destroyed by the campaign's own documented close
ceremony**, and the fourth is destroyed by the recovery from the first:

| close-ceremony act | which precondition it breaks |
|---|---|
| `disarm` (terminal operator act, per ε0's own shakedown ledger) | (1) and (2) |
| re-arm to recover (1)/(2) | (3) — new registration id ⇒ new integration branch name |
| archive the summary refs (F38's "STANDING OPERATOR STEP") | (5), and see PA-17 |
| — | (4) was also missing at 12:28 and was restored by an act with no record |

**PA-16 verdict: not defensible as written.** Requiring an armed registration
is defensible *if* "armed" is what the machine understands as "this campaign is
still the live one". Requiring it *after* telling the operator that disarm is
the terminal act, and then naming a required git ref after the registration id
that re-arming necessarily changes, is a trap with three independent jaws.

The hand-recovery is in the reflog, unambiguous:

```
$ cat .git/logs/refs/heads/tally/epsilon-campaign-019fffba-…/integration
0000000000000000000000000000000000000000 8b45283856d714f8ff1687e102b0237de851d86c
  mecattaf <thomas@leger.run> 1786701876 +0200  branch: Created from 8b45283…
```

`1786701876` = **2026-08-14 12:04:36 CEST**. A human typed `git branch` to
satisfy a release verb's precondition.

### PA-17 — the archive ceremony is one act away from bricking release, and it did not durably take

`release_closing_summary` (`campaign.rs:2306-2330`) filters summaries by
`outcome == "complete"` and matching `source_sha256`, prefers the canonical
name, and then:

```rust
_ => bail!(
    "completed campaign has multiple archived complete summaries for source {source_sha256}"
),
```

Live state proves both refs exist for the same source:

```
summary/complete               975ed552…  source=sha256:20ca5b97…
summary/archive/eps2-complete  5bc2760d…  source=sha256:20ca5b97…
```

The release survived only because one of the two sits at the canonical name.
**Had the operator applied F38's standing step one more time — archiving the
newly re-minted `summary/complete` — the release verb would have failed
closed.** F38's remedy and the release verb's precondition are in direct
opposition, and nobody has noticed.

Worse: `AUG13-RUN.md`'s final paragraph claims "Final disarm done, summary
refs archived (eps2-final-*)". No `eps2-final-*` ref exists. The canonical
`summary/complete` is still live. **The archive ceremony has now silently
failed at the one boundary that mattered most**, which is the third failure of
this manual step in three attempts (F38 already recorded two).

### PA-18 — the canonical closing summary contains 19 of 19 dangling references

The `summary/complete` blob the release verb consumed lists every merged task
with a locator. All nineteen point at branches under registration `019fffba`,
which holds exactly **one** ref (the hand-created integration branch):

```
locators: 19
missing:  19
  refs/heads/tally/epsilon-campaign-019fffba-…/port-shared-campaign-folds-3d1a5385e5a43523
  refs/heads/tally/epsilon-campaign-019fffba-…/release-plan-render-bc0e2fda5d05e3d7
  …
```

The real refs live under `019ffdeb`. The re-arm re-rendered the closing summary
with the *current* registration id substituted into every locator. **The
campaign's own durable completion receipt — the artifact D13/D14 exist to
make authoritative — is 100% dangling.**

### PA-19 — the closing summary also carries 12 reconciler warnings, all D73

```
#### Reconciler warnings
- campaign pardon local://campaign/epsilon/attempt-receipts/4 pardoned 3 earlier machine receipt(s) for task(s) 'chapter-gate'
- dropped machine diagnosis for 'squash-rowversion-ladder': the worklist no longer names that task   [×6]
- campaign pardon local://campaign/epsilon/attempt-receipts/{15,18,27,30,34} pardoned N earlier machine receipt(s)
```

Six "the worklist no longer names that task" warnings are pure D73 residue:
one identity, three different task sets, one shared receipt ledger. Every
reconcile drags stage-1 receipts past a stage-2 worklist and complains.

### PA-20 — the probe reports `failed` for a teardown-scope problem after the thing it tested succeeded

`probes/tally-probe-20260814-6bf9bac2/probe-receipt-v1.json`, verbatim:

```json
"status": "failed",
"repositoryCreated": true,
"releaseComplete": true,
"teardownComplete": false,
"expiredRepositoriesDeleted": 0,
"failure": "tearing down the campaign release probe repository through gh exited exit status: 1: HTTP 403: Must have admin rights to Repository. … This API operation needs the \"delete_repo\" scope. To request it, run: gh auth refresh -h github.com -s delete_repo"
```

Four separate problems in ten seconds:

1. **Verdict conflation.** `releaseComplete: true` is the answer to the
   question the probe exists to ask. `status: "failed"` is the answer to a
   different question. The operator had to read past the verdict to proceed.
2. **No scope preflight.** The probe creates a *real* GitHub repository and
   only discovers at teardown that it cannot delete one. `gh auth status`
   answers this in milliseconds, before any repository exists. D75 specified
   "the release act runs on the operator's ambient gh auth from the
   coordinator" and never specified which scopes ambient auth must carry.
3. **The system knew the exact fix and could not apply it.** It printed
   `gh auth refresh -h github.com -s delete_repo` verbatim — the same pattern
   as every diagnosis in PA-05.
4. **`expiredRepositoriesDeleted: 0`** — the probe has a garbage-collection
   arm for orphaned probe repos, and it is disabled by the same missing scope.
   The orphan therefore accumulates and the collector that would clean it up
   cannot run. **Self-defeating by construction.**

`AUG13-RUN.md` leaves it as operator item (1): "delete_repo scope + probe repo
cleanup." As of this writing `mecattaf/tally-probe-20260814-6bf9bac2` is
presumed still live.

### PA-21 — the released revision is not on `main`

```
$ git merge-base --is-ancestor 8b45283856d714f8ff1687e102b0237de851d86c origin/main
8b45283 is NOT an ancestor of origin/main

$ git diff --stat 8b45283 a8077295
 silent-factory-worklists/epsilon.json | 62 +++++++++++++++++++++++++++++++----
 1 file changed, 55 insertions(+), 7 deletions(-)
```

The GitHub release `0.0.0+20260814092311.8b45283` names `revision`
`8b45283856d714f8ff1687e102b0237de851d86c`, and the release notes' Verification
block names it twice (`Revision:` and `Gate:`). That commit lives only on the
integration branch. `main`'s equivalent is `a8077295`, with a different tree —
and the **entire** difference is the campaign's own authority file, i.e. the
three ε2 amendments the integration branch never absorbed (F31, unresolved).

Anyone who clones the repo and tries to check out the released revision from
mainline history fails. The version string embeds the short sha of a commit
that is not in the published line of development.

### PA-22 — `stable_publish_branch` bakes the registration id into durable ref names, and the bridge tolerates it while the integration lookup does not

`crates/tally-core/src/campaign_folds.rs:188`:

```rust
format!("tally/{campaign}-campaign-{campaign_id}/{task_id}{suffix}")
```

The F44 bridge was explicitly built to tolerate this (report: "It therefore
accepts an earlier registration of the same campaign identity rather than
assuming the currently armed registration ID"), scanning all
`refs/heads/tally/` and matching any generation segment. But the *integration*
ref is looked up by exact name under the current registration id
(`campaign.rs:1497-1505`). **The same file contains both the tolerant lookup
and the intolerant one, three hundred lines apart**, and the intolerant one is
what forced the 12:04:36 hand-created branch.

### PA-23 — the release verb's live run, friction inventory

Everything observed in the one live execution:

| # | friction |
|---|---|
| a | requires ARMED registration after disarm is the documented terminal act (PA-16) |
| b | integration ref named after the current registration id (PA-22) |
| c | checkpoint refs missing at 12:28; restored by an unrecorded act (census row 14) |
| d | closing summary required live; archive ceremony deletes it (PA-17) |
| e | 19/19 proofs via the weaker bridge oracle, unlabeled in durable output (PA-15) |
| f | `planSha256` commits to a document stored nowhere (PA-15) |
| g | probe verdict conflates lifecycle with teardown (PA-20) |
| h | no `gh` scope preflight (PA-20) |
| i | orphaned probe repo + disabled orphan collector (PA-20) |
| j | released revision unreachable from `main` (PA-21) |
| k | release notes' change list disagrees with `main`'s history line-for-line (PA-24) |
| l | the binary used was a fresh build of `e921cccc`; the deployed pin `40957154` predates the release verb — **the release was cut by a tool that is not the installed tool** |

Item (l) is worth its own sentence. `AUG13-RUN.md`: "Release binary used for
the release acts: fresh build of `e921cccc` (the deployed pin `40957154`
predates the release verb…)". D49 self-hosting was achieved with a binary the
fleet does not run.

### PA-24 — the release notes and `main`'s history are two different changelogs

The notes' `## Changes` list is **exactly** the 19 ε2 task-branch commit
subjects (verified: 19/19 set-equal, zero on either side without a match). The
subjects on `main` are the squash-layer template subjects. They never agree:

| release notes | `main` |
|---|---|
| `feat(tally-core): port shared campaign folds` | `port-shared-campaign-folds: Port the driver's pure campaign folds into tally-core, once` |
| `feat(campaign): render local release plans` | `release-plan-render: Rust-native release plan: tally campaign release --plan` |
| `declare-canonical-derived: Declare canonical and derived surfaces` | `declare-canonical-derived: Declare the canonical-versus-derived split in tally-core` |

The Aug-11 ruling (`AUG11-evening-pass.md`) is unambiguous: *"No versions (the
flake pin is the contract), no releases by default, `git log --oneline` is the
changelog."* Epsilon shipped a release whose changelog is a **different
document** from the one the ruling designates, in a **different grammar**, and
neither is the conventional-commit form the ruling actually specified.

### PA-25 — the agent wrote 10 valid conventional-commit subjects and the machine threw all 10 away

Classifying every one of the 37 epsilon task-branch tip subjects:

| stage | registration | CONV | TMPL |
|---|---|---|---|
| ε0 | `019ffbb8` | 2 | 2 |
| ε1 | `019ffc34` | 1 | 13 |
| ε2 | `019ffdeb` | 7 | 12 |
| **total** | | **10** | **27** |

Examples of what the lane agents produced unaided: `fix(driver): preserve
negations in inline code`, `test(final-bar): repair local-only campaign
coverage`, `fix(tally-core): box calendar producer config`,
`feat(crates/tally): probe the release lifecycle`.

**All 37 merged onto `main` as `taskid: Title`.** The squash layer discards a
correct, in-grammar subject that already exists in order to fall back to a
template, because the *narrator* — a different component — failed. The system
already held the right answer, in the right place, 10 times, and did not look.

### PA-26 — the narrator's 74 rejections are 100% machine-repairable, and 54% are a JSON bug

Across all 37 epsilon merges (`git log --format=%B` over `52eff4db..e921cccc`):

```
commits with rejection note: 37   total proposals rejected: 74   accepted: 0
  40  final message is not valid JSON
  12  body wraps past 100 columns
  12  proposal body leading sentence must end with a period
   3  header is 75 characters, over the 72 cap
   2  proposal body must open with a past-tense verb
   2  proposal body contains an exclamation mark
   1  header is 85 characters, over the 72 cap
   1  header is 80 characters, over the 72 cap
   1  header is 76 characters, over the 72 cap
```

- 40/74 (54%) is an **envelope bug**, not a grammar failure — the model's
  output never reached the validator as JSON.
- 12 "wraps past 100 columns" → `fold -s -w 100`.
- 12 "must end with a period" → append `.`.
- 5 "header over cap" → the prompt does not state the real budget.
- **Every remaining category is a deterministic text transformation the
  validator could perform instead of rejecting.**

Verbatim from `427ca0cc`'s body:

> Rejected 2 steward narration proposal(s) and used the task-id template
> instead. Reasons: attempt 1 (rejected): body wraps past 100 columns;
> attempt 2 (rejected): final message is not valid JSON.

The observability is genuinely excellent — the reason is recorded in the
permanent commit. The behaviour is not: a validator that can name the defect
precisely and refuses to fix it is a gate pretending to be a linter.

### PA-27 — F15's exclamation-mark gag was fixed in one validator and survives in the other

ε0's `steering-grammar-negation` (`2d68fca9`) exists solely to fix F15 — *"the
steering grammar cannot express a Nix negation… A grammar that forbids `!`
cannot state a negation in Nix, Rust, C, or shell."*

Its own merge commit body:

> `steering-grammar-negation: Let machine diagnoses state a negation without
> being gagged`
>
> Rejected 2 steward narration proposal(s) … Reasons: **attempt 1 (rejected):
> proposal body contains an exclamation mark**; attempt 2 (rejected): proposal
> body leading sentence must end with a period.

The task that removed the `!` ban had its own commit message gagged by the `!`
ban — in a *different* validator, in the same repo, on the same day. Two more
instances appear in the epsilon corpus. **This is the sharpest single example
in the week of a restriction that reversed the rule it was meant to serve, and
it is not recorded anywhere.**

### PA-28 — the ε2 gate cost was a lint class the campaign chose not to run

The committed `campaign.gates` in `silent-factory-worklists/epsilon.json` is
exactly three gates:

```
driver-suite  : python3 test/spec_build_driver_test.py         (900s)
cargo-tests   : nix develop --command cargo test --workspace  (3600s)
flake-eval    : nix flake check --no-build                    (1800s)
```

No `clippy`. F33 already priced this ("gate-only lint classes"); ε2's single
gate cycle was `clippy large_enum_variant` and its repair
(`schema-example-stderr-lint`) needed a worklist amendment, a re-arm and a
pardon (receipt 34). One line in a JSON array would have erased it. The
mechanism to do so shipped in D74/D77 — *"changing a gate is a worklist
commit, never a deploy"* — and was not used.

### PA-29 — the tail's ratio

19 lanes + gate merged between 04:10 and 11:23 CEST: **7h13m of machine work,
2 human acts** (one grant, one amendment).

11:23 → 12:49: **1h26m of ceremony, ~14 human acts, zero machine work
dispatched by tally.** The close cost 20% of the wall clock of the wave it
closed, and 100% of it was frontier-supervised.

---

## 5. PART C — RECURRING PROCESS PATTERNS ACROSS THE WEEK

### PA-30 — the handoff document is the real orchestration primitive, and it is prose

Every phase transition in the week is a hand-written brief:
`AUG12-HANDOFF.md` (110 lines), `AUG12-DAYRUN-HANDOFF.md` (123 lines), the
three codex `brief.md`/`prompt.md` pairs, plus the plan's Part 6/Part 7
amendments. These carry the load that no verb carries: sequencing,
prohibitions, expected weather, definition of done.

`AUG12-HANDOFF.md` reads, verbatim:

> PROHIBITED, absolutely: any GitHub issue/comment/sub-issue creation for
> tracking or narration… No hand edit to tally source, nix modules, skills, or
> tests (all code changes ride campaigns…). No fleet deploy of any kind. No
> restarting any tally-* unit while a campaign runs. No tags, releases, or
> workflows. No reading scratch state back into any ledger.

Every one of those prohibitions was honoured, and every one of them is
enforced by a model reading prose. **Six of the seven now have a mechanism**
(quiescent ExecCondition, gate ladder, ownership domains, campaign mutex) —
the prose was written before the mechanism and never retired after it.

### PA-31 — "record it and stop that chapter" is the week's single best rule

`AUG13-RUN.md`, ch0, at the F18 deadlock:

> Per the standing rule — *a chapter that cannot proceed without a hand edit is
> failure weather; record it and stop that chapter* — ch0 is stopped… The
> completion sentence is **not** printed.

and

> The one workaround I can see — standing up an unplanned eighth campaign on a
> fresh master with zero comments — is a structural improvisation outside the
> specified ladder, and it is one failed lane away from the identical deadlock…
> That is a coin-flip dressed as a plan, and it is the operator's call to make,
> not mine.

This is the rule that made the record trustworthy. **Encode it.**

### PA-32 — "the machine's list beats the operator's grep" — proven, then only half-obeyed

`AUGUST-12-LEARNINGS.md`:

> For `remove-gate-b-and-contract` a `grep -ci gitai` found 3 consumers; the
> machine, having actually compiled the tree, enumerated **9**… **Take the
> machine's list verbatim. Do not re-derive a narrower one.**

Epsilon obeyed it for `delete-python-driver` (receipt 30: "the amended task
owns `crates/tally`, `crates/tally-flow`, and `nix/lib` **per its own boundary
refusal**") and paid one burned attempt each time it did not. Diagnosis
accuracy across the ladder is recorded at 15-for-15, then 18-for-18, then
16-of-16 in epsilon. **The most reliable component in the system has no
authority.**

### PA-33 — "gate fails once, then passes" is now 5-for-5 and is budgeted, not fixed

F14 → F21 → ch2 → ε0 → ε1 → ε2. `AUG13-RUN.md`: *"'chapter gate fails once,
then passes' is the normal shape, not an alarm."* Five chapters, five cycles,
each costing one worklist amendment + re-arm + pardon. The remedy has been
identified three separate times (cheap flake subset as a lane gate; `clippy` in
the lane set; `--keep-going`) and only `--keep-going` shipped.

### PA-34 — the ownership contract's cost is falling fast, and it was doctrine that did it

| run | tasks | authority corrections | rate |
|---|---|---|---|
| ch0–ch2 | 34 | 14 (9 ownership) | 41% |
| epsilon | 37 | 12 (4 ownership) | 32% (ownership 11%) |

The improvement came from the F42 census-authoring bet (author ε2 only after
ε1 merged, against the *observed* tree) and from H1 shipping
`brief-carries-conflict-domains`. **The doctrine change outperformed every
lint.** F40 records the ownership preflight catching 0 of 4 real corrections.

### PA-35 — every self-hosting seam this week failed the same way: the pieces were proven, the seam was not

Enumerated: F19/F20 (D62 pools landed in four layers, worked in zero),
F14/F21 (no lane gate evaluates the flake), F25 (the suite reads the operator's
deployed config), F39 (deletions merged green, crash-looped the daemon against
4,272 historical rows), F44 (the driver that writes proofs and the verb that
verifies them ship in different languages), PA-16 (the close ceremony and the
release preconditions).

`AUGUST-12-LEARNINGS.md` says it best:

> **A seam test is worth more than the four unit test suites that passed.**

Six occurrences in eight days. The gate ladder has no seam stage.

### PA-36 — the tool's own gate is forge-native and the tool is local-first

F28, PA-09, PA-10 row "PR opened solely to satisfy a gate stage" ×3, ε0's whole
first gate cycle. The repository's conformance bar requires: a commit
resolvable through `gh api`, and an **open pull request** whose head is that
commit. A local-first campaign produces neither by design. Three PRs (#604,
#605, #606) exist in this repo purely as gate fodder.

### PA-37 — deploy is the one act with no reversibility story that works

`AUG13-RUN.md`:

> `nixos-rebuild --rollback` is broken on this flake host (NIX_PATH legacy
> path); the working route is
> `nix-env --profile /nix/var/nix/profiles/system --switch-generation N` +
> `switch-to-configuration switch`.

Discovered at 02:50Z, during a fleet-down crash loop, by a model that had to
find the workaround live. The runbook in `AUGUST-11-OVERNIGHT.md` said
"rollback is one generation."

### PA-38 — the deploy branches are still local, three days on

`AUG14-LEARNINGS.md`: *"Deploy-1's `b2c61c0f` and deploy-2's `60afa885` both
ride dotfiles PR #225 whenever it merges; dotfiles `main` still pins
`78dd4871`… the running fleet is ahead of the declared fleet, and has been for
two days."* It is now three. The declarative host is a lie the operator is
maintaining by hand.

### PA-39 — the deploy-skip drop-in ceremony ran for four days before its own fix shipped

`skip-2026-08-13.conf` (re-stamped "each evening the ladder still runs"), then
`skip-ladder-through-2026-08-17.conf` ("replaces the evening re-stamp
ceremony"), then D63's `campaign quiescent` ExecCondition finally deleted it.
The fix was specified on Aug 11, merged Aug 12 in ch0, and did not take effect
until the pin deployed on Aug 13. **A ceremony's replacement cannot land while
the ceremony is protecting the ladder that is landing it** — the self-hosting
tax in its purest form.

### PA-40 — worker sourcing flip-flopped three times in 36 hours

Aug 13 early: "out-of-band repairs move from Codex to Claude Code on Opus."
Aug 13 midday: "the operator's Codex subscription returned… out-of-band repairs
return to Codex from chapter 3-epsilon onward." D76 then makes Codex both the
campaign agent and the repair worker. Exactly one repair (the ch2 gate repair)
was taken under the Opus directive. The contract survived all three flips
unchanged — *"dedicated git worktree, full task prompt, worker owns
implementation and validation, nothing pushed by the worker, orchestrator
merges only on green"* — which is the strongest evidence in the week that the
**adapter really is interchangeable**, which is tally's founding claim.

### PA-41 — the multi-harness thesis is proven and undocumented

Across the week, work was executed by: Codex lanes inside campaigns, Codex
out-of-band workers, one Claude/Opus out-of-band worker, and Claude sessions as
orchestrator. `AUG14-LEARNINGS.md` F43: *"no adapter-level defect."*
`AUGUST-12-LEARNINGS.md`: *"no adapter-level deaths observed across ~40
attempts."* The original motivation for tally.nix — extend ultracode's
JSON worklists to any harness — is the part of the system that has never
failed and has never been written up.

### PA-42 — receipts, captures and refs are excellent; the index over them is missing

Working durable surfaces: 34 attempt receipts, 25 archived captures, 88 tally
refs, 4 per-registration steering logs, 3 checkpoint refs, 7 summary refs, a
release record + notes + artifacts + probe receipt. Everything needed to
reconstruct the run is on disk, which is why this document exists.

What is missing is any verb that answers "what happened to this campaign, in
order, including what the human did." `campaign status` renders one pass
(F30). The receipts hold 17% of operator acts (PA-03). The steering log resets
per registration (PA-04). The closing summary is 100% dangling (PA-18).

### PA-43 — the escalation shape is the best human-facing artifact in the system

Receipt 3, verbatim and complete:

```
### Spec-build escalation: frontier quiescent

The worklist is incomplete and no unblocked task is dispatchable.
Tally stopped only after each directly blocked task failed twice with machine steering.

Directly blocked tasks: `chapter-gate`
Blocked worklist tasks (including descendants): 1

Accumulated machine diagnoses:
- `chapter-gate` attempt 1: Identified `chapter-gate` as a publication-order failure: `te...
- `chapter-gate` attempt 2: Observed chapter-gate fail before the gate ladder because tes...

Checkpoint captures:
- /home/tom/.local/state/tally/capture/archive/019ffbfe-…-chapter-gate/checkpoint.json
- /home/tom/.local/state/tally/capture/archive/019ffbff-…-chapter-gate/checkpoint.json
```

Bounded, honest, links its own forensics, readable at 3am. **Keep verbatim.**

### PA-44 — bounded escalation held all week

`AUGUST-12-LEARNINGS.md`: *"No loop ever burned more than two attempts before
stopping and asking."* Confirmed in epsilon: every diagnosis pair is exactly
`attempt 1` + `attempt 2`, 9 episodes, 18 diagnoses, zero runaways. This is the
restriction that most clearly earns its keep.

### PA-45 — the "no hand edits to tally source" prohibition also earned its keep

Zero orchestrator hand-edits to tally source across the epsilon run
(`AUG14-LEARNINGS.md`: *"every non-lane commit on `main` this run is a worklist
or plan document"* — verified against `git log`: every non-`tally spec-build`
author commit in the window is `docs(worklists)`, `docs(plan)`, or a merged
out-of-band PR that itself went through the full gate). The prohibition is what
made ch0's F18 stop-and-record possible instead of a silent patch.

### PA-46 — the frozen-flow decision was the week's best structural call

`AUG13-RUN.md`: freezing the flow at `2cc08bec` while the pin stayed at
`78dd4871` let ch1's `worklist-task-revision` and ch2's local-canon tasks
rewrite `examples/flows/spec-build.js` in the repo *without destabilising the
machinery grading them*. It is the correct general rule for any self-hosting
campaign and it is stated only in a run record.

### PA-47 — D77 is the week's proof that deletion beats configuration

`AUG14-LEARNINGS.md` F27: *"this was the single most consequential decision of
the run and it was made by *deleting* a mechanism two independent design agents
had both identified as central. The cheapest campaign mechanism is the one that
does not exist."* Operator ruling, verbatim: *"remove that roundabout way."*
`local_campaign_declaration_from_document` was deleted, not worked around.

**This is the exact posture the ceremony census argues for, applied once.**

---

## 6. THE THREE RESTRICTIONS THAT REVERSED THEIR OWN RULE

Stated plainly, because this is the operator's question:

1. **The lineage-doc merge-control prohibition** (PA-01). Written to stop
   ceremony narration; now the reason the week's entire forensic record is
   untracked in a project whose thesis is durable local state.

2. **The `!` ban in the narration validator** (PA-27). Written to keep commit
   prose calm; gagged the commit message of the very task that removed the same
   ban from the steering validator.

3. **The changelog-policy gate stage** (PA-09, PA-36). Written to make
   changelog discipline mechanical; now forces a local-first tool to open
   throwaway GitHub PRs, and does so as the *last* stage of a 17-minute ladder,
   inverting the cheap-fails-first ordering the same doctrine calls a win.

Two near-misses that will reverse next time:

4. **F38's "archive the summary refs at each stage close"** (PA-17) — one more
   application and the release verb fails closed on "multiple archived complete
   summaries".

5. **D74's "changing a gate is a worklist commit, never a deploy"** (PA-14) —
   correct and liberating, and it silently invalidates the completion proof of
   every already-merged task in the campaign.

---

## 7. ASKS AND DECISIONS

Ordered by ratio of ceremony removed to work required.

1. **Make `resume` automatic for the "attempts predate the amendment"
   predicate.** Nine of ten epsilon pardons are that one predicate (PA-05).
   Widen auto-pardon from "gained a dependency" (R11) to "the approved graph
   changed in any way that touches this task, or a task it depends on merged
   since the burn." This is the largest single ceremony reduction available.

2. **Ship three verbs that do not exist:** `campaign publish` (advance base
   from the integration branch, preserving trailers — replaces `shas.txt` +
   `bodies.txt` + `@@@END@@@`), `campaign archive-summaries` (F38's step, once,
   correctly ordered, idempotent), and `campaign grant <task> <path>...`
   (writes the worklist edit the diagnosis already names, commits, re-arms).

3. **Decide the release/close contract.** Either release accepts a disarmed
   completed campaign from durable state alone, or disarm stops being the
   terminal act. Today they are mutually exclusive and the gap was bridged with
   `git branch` (PA-16, PA-22). Also: stop naming the integration ref after the
   registration id, or make the release lookup as tolerant as the bridge
   already is.

4. **Write down the generalized F44 (PA-14) before epsilon-extension arms.**
   Any gate-set change or ownership grant rotates every task's completion
   revision. Either exclude execution policy from the completion identity, or
   accept that `bridge` is now the normal oracle and say so — and persist
   `completionProofs` into `release-artifacts-v1.json` so the record shows
   which proof was used.

5. **Fix the narrator envelope, then make the validator a formatter** (PA-26).
   54% of 74 rejections are "final message is not valid JSON". The other 46%
   are deterministic text transforms. And **look at the task branch first**
   (PA-25): the agent already wrote a valid conventional subject 10 times out
   of 37 and the machine discarded all 10.

6. **Move `changelog policy` to stage 0 of the fleet gate** (PA-09) and give it
   a local-audit arm (ε0's `gate-local-audit` already proved the pattern).
   One-line reorder; erases a whole class of 17-minute failures.

7. **Add `clippy` to the epsilon gate set** (PA-28). One line of JSON. Erases
   one of three gate cycles.

8. **Preflight `gh` scopes before the probe creates a repository** (PA-20), and
   report `releaseComplete` as the probe's verdict with teardown as a separate
   field. Then delete `mecattaf/tally-probe-20260814-6bf9bac2`.

9. **Commit the week's records** (PA-01), and settle whether `AUG13-RUN.md`'s
   claimed final archive (`eps2-final-*`) actually happened — it did not
   (PA-17), and `summary/complete` plus 19 dangling locators (PA-18) is the
   state a future stage will arm against.

10. **Reconcile `main` with the released revision** (PA-21) or stop cutting
    releases at the integration head. Today `0.0.0+20260814092311.8b45283`
    names a commit that is not an ancestor of `origin/main`.

11. **Encode "record it and stop" (PA-31) and the frozen-flow rule (PA-46)** as
    campaign mechanism rather than plan prose. They are the two best process
    inventions of the week and they live only in run records that are not in
    git.

---

## Appendix — epsilon by the numbers, final

| measure | value |
|---|---|
| campaign identity | `mecattaf/tally.nix` + `silent-factory-worklists/epsilon.json` |
| registrations (fresh arms) | 4 — `019ffbb8` (ε0), `019ffc34` (ε1), `019ffdeb` (ε2), `019fffba` (release) |
| merged lanes | 37 — ε0 4, ε1 14, ε2 19 (merge refs: 4 / 14 / 19 under three worklist digests) |
| chapter gates passed | 3 (checkpoint refs at `914c791f`, `6afee3aa`, `8b452838`) |
| gate failure episodes | 4 (8 chapter-gate diagnoses) |
| attempt receipts | 34 — 18 diagnosis, 10 pardon, 3 escalation, 3 retry |
| operator acts in receipts | 10 (all pardons, all `uid:1000`) |
| steers | **2** |
| worklist authority commits | 12 (3 stage authorings, 1 policy, 4 grants, 4 amendment tasks) |
| out-of-band repairs | 3 Codex runs, 3 PRs (#604 #605 #606), 4 hand-run fleet gates |
| narration proposals | 74 rejected, **0 accepted** |
| generations | 125 → 126 (crash-loop) → 125 (rollback) → 127 |
| release | `0.0.0+20260814092311.8b45283`, executed 10:49:29Z, `planSha256:c78fc039…` |
| probe | `releaseComplete: true`, `teardownComplete: false`, HTTP 403 |
| completion proofs | 19/19 `bridge`, 0 `exact`, none persisted |
