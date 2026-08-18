# ext0 — main-thread run report and stop-state

Written 2026-08-14/15 by the supervising operator session that armed ext0.
Scope: `ZETA.md` acts A1–A4 and the supervision of `epsilon-extension` up to
the chapter gate. **Updated at a full stop on 2026-08-15**, at the operator's
instruction, so no learning paid for in this run is lost.

**Bottom line: nine of ten ext0 tasks are merged and none of them is running
anywhere. The chapter gate has never gone green, so the chapter is unproven,
and the deployed pin still predates every line of it.** The campaign is
stopped, not disarmed.

---

## 0. Stop state (read this first)

| thing | state |
|---|---|
| campaign | `needs-attention`, **still armed**, registration `01a001a3-2c64…` — deliberately NOT disarmed (disarm is terminal) |
| tasks | **9 done, 1 blocked** (`chapter-gate`) |
| job units | none active |
| poll timer | **stopped** (`tally-campaign-poll.timer`) — no new pass can dispatch |
| nightly deploy timer | **stopped** (`tally-producer-nightly-fleet-deploy.timer`) |
| daemon | active, per-job cap **24 GiB** via runtime drop-in |
| deployed pin | `e921cccc`, generation 128 — **unchanged for the entire run** |
| `main` | `1384ea38`, pushed |
| integration | `3c9600c3`, forked from `main` at `8c42762b` |
| supervision watcher | stopped |

To resume later: `systemctl --user start tally-campaign-poll.timer`, then
`tally campaign resume …`. To abandon: `tally campaign disarm …`.

### The nine merged lanes (on the integration branch, not on `main`)

    3c9600c3  authoring-doctrine-skills
    056502b7  epoch-scoped-budgets
    5456b806  subject-adoption-narrator-retire
    492a05ef  receipt-authority-stamp
    eb257f08  judge-verdict
    57c5f717  fleet-gate-cheap-first
    d09b3b36  outcome-envelope
    205f5bd5  summary-ref-stage-digest
    166ce059  final-bar-executes

Every one of these is **inert**. The deployed store path grades, and the pin
has not moved. Nothing ext0 built is running anywhere.

### Operator commits on `main` this run

    1384ea38  record: admit the ch1 ready-diff under the language charter
    c87136b3  record: the ext0 main-thread run report
    511117fd  epsilon-extension: move the agent slot to the claude-code adapter
    54371c81  epsilon-extension: grant the driver harness to receipt-authority-stamp too
    70697347  epsilon-extension: grant the driver harness to subject-adoption-narrator-retire
    8e13bf1c  epsilon-extension: declare crates/tally/tests for receipt-authority-stamp
    8c42762b  spec-layer: land the authority plane as authored bytes  (A2)
    ca77150a  record: commit the lineage record (E6)                  (A1)

### Ledger

26 attempt receipts: 15 diagnoses, **7 pardons**, 3 machinery retries, 1
escalation. Seven pardons is the number to look at — see §3.

---

## 1. The blocker that stopped the run

The chapter gate cannot pass, and not for a reason any lane controls.

    integration tip:       3c9600c3      ← the tree the gate tests
    merge-base with main:  8c42762b      ← A2
    main tip:              1384ea38      ← where the fix lives
    fix present in integration?  NO

The gate failed on `checks.x86_64-linux.language-entry-policy` rejecting
`aug12-campaign-prep/ch1-module-declaration.diff` — a file **I committed at
A1**, because `.diff` is absent from the admitted extension list at
`flake.nix:429`. In the same transcript: `PASS cargo test`, `PASS cargo
clippy`, `PASS cargo deny check`. The code was green; the record commit was
not.

I fixed it on `main` (`1384ea38`, rename to `.txt`, zero offenders remain).
**The fix cannot reach the gate.** The integration branch forked at A2 and only
accumulates lane merges; an operator tree fix landing on `main` afterward never
enters it. Worklist amendments worked all run because those are read from
`main` at admission — a *tree* fix has to be inside the branch being gated.

