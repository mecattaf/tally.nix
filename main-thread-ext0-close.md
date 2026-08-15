# ext0 — main-thread run report (zeta operator session)

Written 2026-08-15 during the run, by the supervising operator session that
armed ext0 on the evening of Aug 14. Scope: acts A1–A4 of `ZETA.md`, plus the
supervision of `epsilon-extension` up to the chapter gate. Purpose: put every
mishap of this run into committed bytes, with its cause and its remedy, so the
same classes stop dragging on the project.

Status at time of writing: **8 of 10 tasks merged**, `authoring-doctrine-skills`
running, `chapter-gate` blocked behind it. The campaign has not been released;
A5 is deliberately not performed.

---

## 1. What landed

| act | result |
|---|---|
| A1 | `ca77150a` — the lineage record (E6): eight day-docs, `aug12-campaign-prep/`, `EPSILON-EXTENSION.md`, the worklist, all of `zeta-learnings/` |
| A2 | `8c42762b` — the spec layer: `specs/README.md` v2, `specs/constitution.md` v2, eight `epsilon-extension/evidence/` ledgers, `specs/zeta/` (proposed, empty trace, contracts), `skills/author-spec/`, `ZETA.md` |
| A3 | no act required — see §2.1 |
| A4 | ext0 armed, 10 tasks admitted |

Merged ext0 tasks (8): `receipt-authority-stamp`, `epoch-scoped-budgets`,
`summary-ref-stage-digest`, `outcome-envelope`, `judge-verdict`,
`subject-adoption-narrator-retire`, `final-bar-executes`,
`fleet-gate-cheap-first`. The entire machinery half of the extension is in.

Operator amendments during the run (all one-value, no schema keys, no gate
definitions touched):

| commit | change |
|---|---|
| `8e13bf1c` | `crates/tally/tests` → `receipt-authority-stamp` domains |
| `70697347` | `test/spec_build_driver_test.py` → `subject-adoption-narrator-retire` domains |
| `54371c81` | `test/spec_build_driver_test.py` → `receipt-authority-stamp` domains |
| `511117fd` | `campaign.agent` bound to the `claude-code` adapter |

---

## 2. Findings — the mishaps, by class

### 2.1 The declared fleet has been behind the running fleet for three days

Deploy-3 was already live when this session began: generation 128 (Aug 14
16:40) evaluates byte-identical to the dotfiles working tree, pinned at tally
`e921cccc`. No deploy act was needed.

But the deploy exists **only in the dotfiles working tree**. `dotfiles/main`
still pins `78dd4871`. This matters because `fleet-deploy.service` resolves its
candidate from `github:mecattaf/dotfiles/main`:

> running `fleet-deploy` today would move the fleet **backward** off deploy-3,
> mid-campaign.

**Remedy owed:** commit the dotfiles deploy (pin bump + the coredump-exclusion
work + the `gitAi` removal) so the declared fleet matches the running one.
Until then `fleet-deploy` is a trap, not a tool.

### 2.2 The nightly fleet deploy had no quiescence guard