Merging `main` into integration by hand would take thirty seconds and is
exactly what `skills/campaign-operator` forbids: *"Do not edit receipts,
worktrees, integration branches, or the approved graph by hand."* The rule is
right — a hand-touched integration branch destroys the audit the gate exists to
produce.

**This is a structural gap in the deployed machinery, and it is the single most
important finding of the run:**

> On the deployed pin, there is no sanctioned path for an operator tree fix to
> reach a running campaign's integration branch. The content-disjoint rebase
> and re-gate of the rebased head is `publish-as-a-machine-stage`, which is
> **ext1** — not built, not deployed.

A campaign whose base needs a one-line correction after lanes have merged has
no way to accept it. That is why this run ended stopped rather than closed.

---

## 2. What this run cost, honestly

Wall clock: roughly 16 hours from arm to stop. Delivered: nine merged lanes,
none deployed, and an unproven chapter.

Consumed and not recoverable:

- **The codex account's quota**, exhausted mid-run, resetting **Aug 20 05:29**.
  Every codex-adapter task is blocked until then.
- Hours of attribution effort spent on faults that were disguised (§3.2, §3.5).
- Seven pardons. Each one is an operator act the destination says should not
  exist.

The user's summary of the position is accurate and worth writing down: tally
has been mutating for a month and there is still no fully-green tool. The
chapter gate — the thing that would prove a chapter — has not gone green in
this run.

---

## 3. Findings, with cause and remedy

### 3.1 A1/A2 were never gated, and the chapter gate paid for it

50 files were committed straight to `main` across two operator commits with no
check run against them. The defect surfaced 18 hours later at the most
expensive possible place: the campaign's final checkpoint.

**Remedy:** an operator commit that lands on the base of an armed campaign must
face the bar the lanes face. Minimum viable version, before any push:

    nix build --no-link .#checks.x86_64-linux.language-entry-policy

This is cheap, mechanical, and belongs in `skills/campaign-operator` next to
the arming steps. The general rule: *the operator is not exempt from the gate
ladder; the operator is simply the lane with no diagnosis loop.*

### 3.2 The per-job memory cap disguised itself as adapter flakiness

The kernel OOM-killed `rustc` inside `tally-job-*` cgroups
(`constraint=CONSTRAINT_MEMCG`) 14 times in five hours on
`epoch-scoped-budgets`, and again on two other lanes' `cargo-tests` gates. The
host was never near pressure: 125 GiB total, ~100 GiB free. The cap was 8 GiB
against a **32-core** host where `cargo test --workspace` fans out 32 `rustc`
processes.

The kill surfaced as `apply_patch verification failed`, `exec_command failed`,
`finalMessage not projected within 10000 ms`, and empty-stderr agent faults.
**None of them says "out of memory."** Hours went into blaming the codex
adapter.

**Action taken:** cap raised to 24 GiB via
`~/.config/systemd/user/tally-daemon.service.d/override-memory-cap.conf` and a
daemon restart. Verified live on running job units.

**Remedy owed:** this is runtime state and dies at the next rebuild. The value
is hardcoded at `nix/modules/home-manager.nix:29` and `nix/modules/nixos.nix:29`
with no option to set it. Make it a module option before the boundary deploy —
which is when it would take effect anyway, since the module comes from the
pinned flake input.

**Second-order remedy, more valuable than the first:** an OOM-killed job must
be *classified* as one. `systemd` records `Failed with result 'oom-kill'` at the
moment the unit dies; nothing reads it. A machinery fault that presents as a
tool fault will burn attempts every time it happens. This is ext1-shaped work
and it is cheap.

### 3.3 Four escalations, one defect class: acceptance criteria outside declared domains

Every escalation before the quota wall was the same: a task whose
`acceptanceCriteria` require a path its `conflictDomains` omit, making it
**unsatisfiable inside its ownership boundary**.

| task | criterion | path needed | omitted |
|---|---|---|---|
| `receipt-authority-stamp` | `workspace-green` | `crates/tally/tests/flow_live.rs` (pins `schemaVersion == Some(1)`, which the task must bump) | `crates/tally/tests` |
| `subject-adoption-narrator-retire` | `driver-suite-green` | the driver harness, whose `NarrationValidatorTests` assert the narration the goal deletes | `test/spec_build_driver_test.py` |
| `receipt-authority-stamp` (again) | `driver-suite` | the same harness, which never provisions the authority file the stamped writer requires | `test/spec_build_driver_test.py` |

**The lanes were right every time.** Each diagnosed its boundary correctly and
refused to breach it; `subject-adoption-narrator-retire` twice declined to
reintroduce the narrator to satisfy a stale test, and `receipt-authority-stamp`
declined to weaken the receipt writer for a green gate. F22/H1 behaving exactly
as designed. The defect is worklist authoring.

**Remedy:** mechanical, and it should become a lint rule —

> Every path an acceptance argv reads or writes must be inside the task's
> declared `conflictDomains`.

Same shape as the phantom-pointer class (`L13`) that `spec-lint-resolution`
already targets. Apply by hand to `zeta.json` before it is ever armed.

### 3.4 Attempt counters are not epoch-keyed, and receipts cannot date themselves

`receipt-authority-stamp` was recorded as failing twice. Both diagnoses were
byte-identical, cited the same pre-amendment `taskUuid`, and carried evidence
reading `"attempt":1`. One pre-amendment breach counted twice, and latched the
campaign. Proving it required reconstructing the timeline from the journal,
because **v1 receipts carry no timestamp**.

That is verbatim the CA-3 rationale in `receipt-authority-stamp`'s own goal,
and the counter behaviour is the CA-2/PA-05 defect `epoch-scoped-budgets`
deletes. **The campaign burned its first task on the two defects that task and
its successor were written to fix.** Both are merged. Neither is deployed.

### 3.5 The codex account hit its usage limit mid-run

`authoring-doctrine-skills` could not start a turn. Six captures under
`~/.local/state/tally/capture/` carry it verbatim: *"You've hit your usage
limit … try again at Aug 20th, 2026 5:29 AM."* It presented as empty-stderr
agent faults and projection timeouts — the same disguise as §3.2, different
cause.

**Action taken:** `campaign.agent` bound to the `claude-code` adapter
(`511117fd`). Two non-obvious requirements:

- the schema policy defaults are codex-shaped (`"never"`,
  `"danger-full-access"`, `"read-only"`); `claude-code` declares no policies,
  and `render_policy` rejects any policy an adapter has not declared, so all
  three must be explicitly `null`
- `cwdArgv: null` is harmless — the executor sets the lane working directory
  via systemd-run `--working-directory`

The adapter had **zero prior attestations**, so it was verified first:
`tally adapter smoke claude-code --assert-commit --pool campaign-agent` →
`verdict: pass`, `commitProbe: verified`. No model name entered authority
bytes; `~/.claude/settings.json` already resolves `opus`. It then merged the
task that codex could not start.

**Remedy owed:** the capture archive held the answer from the first failure.
The escalation report should quote the adapter capture tail — one glance would
have named this instead of three escalations of misattribution.

### 3.6 The nightly fleet deploy had no quiescence guard

`AUG14-LEARNINGS.md` records *"the nightly deploy guards itself."* **False
against generation 128.** `fleet-deploy.service` carries only `ExecStart`; the
script has no quiescence check; the producer has no condition and fires at
02:00, enqueuing `sudo -n systemctl --wait start fleet-deploy.service`.

Worse, `fleet-deploy` resolves from `github:mecattaf/dotfiles/main`, which
still pins tally `78dd4871`, while the running fleet is at `e921cccc` from an
**uncommitted** dotfiles working tree. Firing it would have moved the fleet
*backward* under a running campaign.