`AUG14-LEARNINGS.md` records: *"the unit's `ExecCondition` is now D63's `tally
campaign quiescent`. The nightly deploy guards itself; nothing needs
re-stamping."*

**That is false against generation 128.** Verified during this run:

- `fleet-deploy.service` carries only `ExecStart` — no `ExecCondition`, no
  `Condition*`, no `Assert*`
- the `fleet-deploy` script contains no quiescence check
- the tally producer `nightly-fleet-deploy` has no condition field; it fires at
  `onCalendar: 02:00` and enqueues
  `sudo -n systemctl --wait start fleet-deploy.service`

The Aug-13 journal shows the guard working, so it was removed or lost between
then and gen 128. Combined with §2.1, the 02:00 timer would have regressed the
pin under a running campaign — the exact "never move the substrate
mid-campaign" hazard, in the one defect class (F39) with no coverage at all.

**Action taken:** the timer was stopped for the duration of the run.

    systemctl --user stop tally-producer-nightly-fleet-deploy.timer

**Remedy owed — two, and the second is the real one:**

1. **Restart the timer** when the run is over:
   `systemctl --user start tally-producer-nightly-fleet-deploy.timer`
2. **Restore the guard in committed bytes.** A doc claiming a guard exists is
   worth nothing; the guard must be an `ExecCondition` (or an equivalent
   predicate) that a check can witness. This is A15 applied to the deploy path:
   a bar without a gate is not a bar.

### 2.3 Four escalations, one defect class: acceptance criteria outside declared domains

Every ext0 escalation before the quota wall was the same structural defect — a
task whose own `acceptanceCriteria` require a path its `conflictDomains` omit,
making the task **unsatisfiable inside its ownership boundary**.

| task | criterion | path it needed | omitted |
|---|---|---|---|
| `receipt-authority-stamp` | `workspace-green` → `cargo test --workspace` | `crates/tally/tests/flow_live.rs` (asserts `schemaVersion == Some(1)`, which the task must bump) | `crates/tally/tests` |
| `subject-adoption-narrator-retire` | `driver-suite-green` → `python3 test/spec_build_driver_test.py` | the harness, whose `NarrationValidatorTests` assert the narration the goal orders deleted | `test/spec_build_driver_test.py` |
| `receipt-authority-stamp` (again) | `driver-suite` | the same harness, which never provisions the `receipt-authority-v1.json` the newly stamped writer requires | `test/spec_build_driver_test.py` |

**The lanes were right every time.** Each diagnosed its own boundary correctly
and refused to breach it; `subject-adoption-narrator-retire` twice declined to
reintroduce the narrator to make a stale test pass, and
`receipt-authority-stamp` declined to weaken the receipt writer to get a green
gate. This is F22/H1 behaving exactly as designed. The defect is in worklist
authoring, not in the agents.

**Remedy owed:** an authoring rule, and it belongs in `skills/assign-tally`
alongside the existing "rehearse admission" step —

> Before arming, check every task's `acceptanceCriteria` argv against its
> declared `conflictDomains`. Any path an acceptance command reads or writes
> that the task does not own is an under-declaration, and the task cannot pass.

This is mechanically checkable and should become one. It is the same shape as
the phantom-pointer class (`L13`) that `spec-lint-resolution` already exists to
catch, and it is worth pricing as a zeta follow-on: the worklist ↔ acceptance ↔
domain join is exactly the kind of enumeration the authority plane is for.

### 2.4 Attempt counters are not epoch-keyed, and the receipts cannot date themselves

`receipt-authority-stamp` was recorded as having failed twice. Both diagnoses
were **byte-identical**, cited the same pre-amendment `taskUuid`, and carried
evidence reading `"attempt":1`. The counter never reset when the amended graph
was re-admitted, so a single pre-amendment breach was counted twice and latched
the campaign.

Establishing that took a journal reconstruction, because **v1 receipts carry no
timestamp** — the operator could not prove from the ledger that the burned
attempts predated the amendment that fixed them.

That sentence is, verbatim, the CA-3 rationale in `receipt-authority-stamp`'s
own goal, and the counter behaviour is the CA-2/PA-05 defect
`epoch-scoped-budgets` exists to delete. **The campaign burned its own first
task on the two defects that task and its successor were written to fix.** Both
are now merged; neither is deployed, because the deployed store path grades
(frozen-flow rule). They take effect at the A6 boundary deploy.

No remedy owed beyond deploying them — but it is worth recording that this is
what "the machinery cannot describe its own failure" costs in operator hours.

### 2.5 The per-job memory cap is 8 GiB and it is hardcoded

The kernel OOM-killed `rustc` inside `tally-job-*` cgroups
(`constraint=CONSTRAINT_MEMCG`) **14 times in five hours** on
`epoch-scoped-budgets` alone, and again on `authoring-doctrine-skills`'s
`cargo-tests` gate. Earlier in the run it killed `fleet-gate-cheap-first`'s
`cargo-tests` too.

The host is nowhere near pressure: 125 GiB total, ~100 GiB available. The limit
is the per-job cap, `--memory-max-bytes 8589934592`, against a **32-core** host
where `cargo test --workspace` fans out 32 `rustc` processes in a cold
worktree.

**The damage was not the OOM — it was the disguise.** The kill surfaced as:

- `codex_core::tools::router: error=apply_patch verification failed`
- `exec_command failed ... CreateProcess Rejected`
- `finalMessage capture was not projected within 10000 ms`
- agent stages failing with an empty stderr tail

Every one of those reads as adapter flakiness. None of them says "out of
memory". Hours went into attributing this to the codex adapter before the
cgroup evidence surfaced.

A steer telling the lane to constrain build parallelism
(`CARGO_BUILD_JOBS=2`) worked — kills fell 14 → 3 and the lane completed. But
**a steer cannot reach a gate**: gate argv is fixed worklist bytes
(`nix develop --command cargo test --workspace`, no job limit), and a gate is
never changed mid-run.

**Action taken (operator-authorized):** the cap was raised to 24 GiB by systemd
drop-in and the daemon restarted.

    ~/.config/systemd/user/tally-daemon.service.d/override-memory-cap.conf
    systemctl --user restart tally-daemon.service

Verified live: `--memory-max-bytes 25769803776`, and running `tally-job-*`
units now report `MemoryMax=25769803776`.

**Remedy owed — this is runtime state and will not survive a rebuild.** The
value is hardcoded at `nix/modules/home-manager.nix:29` (and `nixos.nix:29`)
with no option to set it:

    "--memory-max-bytes"
    "8589934592"

The durable fix is a tally.nix change making the per-job cap a module option
with a sane default for the host class, landed **before the A6 boundary
deploy** — which is when it would take effect anyway, since the module comes
from the pinned flake input. Delete the drop-in once that is deployed.

Two second-order findings worth carrying:

- **A memory cap that manifests as adapter errors is a diagnosis defect.** An
  OOM-killed job should be classified as such in the attempt receipt, not left
  to look like a tool fault. This is `outcome-envelope`/`judge-verdict`
  territory and is a candidate ext1 task: the kill is visible in the journal
  (`Failed with result 'oom-kill'`) at the moment the unit dies.
- **Gates inherit the cap and cannot be steered.** Any per-lane environmental
  constraint that a gate cannot express is a hazard by construction.

### 2.6 The codex account hit its usage limit mid-run

`authoring-doctrine-skills` could not start a turn at all. Six captures under
`~/.local/state/tally/capture/` carry the error verbatim:

    {"type":"error","message":"You've hit your usage limit. ...
     or try again at Aug 20th, 2026 5:29 AM."}

It surfaced as empty-stderr agent faults and 10-second projection timeouts —
the same disguise as §2.5, from a different cause.

**Action taken (operator-directed):** `campaign.agent` was bound to the host
catalog's `claude-code` adapter (`511117fd`). Two things would have broken a
naive switch:

- the schema defaults are codex-shaped (`approvalPolicy: "never"`,
  `sandboxPolicy: "danger-full-access"`, `diagnosisSandboxPolicy: "read-only"`)
  and `claude-code` declares no policies; `render_policy` rejects any named
  policy an adapter has not declared, so all three must be explicitly `null`
- `cwdArgv: null` looks disqualifying but is not — the executor sets the lane
  working directory itself via systemd-run `--working-directory`

The adapter had **zero prior attestations** on this host, so it was verified
before arming rather than assumed:

    tally adapter smoke claude-code --assert-commit --pool campaign-agent

returned `verdict: pass`, `commitProbe: verified` — one commit descended from
the seeded base, clean worktree, both captures scraped. No model name entered
authority bytes; `~/.claude/settings.json` already resolves `opus`.

**Remedy owed:** the capture archive is where this was visible from the first
failure. The run doctrine already says captures are first-line forensics; this
session did not open them until the third escalation of the class. Worth
hardening into the escalation path itself — an escalation report that quoted
the tail of the adapter capture would have named this in one glance.

### 2.7 `campaign status` lags a live pass, and its failure list re-renders history

`tally campaign status` reports the last **reconciled** pass. While a newer
pass is in flight it shows `running=0` with stale counts, and re-reports old
failures as if new. This produced one false "stall" reading and one false
wake-up in this session's tooling.

The reliable liveness signal is `systemctl --user list-units 'tally-job-*'`.
The supervision watcher was corrected twice: to derive liveness from active job
units rather than `counts`, and to key new-failure detection on the newest
failure **timestamp** rather than the failure count.

**Remedy owed:** worth a line in `skills/campaign-operator` — the status view
is authoritative for the reconciled past, not for the present, and a supervisor
polling it must not treat `running=0` as quiescence.

### 2.8 A model name is already in the worklist bytes

`silent-factory-worklists/epsilon-extension.json` line 184, inside
`judge-verdict`'s goal:

> `AUGUST-01-DESIGN.md:138 assigned it to Sonnet and the implementation
> drifted`

Pre-existing, historical citation, in an already-merged task, and not edited
mid-run (record-don't-fix). It is a genuine instance of the `L16` class —
*model names in spec or governing worklist bytes* — and useful evidence that
the rule zeta is building has real targets in the existing corpus.

---

## 3. State handed forward

- **Base tip:** `511117fd` on `main`, pushed.
- **Campaign:** `epsilon-extension` armed, 8/10 merged, `authoring-doctrine-skills`
  running on `claude-code`, `chapter-gate` blocked behind it. Not released.
- **Deployed pin:** `e921cccc`, generation 128. Unchanged all run — no
  substrate moved under a running campaign.
- **Daemon:** per-job cap 24 GiB via runtime drop-in (§2.5).
- **Timer:** `tally-producer-nightly-fleet-deploy.timer` **stopped** (§2.2).

### Outstanding, in priority order

1. Restart the nightly fleet-deploy timer.
2. Commit the dotfiles deploy so `fleet-deploy` stops being a trap (§2.1).
3. Land the per-job memory cap as a module option before the A6 deploy (§2.5).
4. Restore a witnessed quiescence guard on the deploy path (§2.2).
5. Add the acceptance-argv ↔ conflictDomains check to `skills/assign-tally`,
   and price it as a mechanical rule for the linter (§2.3).

### Carried into the zeta sitting (A7)

- Zeta's five implementation tasks are agent tasks. On the `claude-code`
  adapter they are unblocked; on codex they would wait until Aug 20.
- Apply §2.3's rule to `zeta.json` before arming: every acceptance argv checked
  against declared domains. Zeta's tasks run `cargo test -p spec-lint`,
  `nix build .#checks.x86_64-linux.spec-lint`, and a doc check — each needs its
  read/write set owned.
- `DECISION-1` (the `steward` field value) and `UNKNOWN-1` (read-first brief
  rendering coverage) remain undrained, to be settled against the merged
  post-ext0 tree.
- The chapter-gate duration cap (10800s) should be re-checked against the
  merged `final-bar-executes` reality, per `ZETA.md` §Risks.