**Action taken:** timer stopped. **Remedy owed:** restart it when appropriate,
commit the dotfiles deploy so declared matches running, and restore the guard
as committed bytes a check can witness — a doc claiming a guard is worth
nothing (A15).

### 3.7 `campaign status` lags, and its failure list re-renders history

The status view reports the last **reconciled** pass. While a newer pass is in
flight it shows `running=0` with stale counts and re-reports old failures as
new. It produced one false stall reading and one false wake in this session's
tooling. Reliable liveness is `systemctl --user list-units 'tally-job-*'`.

**Remedy:** a line in `skills/campaign-operator` — the status view is
authoritative for the reconciled past, never for the present; `running=0` is
not quiescence.

### 3.8 A model name is already in worklist bytes

`silent-factory-worklists/epsilon-extension.json`, inside `judge-verdict`'s
goal: *"AUGUST-01-DESIGN.md:138 assigned it to Sonnet and the implementation
drifted."* Pre-existing historical citation in an already-merged task; left
alone (record-don't-fix). A genuine `L16` instance, and evidence the rule zeta
is building has real targets.

---

## 4. Another session is writing into this checkout

Untracked and not authored by this session:

    AUG15-SESSION-FINDINGS.md
    specs/substrate/
    zeta-learnings/12-local-models-synthesis.md
    zeta-learnings/13-final-state-portrait.md
    zeta-learnings/raw/tandem-architect.md
    zeta-learnings/raw/tandem-plane.md

`specs/substrate/` matters: it is a second identity directory under `specs/`,
which the linter will read and which the A7 falsity pass would meet as observed
tree. Not committed here — not this session's to commit — but it must be
reconciled before any spec-layer work proceeds.

---

## 5. The decisions this run has surfaced

Stated as forks, not recommendations, because they are yours.

1. **Can a chapter close without a green chapter gate?** Nine lanes are merged
   and unproven as a set. Deploying them (A6) without the gate means the
   frozen-flow rule starts grading code that no bar ever accepted. Refusing
   means ext0 stays open until §1's structural gap is closed.

2. **How does an operator tree fix reach a running campaign?** Today: it
   cannot. Either `publish-as-a-machine-stage` (ext1) gets pulled forward, or a
   narrow sanctioned verb is built for it, or campaigns must be re-armed from
   scratch whenever the base needs a correction — which discards merged lane
   work.

3. **Does ext0 re-run, or does its merged work get salvaged?** The nine lane
   commits exist and are good. A fresh campaign on the corrected base would
   re-derive them at full cost. Salvaging them needs a path that does not
   involve hand-editing an integration branch.

4. **Is the campaign the right instrument for machinery that the campaign
   itself runs on?** ext0 burned its first task on the two defects that task
   was written to fix, and its chapter gate on a file the operator committed.
   Every fix it produced is inert until a deploy that cannot happen until the
   gate it cannot pass goes green. That circularity is the month's central
   lesson and deserves an explicit ruling.

5. **Adapter strategy.** codex is unavailable until Aug 20. `claude-code` is
   proven on this host now — smoke-verified and it merged a real task. Zeta's
   five tasks are all agent tasks; on codex they wait, on `claude-code` they do
   not.

---

## 6. Outstanding, in priority order

1. Decide §5.1 and §5.2 — everything else waits on them.
2. Restart the poll timer and the nightly deploy timer when the run resumes
   (`systemctl --user start tally-campaign-poll.timer`,
   `… tally-producer-nightly-fleet-deploy.timer`).
3. Commit the dotfiles deploy so `fleet-deploy` stops being a trap (§3.6).
4. Land the per-job memory cap as a module option before the boundary deploy
   (§3.2).
5. Restore a witnessed quiescence guard on the deploy path (§3.6).
6. Add the acceptance-argv ↔ `conflictDomains` check to `skills/assign-tally`,
   and price it as a lint rule (§3.3).
7. Classify OOM kills as machinery faults in the receipt (§3.2).
8. Reconcile the other session's untracked files, `specs/substrate/` first (§4).
