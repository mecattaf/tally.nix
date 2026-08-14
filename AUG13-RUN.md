# AUG-13 RUN — silent-factory ladder, unsupervised steward record

Forensic record. Not an operator report. One section per chapter plus step 0.

Terminal state sought: ch0 → ch1 → ch2 → chP(=chR) → ch3 → ch4 → ch5, all seven
masters closed with `tally:campaign-complete:v1` receipts, all chapter merges
reachable from `origin/main`, 57 tasks settled.

---

## STEP 0 — preamble (2026-08-12, 10:30–10:37 CEST)

**F2 landed.** `nixos-rebuild build --flake .#coordinator` in `~/mecattaf/dotfiles`
was already complete from the background run (result →
`3pr53i3knin70fsfsndz6wdsc8svn6vn-nixos-system-coordinator-26.11.20260723.e2587ca`);
re-ran to confirm, exit 0. `sudo nixos-rebuild switch --flake .#coordinator` exit 0
at 10:34 — agenix generation 18, `home-manager-tom.service` restarted,
`tally-daemon.service` active afterwards, `tally query pools` protocol 5,
`tally campaign list` `[]`. Lock committed as `8e120e52`
*flake-lock-f2: flake: land the pending five-input lock update* (dotfiles).

**Deploy skip stamped.** New drop-in
`~/.config/systemd/user/tally-producer-nightly-fleet-deploy.service.d/skip-2026-08-13.conf`,
`ExecCondition` threshold **1786665600** = Fri 2026-08-14 02:00 CEST, so the
Aug-13 02:00 firing (epoch 1786579200) is skipped. Verified *loaded* via
`systemctl --user show -p DropInPaths -p ExecCondition`: both the superseded
Aug-12 drop-in and the new one are listed; the Aug-14 threshold is the binding
one. Re-stamp each evening the ladder still runs, until `campaign quiescent`
lands and replaces the ceremony.

**Authority committed and pushed.** One hand commit, exactly nine files
(`SILENT-FACTORY-PLAN.md`, `silent-factory-worklists/ch{0,1,2,3,4,5}.json`,
`chR.json`, `sodimo-aug11-learnings.md`): **`5be7e648`**
*docs(plan): aug-12 amendment — chapter 0, sodimo learnings, pointer repair*,
pushed `13307c67..5be7e648 main -> main`. The two modified skill files in the
working tree were deliberately **not** staged — skills are rewritten by ch2's
`skills-rewrite` through the gate ladder, never by hand.

Pre-commit verification: zero `final-plan-A.md` phantom pointers remain across
all seven worklists, and every `readFirst` file reference resolves on the
authority revision (D68) — 11 distinct paths checked, all present.

**Superseded registrations cleared.** `tally campaign list` was already `[]` on
arrival (the operator's pre-dispatch disarm of #529 held across the switch);
`tally campaign disarm .../529` returned `{"disarmed": false}` — nothing armed,
as expected, no-op. Closed #529 and #530–#535 as superseded, one line each.

Task count re-verified from the committed files: 9 + 6 + 18 + 6 + 6 + 7 + 5 = **57**.

---

## CH0 — silent-factory-ch0, pre-flight mechanism (D62–D65, D69–D70)

- **Master:** <https://github.com/mecattaf/tally.nix/issues/536>
  — title `campaign: silent-factory-ch0 — pre-flight mechanism (D62–D65, D69–D70)`,
  body = Part 6 §6.3 verbatim + task inventory + line-drift note + self-hosting
  notice + scope note.
- **Authority revision:** `5be7e648` on `origin/main`.
- **Projection:** input composed in the scratchpad — campaign object = the #467
  manifest minus tasks, `name silent-factory-ch0`, `maxTasks 9`, `maxParallel 2`,
  same codex agent / policy / six-gate set as #527; tasks = committed `ch0.json`
  verbatim except a one-line `body` added to the `chapter-gate` checkpoint task
  (the F11 workaround, last time — this chapter's own `checkpoint-brief-render`
  retires it). Sub-issues **#537–#545**.
- **Pre-arm verification:** `tally:campaign:v1`/`:end` and
  `tally:campaign-worklist:v1`/`:end` markers paired, prose intact (all three
  section headers present), 9 checkboxes, label `tally-campaign`.
- **Armed:** 2026-08-12 **10:36:46 CEST** → `disposition: created`,
  `state: "running"` (dispatched immediately; campaign pool free, estate quiet),
  attempt 1, projection `native-sub-issues`, registration/runner task_uuid
  `019ff51d-f64f-77e0-b1ce-2c5e27cbf123`, payload hash
  `sha256:36fce72f…411d70ee`. Ten arm-time cache-redirect warnings — the known
  F9 static lint, advisory on this no-hardening host; this chapter's
  `tier-aware-arm-lint` deletes them.
- **Supervision:** forge state + poll journal only; never the arm-time run uuid
  (sodimo F-01 — it goes silently stale). All `gh` calls pass
  `-R mecattaf/tally.nix` (sodimo F-06).

### Progress

**All eight implementation lanes merged, zero interventions, zero escalation
comments on the master.** `origin/main` reached `562d0b5f`:

| PR | task | merged (UTC) |
|---|---|---|
| #546 | `campaign-pool-namespace` (D62) | 09:22:11 |
| #547 | `mutex-restart-recovery` (D59) | 09:41:22 |
| #548 | `tier-aware-arm-lint` (D65) | 09:45:53 |
| #549 | `campaign-quiescent-verb` (D63) | 09:59:48 |
| #550 | `checkpoint-brief-render` (D64) | 10:17:14 |
| #551 | `campaign-status-verb` (D69) | 10:52:52 |
| #552 | `preflight-error-argv` (D70) | 11:12:50 |
| — | `poll-event-quality` (D69) | 11:45 (`562d0b5f`) |

### Failure weather — chapter-gate, and the recovery

`chapter-gate` dispatched at 11:47Z once its eight dependencies merged, and
**failed both attempts within 43 seconds** (11:47:28Z, 11:48:10Z). The campaign
then sat with the registration armed, no job units, master open, from 13:48
CEST until the operator asked for status at 15:42 CEST — ~1h54m of dead air.
The supervisor logged `jobs=0` from iter 65 onward but did not treat it as
trouble: my exit conditions were *master closed* or *registration vanished*, and
neither held. **Finding F13a — the supervisor's liveness predicate was wrong.**
An armed registration with zero job units and an open master is a stall, and it
is the shape a counter-exhausted campaign takes. Any future watcher must wake on
sustained `reg=1 && jobs=0`, not only on close or deregistration.

**Root cause — an invocation-contract defect in the worklists, not in the code.**
The checkpoint argv was `["bash","test/fleet-gate.sh"]`. `test/fleet-gate.sh:249`
is `[[ "$#" -eq 1 ]] || fail "usage: $0 <full-commit-sha>"`, and `:251` requires
a full lowercase 40-hex SHA. The gate died before a single check ran, so **no
verdict on the implementation was ever produced** — the eight merged lanes were
never in question. The machine diagnosed it correctly and identically on both
attempts and prescribed the exact repair:
`["bash","-lc","exec test/fleet-gate.sh \"$(git rev-parse HEAD)\""]`.

**Finding F13b — the defect was in all seven worklists**, ch0 through chR, and
would have tripped six more times. Pre-validation against
`normalize_task(require_conflict_domains=True)` cannot catch it: that is a schema
check, and this is an argv/invocation-contract error. D70's pre-arm freeze
rehearsal covers *gate preflight* argvs; **checkpoint argvs have no rehearsal at
all**. Candidate mechanism for a later chapter: extend the rehearsal to
checkpoint argvs, or give checkpoints a preflight of their own.

Repair, taken as the sanctioned per-chapter plan+worklist hand commit
(Part 5 §5.2 item 3), one line per file, machine's own prescription verbatim:
**`147d2493`** *docs(worklists): checkpoint gate argv — pass the full commit sha
fleet-gate.sh requires*, pushed. All 57 tasks re-validated against
`normalize_task(value, index, prior_ids, require_conflict_domains=True)` — clean.
(The two pre-existing skill-file edits were stashed across the rebase and
restored untouched; they remain uncommitted for ch2's `skills-rewrite`.)

**Finding F13c — disarm + re-project + re-arm does NOT clear exhausted counters.**
The full sequence ran clean: `disarm` → `{"disarmed": true}`; `project` converged
on the identical sub-issues #537–#545 and correctly reported all eight as
`merged`; `arm` at 15:45:20 CEST returned `state: "running"` with
`autoPardons: []`. The runner then **exited after 5.8 s wall / 274 ms CPU** with
nothing dispatched — the escalation survived. Two independent reasons, both
documented in the tree: auto-pardon fires *only* for an escalated task that
gained a dependency (`doc/audit/campaigns-steering-failure.md` R11;
`campaign.rs:1040-1092`), and mine was an argv change; and file-worklist tasks
carry no `revision` on this pin at all — which is exactly what ch1's
`worklist-task-revision` adds. **An operator who only knows `disarm/project/arm`
cannot recover this state**, and the arm output says `state: "running"` while
dispatching nothing, which reads as healthy. Worth a mechanism ask.

The correct verb was already there: **`tally campaign resume --reason <TEXT>`** —
"pardon an escalated campaign's counters, record why, and re-arm it", a
campaign-wide append-only boundary that restarts later receipts at attempt 1
(R17/R20). Run 15:46:11 CEST → `status: "resumed"`, `disposition: "created"`,
`state: "running"`, zero warnings, graph digest unchanged
(`sha256:ef7cfaa8…`), audit receipt at
<https://github.com/mecattaf/tally.nix/issues/536#issuecomment-5267667732>.
`chapter-gate` dispatched immediately at **attempt 1** as
`019ff639-60be-7b60-9781-ae836575d3b3`; campaign pool held 1 / STOP.

Supervisor rewritten with a stall predicate: exit 2 on `reg=1 && jobs=0`
sustained four ticks (720 s). It fired correctly at 16:11:25 CEST on the very
next stall — the predicate is proven, not just intended.

### Second failure — the argv repair worked, and the gate found a real defect

With the corrected argv the gate **ran**, reached `nix flake check`, and failed
on substance at 13:53:14Z and 13:59:54Z (attempts 1 and 2 of the pardoned
counter). From the checkpoint capture
(`019ff63f-c79b-78a3-ad2b-4e1bc0853d38.chapter-gate`), verbatim:

```
checking derivation checks.x86_64-linux.module-layer...
error: attribute 'campaign' missing
at .../worktree/flake.nix:4661:18:
  4661|   assert campaignSystemConfig.pools.campaign.resource == "mutex";
  4662|   assert campaignSystemConfig.pools.campaign.capacity == 1;
```

**Finding F14 — merged-green integration breakage; the chapter gate is the only
net that catches it.** `campaign-pool-namespace` (#546, D62) retired the generic
host-wide `campaign` pool from the module defaults but left `flake.nix`'s
`module-layer` contract asserting it exists. The lane's own six gates are `fmt`,
`no-stubs`, `deny`, `tests`, `clippy` — **none of them evaluates the flake**, so
the lane merged green and the breakage only surfaced at the chapter gate, after
merge, in a task with `nix-modules` in its conflict domains. This is the gate
ladder working as designed (the chapter gate *is* the integration oracle), but
it means any `nix-modules`/`flake-checks` domain task can merge broken and cost a
chapter-gate round trip. Candidate mechanism: a `flake-eval` lane gate for tasks
carrying those domains, or `nix flake check` in the standing gate set. Not
changed here — the proven #527 gate set is not something to mutate mid-ladder.

**Finding F15 — the steering grammar cannot express a Nix negation.** Both
diagnoses were *grammar-rejected* before they could steer anything: "Validation
rejected the proposal because diagnosis contains an exclamation mark." The
steward's correct fix is `assert !(campaignSystemConfig.pools ? campaign);` —
and the validator refuses it **because** of the `!`. The redacted excerpts on
#545 show the hole where the operator was punched out:
`assert .(campaignSystemConfig.pools ? campaign);`. A grammar that forbids `!`
cannot state a negation in Nix, Rust, C, or shell; the machine diagnosed
correctly twice and was silenced twice by its own validator. This is a genuine
mechanism defect and the sharpest finding of the chapter.

**Finding F16 — a checkpoint cannot repair code.** Even ungagged, the steering
loop had nowhere to go: `chapter-gate` is a `kind=checkpoint` task — an argv, no
worker, no worktree. The diagnosis prescribed a `flake.nix` edit that no
checkpoint can perform. A campaign whose integration gate fails on a code defect
has no in-band repair path at all; it can only re-run the same failing argv until
its counters are gone.

### Repair — a remediation lane, not a hand edit

The fix is in `flake.nix`, which is tally nix source, so it does not touch my
hands (prohibition honored). Added a tenth ch0 task
**`module-contract-pool-retirement`** (`conflictDomains: ["flake.nix"]`,
oracles `nix build --no-link .#checks.x86_64-linux.module-layer` and a negative
grep proving no assertion requires the generic pool), and added it to
`chapter-gate`'s dependencies. The goal states D62's invariant and explicitly
forbids reintroducing the pool, weakening the surviving resource-pool
assertions, or touching Rust/driver/worklists. Committed as **`3aed2210`**
*docs(worklists): ch0 — remediation lane for the module-layer pool contract left
stale by D62*, pushed. **The ladder is now 58 tasks, not 57** — one forced
addition, recorded here rather than silently absorbed.

Re-projected: sub-issues unchanged, new lane at **#554**, the eight still
reported `merged`. Armed 16:14:21 CEST; lane dispatched 16:15:10 CEST at
attempt 1.

**Finding F17 — disarming before projecting destroys the auto-pardon baseline.**
`chapter-gate` gained a dependency, which is precisely and only the condition
auto-pardon fires on (R11) — yet `autoPardons` came back `[]` again, with no
retained-escalation warning either. Per A08/R14, auto-pardon needs the prior
approved graph snapshot to *prove* the dependency was added, and my
`disarm` → `project` → `arm` sequence discards it. The recovery order matters:
`project` → `arm` (keeping the registration) would likely have auto-pardoned;
`disarm` first guarantees it cannot. Consequence for this run: `chapter-gate`
still carries its exhausted counters and will need one more manual `resume` once
#554 merges. Deliberately deferred rather than resumed mid-lane — the stall
detector is the wake signal, and it is now proven.

---

## LADDER HALTED — F18, a flow-engine defect that deadlocks its own repair

The remediation lane `module-contract-pool-retirement` (#554) **never reached
Codex.** Both attempts faulted at stage `steering:recheck` (14:14:43Z,
14:15:11Z) with `result-schema-mismatch` / `result-projection-timeout` — the
adapter died before it could project a `finalMessage`. The stall detector fired
again at 16:31:05 CEST. `chapter-gate` and #554 are both open; nothing else can
run.

**Root cause, directly observed — not inferred from the steward.** The driver's
own stderr, captured at
`~/.local/state/tally/capture/019ff653-202e-…-module-contract-pool-retirement.err`
and identically for attempt 2:

```
spec-build-driver: preparedComments[0].id must be a positive integer
```

That is `steering_comment` (`drivers/spec_build_driver.py:4624-4625`), which
requires `isinstance(identifier, int)`. The id arrives as a **float**. The
boundary is `value_to_json` → Boa's `JsValue::to_json`
(`crates/tally-flow/src/engine/interop.rs:3`): Boa holds small integers as
`JsValue::Integer(i32)` and everything larger as `Rational(f64)`, which
serde_json emits as `5266404097.0`. **Every GitHub comment ID today (~5.27 × 10⁹)
exceeds `i32::MAX`**, so every prepared steering comment reaches the driver as a
float and every implementation lane dies on it.

The four comments now on #536, all above the boundary, all tally's own writes:

| id | posted | kind |
|---|---|---|
| 5266404097 | 11:48:27Z | `tally:campaign-summary:v1` (quiescent) |
| 5266404321 | 11:48:29Z | `tally:spec-build:escalation:v1` |
| 5267667732 | 13:46:14Z | `tally:spec-build:resume:v1` (my resume receipt) |
| 5267833826 | 14:00:12Z | `tally:spec-build:escalation:v1` |

The timeline fits exactly: the eight lanes that succeeded all ran **before**
11:48:27Z, when the master had zero comments. The first implementation lane
attempted after that instant is the first one to die, and it died twice.

There is a **second, independent defect visible in the same error**: all four
comments are tally-authored system receipts, and the driver carries an explicit
`fail("preparedComments contains a system receipt")` at `:4794` — it expects the
flow to have filtered them out. They were not filtered. Either fix alone would
unblock; both are real.

**Blast radius beyond this chapter.** This is not a ch0 problem. Any campaign
master that acquires a single authorized steering comment paralyzes every
subsequent implementation lane on that campaign. ch1–ch5 and chP would each run
clean only for as long as nothing escalates and nothing posts a master receipt.
The machinery cannot steer, and cannot repair itself, once it has spoken once on
its own master issue.

**Why I stopped instead of improvising.** The fix is in
`crates/tally-flow/src/engine/interop.rs` and/or `examples/flows/spec-build.js` —
tally source. Hand-editing it is prohibited, and the prohibition is right: the
six-gate ladder is the whole point. The only in-band repair path is a codex lane,
and **the defect blocks every lane on this campaign**. The one workaround I can
see — standing up an unplanned eighth campaign on a fresh master with zero
comments, carrying the interop fix plus the flake fix — is a structural
improvisation outside the specified ladder, and it is one failed lane away from
the identical deadlock, because its own first escalation would post the comment
that poisons it. That is a coin-flip dressed as a plan, and it is the operator's
call to make, not mine.

Per the standing rule — *a chapter that cannot proceed without a hand edit is
failure weather; record it and stop that chapter* — ch0 is stopped, and because
ch0 gates the pin deploy and every chapter after it, the ladder is halted. The
completion sentence is **not** printed.

### State left for the operator (nothing cleaned up, deliberately)

- Registration for #536 **still armed**, zero job units, counters exhausted on
  both `chapter-gate` and `module-contract-pool-retirement`. Forensics intact.
- `origin/main` at `3aed2210`. Eight of ch0's ten tasks merged and green under
  their own gates; the ninth (`module-contract-pool-retirement`) never ran; the
  chapter gate never returned a verdict on the chapter.
- **`origin/main` currently fails `nix flake check`** (F14, `flake.nix:4661`).
  This is the state the repo is in right now — worth knowing before anything
  else is built from it.
- Deploy skip holds until **Fri 2026-08-14 02:00 CEST**. If the ladder stays
  halted past Thu evening, re-stamp or the fleet-deploy fires and moves the pin.
- **No pin deploy was performed.** The running tally is still the pre-ch0 pin;
  every ch0 improvement is merged-but-not-live, which is why none of the new
  supervision verbs (`campaign status`, truthful poll events) were available to
  diagnose any of this.
- dotfiles: `8e120e52` (F2 lock) landed and switched; no second lock commit.

### The four findings, ranked by what they cost

1. **F18** — Boa's `i32` integer boundary turns every GitHub comment ID into a
   JSON float; steering dies; the campaign cannot repair itself. Fleet-wide.
2. **F15** — the steering grammar forbids `!`, so the machine cannot state a
   negation in Nix, Rust, C or shell. It diagnosed F14 correctly twice and was
   gagged twice by its own validator.
3. **F16** — a `kind=checkpoint` task has no worker and no worktree, so a
   campaign whose integration gate fails on a code defect has no in-band repair
   path at all.
4. **F14** — no lane gate evaluates the flake, so `nix-modules` breakage merges
   green and surfaces only at the chapter gate, after merge.

F17 (disarm destroys the auto-pardon baseline) and F13a/b/c stand as recorded
above.

---

## RESUMPTION — operator authorized full repair (Aug 12 evening)

Operator ruling: "you have full permission to make edits to the tally campaign.
just get it fixed and to full completion." Coding work routed through Codex per
standing practice. Steward session change: this record is now continued by the
successor session.

**Diagnosis refinements on re-verification:**
- `origin/main` (3aed2210) **already carries the receipt filter**:
  `tally_authored_comment` in `crates/tally/src/cli/campaign.rs` excludes
  spec-build receipts, campaign-complete, and campaign-summary markers from
  steering, with a pinning test. The pin (13307c67) has zero occurrences —
  the second F18 defect is fixed by the pin deploy alone.
- The F14 flake breakage is confined to `checks.module-layer` (flake.nix:5050);
  the tally package build is unaffected, so the pin deploy from main+fix builds.
- `tally campaign resume` (main) re-arms in place: armSerial+1, fresh approved
  graph digest, resume receipt, counters pardoned, immediate dispatch — **no
  disarm needed**, so F17's baseline destruction is avoided entirely. It reuses
  the registration's recorded flow/driver store paths, which is safe: both
  fixes live in the tally binary (flow engine + fetch_campaign_steering), not
  in the recorded .js/driver files.

**Repair plan in flight:**
1. Codex worker (thread 019ff67e-4cce-79e2-bc83-c0614a6d6209, branch
   `fix/f18-interop-integer-json`) fixes `value_to_json` to preserve integral
   numbers as JSON integers. Merge to main on green cargo fmt/clippy/test.
2. Pin deploy = the mandated post-ch0 deploy, taken now because the fix must be
   live before any lane can run: dotfiles tally input → fixed rev, build,
   switch at zero-job quiescence, `flake-lock-ch0` commit.
3. `tally campaign resume --reason <F18 pardon>` on #536. Lane #554 fixes the
   flake in-band; chapter-gate rules; master closes.
4. Chapter loop ch1 → ch2 → chP → ch3 → ch4 → ch5 unchanged.

Deploy-skip verified still loaded: skip-2026-08-13.conf, threshold 1786665600
(Fri Aug 14 02:00 CEST).

**F19 — the enqueue RPC front door rejects the campaign/ namespace the lease
layer mints.** Discovered at first resume attempt on the new pin:
`tally: RPC error InvalidParams: unknown pool "campaign/mecattaf/tally.nix"`.
`campaign-pool-namespace` (90f99d00) taught the lease layer on-demand minting
(lease.rs:583,644) and made the nix module refuse explicit configuration of
campaign/ pools (common.nix:1793) — but the daemon enqueue RPC gate
(daemon/rpc/enqueue.rs:629) still requires every requested pool to be
configured. The merged mechanism is unreachable end-to-end: no campaign using
the D62 pool form can ever dispatch. ch0's own lanes never caught it because
the ch0 manifest was armed under the old pin with the old host-wide `campaign`
pool. Same shape as F14/F16: the gate ladder proves the pieces, nothing proves
the seam. Fix routed through the same Codex worker; will require one more pin
deploy at quiescence.

Sequence note: `campaign project --issue` re-projection converged on identical
sub-issue numbers with all 8 merged tasks recognized; resume posted its receipt
and advanced armSerial to 2 before dying at dispatch — counters are pardoned,
no task attempts were spent.

**F20 — the flow's own argsSchema rejects the campaign/ namespace.** After the
F19 deploy, resume dispatched a real pass which died at args validation:
`/campaignGraph/manifest/pool: "campaign/mecattaf/tally.nix" does not match
"^[A-Za-z0-9_][A-Za-z0-9_.-]*$"`. Fourth seam of the same half-landed D62
mechanism: CLI accepts, lease mints, daemon (F19 fix) admits, flow schema
refuses. Also: `campaign resume` preserves the registration's recorded
flow/driver store paths, so a resumed campaign keeps executing the OLD pin's
flow forever — only a fresh arm re-resolves packaged paths.

**Operator directive (Aug 12 evening): no further dotfiles/pin deploys until
the full ch5 run completes.** Standing workaround adopted: every arm from now
on passes `--flow /home/tom/mecattaf/tally.nix/examples/flows/spec-build.js`
(the committed repo bytes on main at arm time; the checkout is pulled only at
chapter boundaries, so the file is stable for a chapter's lifetime). The
engine and driver stay the deployed 78dd4871 pin. F20 fix routed to the same
Codex worker; ch0 recovery = disarm → arm --flow → resume.

**DEADLOCK BROKEN — 19:0x CEST.** After disarm → arm --no-enqueue --flow
<repo spec-build.js> → resume, pass 019ff6f7-e0d9 dispatched, and
steering:recheck on module-contract-pool-retirement PASSED for the first time
since the master acquired comments — the new engine serialized comment ids as
integers, the new fetch_steering filtered all seven tally receipts, the new
driver accepted the empty prepared set. Lane advanced to the agent stage
(2h budget). Registration 019ff6f7-c1d6, armSerial 2, flow snapshot under
campaigns/assets/<reg>/2/snapshots/flow (arm snapshots the --flow bytes; the
checkout can move without touching a live pass). F18/F19/F20 fix lineage on
main: 8e6285d5, ef679342, 78dd4871, 2cc08bec.

**Standing decision — the frozen ladder flow (honors the operator's no-deploy
directive AND the plan's own self-hosting rule).** The pin stays at 78dd4871
for the rest of the ladder. Every chapter arms with
`--flow /home/tom/.local/state/tally/ladder-assets/spec-build-2cc08bec.js`
(sha256 2b22661c…), a frozen copy of the flow at 2cc08bec. Rationale: the only
divergence between that flow and the pin's packaged flow is the F20 pool
pattern, so flow/driver/CLI stay a matched set; and freezing means ch1–ch5's
own edits to `examples/flows/spec-build.js`, `drivers/`, and `crates/` ride the
repo without ever reconfiguring the machinery that is grading them — which is
exactly what Part 6 §6.3 asks for ("ch1–ch5 changes ride the repo, not the
running pin; the end-state deploy is the operator's morning act"). No
`nixos-rebuild`, no flake.lock commit, no daemon restart until after ch5.

**Deploy-skip extended once, not nightly.** `skip-ladder-through-2026-08-17.conf`
(threshold 1787011200 = Mon Aug 18 02:00 CEST) replaces the evening re-stamp
ceremony for the rest of the ladder: the nightly fleet-deploy would move the pin
mid-ladder, which the frozen-flow decision above exists to prevent. Verified
loaded via `systemctl --user show`. Delete it (and the two dated drop-ins) when
the end-state deploy is taken.

Also verified for D68: every `readFirst.styleReferences` path across all seven
worklists exists on the current authority revision. `specSections` entries are
prose section references, not paths — they are not path-checked.

**ch0 chapter gate FAILED on 52b11b73 — F21, and it is F14 vindicated.**
`bash test/fleet-gate.sh` passed cargo fmt, cargo test, cargo clippy and
cargo deny, then `nix flake check -L` failed on exactly one attribute:
`checks.x86_64-linux.system-socket-execution`. The NixOS VM test drives
`tally campaign poll --once --wait` and its Python test script then raises
`KeyError: 'observed'` (test script line 232) — the poll output no longer
carries a key the test reads. Transcript:
`~/.local/state/tally-fleet-gate/transcripts/52b11b736af63b13a53fe88f080fb25a7838329b.log`
(~line 11930).

Prime suspect is `poll-event-quality` (562d0b5f), which deliberately reshaped
poll events and output — merged green this morning because, exactly as F14
predicted, **no lane gate evaluates the flake**, so VM-test and nix-module
breakage cannot be caught until the chapter gate runs. F14 is therefore no
longer a theoretical finding: it has now cost this chapter one full gate cycle.
F16 compounds it — `chapter-gate` is a checkpoint task with no worker, so the
campaign cannot repair this in-band; the fix must come from outside and the
gate be re-run.

Routed to a fresh Codex worker (thread 019ff707-99c8-77c1-85a1-674fcbdbea33,
branch `fix/vm-test-socket-execution`) with the explicit instruction to decide
which side is wrong — stale test vs. dropped contract field — and to check the
other VM tests for the same latent breakage, since `nix flake check` stops at
the first failing attribute.

**The machine diagnosed F21 correctly and completely (third time it has been
right when allowed to speak).** Its steering comment on #545:

> Diagnosed chapter-gate as stale assertions in `flake.nix` after the
> campaign-poll output contract changed.
> - `system-socket-execution` reads the removed aggregate key `observed`.
> - Update the edited-graph test to poll twice and assert `status ==
>   "stabilizing"` then `status == "rearm-required"`, using `issueUrl`,
>   `approvedGraphDigest`, `liveGraphDigest`.
> - Update the convergence assertion from `dispatched == 1` to
>   `status == "dispatched"`.

So the test side is stale, not the emitter — `poll-event-quality`'s new output
contract is the intended one — and there are at least three stale assertions,
not the single one the gate transcript surfaced (nix flake check stops at the
first failing attribute). The Codex worker was already told to hunt for exactly
this; its conclusion will be compared against this diagnosis before merge.

---

## CHAPTER 0 — COMPLETE (closed 2026-08-12T17:48:01Z)

Receipt: `tally:campaign-complete:v1`, 10 of 10 tasks settled against durable
merge/checkpoint facts. Worklist sha256:7c94a430… at 43f0a747. Nine merges
(PRs #546–#553, #555) each verified an ancestor of origin/main. Chapter gate
passed at 43f0a747. One reconciler warning, expected: the resume pardoned 11
earlier machine receipts.

Out-of-band repairs required to get ch0 through, all authored by Codex under
the full gate ladder, none hand-edited: 8e6285d5 (F18 integer JSON boundary),
ef679342 (nix package source filter), 78dd4871 (F19 daemon pool admission),
2cc08bec (F20 flow argsSchema pool pattern), 43f0a747 (F21 stale VM test
assertions). Pin deployed once at 78dd4871 and frozen there for the rest of the
ladder; flow frozen at 2cc08bec.

Estate verified quiescent (`tally campaign quiescent` exit 0, zero job units)
before arming ch1.

---

## CHAPTER 1 — ARMED

Master #556 (`campaign: silent-factory-ch1 — squash prerequisites + contract
fixes`), prose = the prepared aug12 body (plan Chapter 1 table verbatim +
line-drift note + self-hosting notice). Projected 6 tasks → sub-issues
#557–#562, markers paired, prose preserved. Armed 2026-08-12 evening on the
frozen ladder flow (`--flow …/ladder-assets/spec-build-2cc08bec.js`), pool
`campaign/mecattaf/tally.nix`, maxParallel 2, dispatched immediately at
attempt 1 with zero arm warnings and no auto-pardons needed.

**F22 — ch1 `squash-legacy-checkpoint-tag`: the worklist declared an ownership
boundary that excluded a file the task's own goal requires editing.** Both
attempts died at `ownership-*` with `agent produced no commit relative to the
prepared base`, and the machine diagnosed it identically both times: the
cleanup must edit `test/spec_build_checkpoint_receipts_test.py` (named
explicitly in the goal, lines 678/758), but `conflictDomains` listed only
`drivers`, `crates/tally`, `test/fixtures/spec-build` — and the test file is
under `test/`, not `test/fixtures/spec-build`. The lane could not fix this
itself: conflictDomains are campaign authority, not lane-editable.

Fixed by correcting the committed worklist (`ea152fef`), re-validated against
`normalize_task(require_conflict_domains=True)`, re-projected into #556, then
`resume`. No other ch1 task touches that file, so no new serialization. This is
a *worklist* defect class, distinct from F14/F16/F18–F21: the ladder's
pre-validation checked schema conformance but could not check that each task's
declared ownership actually covers the edits its own goal names. Worth a lint —
cross-check every path mentioned in `goal`/`acceptanceCriteria` against
`conflictDomains` — and worth auditing ch2–ch5 for the same shape before they
arm.

Note the timing hazard observed here: `git push` of the authority fix raced two
lanes merging. Pull-rebase and re-push is safe, but the two pre-existing
uncommitted `skills/*.md` amendments from the morning session block a rebase and
must be stashed around it. They are still uncommitted and are NOT this session's
work.

**F22 generalized, and caught before it cost anything.** Wrote an ownership
lint (scratchpad `ownership-lint.py`): for every task, extract repo-path tokens
from `goal` / `deliveredBehaviors` / `acceptanceCriteria` and report the ones no
declared `conflictDomain` covers. Run against ch1 it reproduced the known defect
and found two more of the same class, both still unstarted:

- `worklist-task-revision` — goal says "cover it with a driver test", owns only
  `drivers`, so writing `test/spec_build_driver_test.py` would have been
  rejected.
- `marker-single-arm` — deletes the v1 marker branch that same suite asserts on
  and runs that suite as an acceptance command, but did not own it.

Both corrected at b7976da8 before dispatch. They are already
dependency-serialized (marker-single-arm depends on worklist-task-revision), so
the shared domain costs no parallelism.

The lint's one remaining ch1 flag, `corpus-divergence-vectors`, is a confirmed
false positive: it only *runs* `test/spec_build_contract_corpus_test.py` as an
acceptance command, which needs no ownership — and the task merged green. So
the lint is advisory and needs the read-vs-edit triage a human (or the steward)
must apply; it is not a gate. Remaining chapters ch2–ch5 and chR will be linted
and triaged the same way before each arms.

**Ownership audit extended to ch2, with evidence rather than guesswork
(7d3d4e24).** The lint flagged nine tasks across ch2–ch5. Triage separated
GOAL-mentions (the task must edit the file) from AC-argv-only mentions (it only
runs the file, which needs no ownership) — and for the ambiguous middle, the
decisive test is whether the driver test asserts symbols the task deletes:

| symbol | refs in test/spec_build_driver_test.py | task that removes/rewrites it |
|---|---|---|
| `contiguous_receipts` | 1 | ch2 attempt-receipts-jsonl |
| `merge_local` | 14 | ch2 integration-branch-merge |
| `stable_publish_branch` | 11 | ch2 integration-branch-merge |
| `pull_request_marker` | 12 | ch2 marker-respell-trailers |
| `action_steering_recheck` | 0 | ch2 local-steering-source |
| `github_login` | 0 | ch2 local-steering-source |

The first three tasks now own the test. `local-steering-source` deliberately
does NOT: the suite references neither symbol it touches, and the plan's own
domain table for 2.2 lists no test domain. If that judgment is wrong the machine
will diagnose it in one attempt, exactly as it did for F22, and the correction
costs one resume.

`delete-forge-io-python` and `git-ai-test-doc-sweep` already own all of `test`,
so they were never at risk.

**ch3/ch4/ch5/chR deliberately NOT corrected yet.** Linting them now would be
unsound: ch2 rewrites the driver and its test suite wholesale, and ch5 deletes
the Python driver and corpus outright, so today's paths and symbol counts do not
describe the tree those chapters will meet. Each will be linted and triaged at
its own boundary, against the tree as it then stands. (Known carry-forward:
ch4 `dead-cuts` names `test/fleet-gate.sh` in its GOAL and does not own it —
that one is structural and will hold.)

**F23 — a campaign can come to rest with unblocked work outstanding, and the
poll will not restart it.** After `squash-legacy-checkpoint-tag` merged (PR
#565, confirming the F22 authority fix), ch1 settled to `idle`: 3 done, 1
pending (`worklist-task-revision`, dependencies `[]`, therefore dispatchable),
2 blocked behind it, zero job units. `tally campaign poll --once` returned
`status: unchanged` and dispatched nothing — the cheap forge-observation
precondition is stable precisely because nothing is running to move it, so the
campaign cannot wake itself. `tally campaign resume` forces a pass and clears it.

This is the exact signature the supervision watcher was built for (armed +
zero job units + master open), and it is a mechanism gap rather than operator
error: the poll's change-detection is sound for "has the forge moved" but is
not a liveness check for "is there dispatchable work with nothing running".
Deadlock is not permanent — any operator resume clears it — but an unattended
overnight ladder would sit here indefinitely. Worth a later chapter: either a
liveness arm in the poll (dispatchable work + no live nodes ⇒ dispatch) or a
terminal-pass invariant that a pass never ends leaving unblocked work unstarted.

Cost this time: caught within one watcher cycle. This is the same shape that
cost ~2 hours in the morning session before the watcher existed.

**F24 — an ownership gap the path lint provably cannot find.**
`worklist-task-revision` exhausted both attempts on the `tests` gate. The
machine diagnosed it three times, identically: the task adds a `revision` field
to file-worklist tasks in the driver, but `examples/flows/spec-build.js`
declares `implementationTaskSchema` and `checkpointTaskSchema` with
`additionalProperties: false`, so the new field is rejected and
`crates/tally/tests/flow_live.rs` fails before the driver suite is reached. The
flow is part of the change, not collateral — and the task owned neither the flow
nor `crates/tally-flow`.

This is a strictly harder class than F22. The F22 lint works on paths a task's
own text *names*; here the coupling is **semantic** — "a new field in a payload
must be admitted by a closed schema in another file" — and the task text names
neither the flow nor the schema. No textual lint can find it. The general shape:
**a task that widens a data shape must own every closed schema that shape passes
through.** For this codebase that means driver payload changes routinely need
`examples/flows/spec-build.js`, and flow-schema changes need `crates/tally-flow`
for the fixtures and goldens that assert them.

Corrected at ad5f3803; re-projected and resumed. `marker-single-arm` was checked
for the same exposure and deliberately not granted the flow: the flow contains
zero references to marker spellings (`grep -c` = 0), and the task changes no
task-payload shape.

Ladder-relevant consequence: the frozen-flow decision is now load-bearing in a
second way. This task edits the repo's `examples/flows/spec-build.js` while the
campaign executes the frozen copy at 2cc08bec, so a lane rewriting the flow
schema cannot destabilise the machinery that is grading it mid-chapter.

**ch1 chapter gate FAILED at 84276e99 — F14 again, second chapter running.**
cargo fmt/test/clippy/deny all passed; `nix flake check -L` failed on
`checks.x86_64-linux.spec-build-conflict-domains`:
`test_case_only_duplicate_domains_are_rejected` — `DriverError not raised`
(67 tests, 1 failure).

Cause is a *deliberate* behavior change colliding with a stale assertion.
`corpus-divergence-vectors` implemented D38 by aligning
`normalize_conflict_domains` to the Rust contract's exact `BTreeSet` dedup and
dropping the casefold, so `["Docs","docs"]` is now two distinct domains rather
than a rejected duplicate. `test/spec_build_conflict_domains_test.py:111` still
asserted the rejection. That test runs ONLY as a flake check, so — exactly as in
F21 — no lane gate could see it and it merged green.

Live nuance handed to the worker rather than decided here: `domains_overlap`
(driver ~:3602) still casefolds path parts, so the two domains are now accepted
as distinct at validation yet still collide for scheduling. That may be
perfectly coherent (accept both, never run them concurrently) or may be a real
contradiction; D38 and `crates/tally-core/src/campaign_contract.rs` are the
authority and the worker was told to decide deliberately and pin the answer with
a test, not to silence the assertion.

Routed to Codex thread 019ff78b-674f-7293-b7ed-5f691e702ba0, branch
`fix/conflict-domains-case-test`, with the standing instruction to check whether
a second stale assertion hides behind this one.

Pattern now firm across two chapters: **the chapter gate is the only thing that
runs the flake, so every chapter should be expected to cost one gate cycle to a
stale flake-only assertion.** Cheap and self-correcting, but it means "chapter
gate fails once, then passes" is the normal shape, not an alarm.

---

## CHAPTER 1 — COMPLETE

Receipt `tally:campaign-complete:v1`, 6 of 6 settled, worklist
sha256:bf2d7679… at 3ee84031. Merges PRs #563–#567 all verified ancestors of
origin/main. Chapter gate passed at 3ee84031 on its second run.

Cost of the chapter: three worklist-authority corrections (F22 ownership gap,
its proactive extension, F24 semantic schema gap), one poll-liveness stall
(F23), and one chapter-gate cycle to stale flake-only assertions — of which the
Codex repair found two more hiding behind the first.

## CHAPTER 2 — pre-corrected before arming

Applied the F24 lesson ahead of dispatch rather than after four failures
(6e8491fc). `examples/flows/spec-build.js` declares ~40 closed schemas covering
essentially every driver action result — steering, merge, publication,
integration, diagnosis, retry, escalation, ownership, treeDelta. Four ch2 tasks
rewrite exactly those surfaces and now own the flow plus `crates/tally-flow/tests`:

| task | flow surface it rewrites |
|---|---|
| local-steering-source | steeringSchema, steeringRecheckSchema, authorizedSteeringCommentSchema |
| attempt-receipts-jsonl | diagnosisFactSchema, retryFactSchema, escalationSchema |
| integration-branch-merge | mergeSchema, publicationSchema, integrationSchema |
| delete-forge-io-python | removes actions the flow orchestrates |

`remove-gate-b-and-contract` already owned `examples/flows`. The shared domain
also removes a genuine hazard: three of these would otherwise have edited that
one file concurrently in separate worktrees and collided at merge.

Deliberately NOT granted: `authority-schema-v3` and `port-local-semantics`
(Rust registry/CLI, no driver result shape), `marker-respell-trailers` (the flow
contains zero marker references). If any of those judgments is wrong the machine
diagnoses it in one attempt and the correction costs one resume — the same cheap
recovery already exercised four times today.

**F25 — the test suite reads the operator's deployed config, so a task that
tightens config parsing fails on ambient host state.** ch2 `remove-gate-a`
exhausted both attempts on `tests`. The machine's diagnosis was exact: three
tests in `crates/tally/tests/migrate_cli.rs` launch `tally migrate` with no
`--config`, so the child process loads `$HOME/.config/tally/config.json` — the
operator's real deployed config, still carrying the `gitAi` key that this very
task makes illegal. The suite fails on the host's state rather than on the
change, and `grep -c HOME crates/tally/tests/migrate_cli.rs` is 0: there is no
isolation at all.

This is a distinct class again — not an ownership *omission* (F22), not a
semantic schema coupling (F24), but **host-state leakage into the test suite**.
It is self-inflicted by self-hosting: only a campaign that edits the very tool
whose config is deployed on the grading host can hit it. It will recur for every
later task that narrows config acceptance, which is why the durable fix (isolate
the config home in that file) matters more than the unblock.

Corrected at 7f837ff0 by granting `crates/tally/tests/migrate_cli.rs` to
remove-gate-a — the in-band option the machine itself named, preferred over the
alternative of isolating HOME in the campaign's gate argv, because fixing the
test is better engineering than working around it and it benefits
`gitai-config-purge`, which depends on this task.

Accepted cost: that file overlaps `crates/tally`, owned by four other ch2 tasks,
so remove-gate-a now serializes against them. Correct rather than merely
tolerable — they would otherwise edit the same crate concurrently.

**F26 — the end-to-end test asserts the very behaviour the chapter replaces.**
ch2 `integration-branch-merge` exhausted both attempts on `tests`.
`crates/tally/tests/flow_live.rs` asserts that squash merges publish receipt
refs and commits to `origin/main` — precisely what D14–D15 replace with a local
integration branch. The machine named the boundary in as many words: "the
current boundary cannot legally fix the failing test", and warned against the
tempting wrong fix ("Do not restore remote pushes; that contradicts D14–D15").
Corrected at 4249b741.

`delete-forge-io-python` received the same grant pre-emptively: it removes the
forge I/O that same end-to-end test exercises, and it owns `test/` but not
`crates/tally/tests`, so the identical wall was waiting for it.

Running tally of ownership corrections: F22 (path named in goal), F24 (closed
schema the payload must pass), F25 (host config leaking into the suite), F26
(end-to-end test asserting the replaced behaviour). All four are the same
underlying omission — **a task must own every file that its change makes
false** — and only the first is findable by a textual lint. The other three
require knowing what the change means. For ch3–ch5 the practical rule is: before
arming, ask of each task not "which paths does it name" but "which existing
assertions does this make wrong, and does it own them".

---

**OPERATOR DIRECTIVE (Aug 13, early hours): run ch2 to completion, then stop.**
chP, ch3, ch4 and ch5 are NOT to be armed by this session. The ladder ends for
now at the close of #568. Standing constraints unchanged: pin frozen at
78dd4871, flow frozen at 2cc08bec, all code through Codex, deploy-skip holds
through Aug 18.

**Worker change (operator directive, Aug 13): out-of-band repairs move from
Codex to Claude Code on Opus**, effective from the next repair task. Same
contract as before — dedicated git worktree, full task prompt, worker owns
implementation and validation, nothing pushed by the worker, orchestrator merges
only on green. Codex threads used earlier today (F18–F21, ch1 gate repair)
remain the record for those fixes. Lane workers inside the campaign are
unaffected: the campaign manifest still dispatches its own agent adapter, and
changing that would require re-projecting authority mid-chapter.

(Reverted for the next chapter: the operator's Codex subscription returned on
Aug 13 midday — out-of-band repairs return to Codex from chapter 3-epsilon
onward. The one repair taken under the Opus directive is the ch2 gate repair
below.)

---

## CHAPTER 2 — COMPLETE (closed 2026-08-13, receipt 12:08:20Z)

Receipt `tally:campaign-complete:v1`, **18 of 18 settled**, worklist
sha256:fde8ad81… at `52eff4db`. Seventeen merges (PRs #587–#603) each verified
an ancestor of `origin/main`; chapter gate passed at `52eff4db` on its second
run. Registration pruned, `tally campaign quiescent` exit 0.

The gate followed the now-firm two-chapter pattern (F14/F21, 3-for-3): first
run failed both attempts on flake-only stale assertions —
`spec-build-checkpoint-receipts` (3/21) — and the repair's mandated
`--keep-going` sweep found **two more suites hiding behind it**
(`spec-build-two-repo` 4/16, `spec-build-conflict-domains` 40/67), each a stale
assertion meeting a deliberate ch2 change (D13 attempt-receipts JSONL, D21
trailer markers, D14–D15 local integration). Repair `52eff4db` — the one
out-of-band fix authored by a Claude/Opus worker under the Aug-13 directive —
was test-only (3 files, no driver code), merged as a fast-forward from the
worker's worktree, followed by one `resume`; the gate then passed at attempt 1.

**LADDER STOPPED HERE by operator directive.** chP/ch3/ch4/ch5/chR are NOT
armed. The remainder was redesigned on Aug 13 as the staged **chapter
3-epsilon** pass (2 Fable proposals + 1 Opus devil's-advocate critique,
synthesis minted; see the session record): ε0 shakedown (3 tasks, first
local-mode campaign) → ε1 deletion wave (~13) → ε2 build wave (~17), two
deploys at the stage boundaries, worklists to be authored as a Part 7 plan
amendment against the post-ch2 tree.

Next act is the operator's **deploy-1**: dotfiles flake bump to `52eff4db` +
`nixos-rebuild switch`, paired with (1) removing the `gitAi` key from
`~/.config/tally/config.json` — `gitai-config-purge` makes the stale key a
loud boot refusal, key verified still present; (2) deleting
`skip-ladder-through-2026-08-17.conf` and the two dated deploy-skip drop-ins
(D63's quiescent ExecCondition is live once deployed); (3) discarding the two
stale uncommitted `skills/*.md` edits superseded by the merged skills-rewrite.
Known pre-existing breakage riding to ε0: four `test/final-bar` call sites
still pass the deleted `--allow-test-local-forge`, and no gate covers
final-bar (the flake harness check only runs `--list`).

## D77 — SELF-CONTAINED ARM (landed out-of-band, 2026-08-13 ~14:50Z)

Deploy-1 landed (pin `52eff4db` via dotfiles PR #225's branch, switched but
unmerged; dotfiles main still pins `78dd4871`). Part 7 + the ε0 worklist were
authored and pushed (`6927848c`, `1953bb49`). Then the operator rejected the
per-campaign dotfiles declaration outright ("remove that roundabout way") —
the prepared `services.tally.campaigns.epsilon` branch was discarded and the
mechanism itself was removed instead.

**D77**: `tally campaign arm <owner/repo> <worklist>` is self-contained.
Campaign policy lives in the worklist's closed `campaign` section; adapters
resolve from the host catalog; flow/driver default to the packaged assets
beside the binary (`share/tally/flows/spec-build.js`,
`libexec/tally/spec-build-driver`); the campaign mutex is the reserved minted
`campaign/<owner>/<repo>` pool; `--checkout`/cwd binds the repository into
registration authority v4, which resume and poll read;
`local_campaign_declaration_from_document` is deleted. Implemented by a
Codex worker (first Codex out-of-band worker of the ladder) in an isolated
worktree from `tally-codex-runs/2026-08-13-arm-self-contained/brief.md`;
report in the same directory. Fleet-gate history: first run exit 2 (commit
not on remote — the gate cross-checks PRs), second run failed ONLY the
changelog-policy stage (CHANGELOG entry requires an open PR), PR #604 opened,
third run PASS on `7c2b4954`. Fast-forward pushed to main (PR #604 marked
merged), then `996e42ae` (plan: D77, P1 re-scoped to nix-surface deletion)
and `6a7c841a` (epsilon.json gains its campaign section, validated against
the merged driver). ε1's P1 is now `campaign-nix-surface-retire`.

Consequence for the ladder: dotfiles never carries campaign config. The
remaining operator act before ε0: extend PR #225's branch with
`nix flake update tally` (→ `6a7c841a`) + switch. Preflight of exactly that
lock bump: eval/build of the rendered tally config in a scratch worktree —
running at the time of this note; arm follows on its heels.

## EPSILON STAGE 0 — COMPLETE (2026-08-13, armed 15:23Z, closed ~17:20Z)

First local-mode campaign, first D77 arm. 4 implementation tasks + gate, all
green; main advanced 19bd53af → 6fdf108f (rebase-publish of integration head
914c791f — proven sha and published sha recorded as distinct because the
integration branch does not rebase onto operator worklist commits; see
finding 5). Chapter-gate checkpoint ref pushed durably
(refs/tally/spec-build/v1/34af0056…/chapter-gate-ca88…/914c791f).

Shakedown ledger (all items exercised):
- Local arm: self-contained, packaged assets resolved, registration v4. ✓
- Task-addressed steer: recorded seq 1, dispatch fence honored. ✓
- Worklist-correction cycle (OQ3 settled): edit → validate → push → RE-ARM
  same identity (never disarm); autoPardons recorded the amendment delta with
  a durable receipt; 3 completed tasks preserved. ✓
- Completion semantics: campaign reaches `complete` but STAYS ARMED —
  disarm is the operator's terminal act; base advance is the operator's
  publish (machine pushes only the checkpoint ref). ✓
- F18 large-id: structurally absent in local mode (steering ids are small
  sequences). Noted, not pinned.

Findings:
1. `campaign status` renders the LATEST pass — after a steer/re-arm that is
   a queued un-reconciled pass: empty table, zero counts, placeholder name
   "Campaign campaign". Truth lives in `tally query run <pass-id>`. Fix
   rides ε1 as H4 status-renders-reconciled-truth.
2. Steward narration fell back to template subjects ("task-id: Title") on
   all four merges — narrator shim likely failing headless; investigate
   during ε1 (non-blocking by design; commit grammar still valid).
3. Chapter gate failed 2 attempts on the PREDICTED defect (fleet-gate
   hard-fails on a commit absent from the remote; changelog stage demands a
   PR) — repaired IN-CAMPAIGN via the gate-local-audit amendment task.
   Machine diagnosis correct both times: streak now 15-for-15. Gate pattern
   "fails then passes" holds 4-for-4 chapters.
4. Merges are deferred while a sibling agent holds the base (no mid-attempt
   rebase); lanes park "pending" after gates until the last agent exits.
5. The integration branch cuts from the base at arm and does NOT absorb
   operator commits pushed to main mid-campaign (the worklist amendment) —
   publish therefore needed a content-disjoint rebase. Candidate ε1 goal
   note for the advance-base path; not blocking.
6. Escalation shape: "frontier quiescent" with directly-blocked task list
   and accumulated diagnoses in attempt-receipts JSONL — readable, honest.

Next: author ε1 (P1-P4, A1-A4, H1-H4 + gate) against the post-ε0 tree,
replace epsilon.json content, push, arm. Deploy-2 at ε1 quiescence.
migrate-plans: unit-exit-labels alreadyLabeled=2599 rewritten=0; capture-labels alreadyLabeled=2599 renamed=0 (pre-arm A1 precondition, recorded)
Finding-2 progress (narrator template fallback): the shim's claude call
works headless AND under a scrubbed unit-like env (both probes returned
OK). Fallback therefore originates inside the seam — scrape/jq extraction
or the driver's commitlint-shaped validator refusing the proposal. Next
probe: read the publish-node capture of ε1's first merge live.
Finding-2 CLOSED: the steward seam works end to end; proposals are refused
by the deterministic validator on format — header over the 72-char cap
(type(scope): prefix + Sonnet's 60-char subject budget) and unwrapped
bodies past 100 columns. Two rejections spend the slot, template fallback
proceeds, reasons recorded in the merged commit body (excellent
observability). Remedy = dotfiles narrator-shim hardening (fold -s -w 100
the body; prompt the model with the real ~48-char subject budget) — rides
deploy-2. ε1 first three merges: campaign-nix-surface-retire,
delete-gh-inbound-core (the ~10k-LOC wave centerpiece, merged inside 75
minutes), brief-carries-conflict-domains.

## ε1 NIGHT FINDING — H1 refusals explained the "no-commit deaths"

Every mid-wave attempt that "died without committing" (rowversion x3, the
variant-box fix lane) was in fact an AGENT HONORING ITS OWNERSHIP BOUNDARY:
H1 (brief-carries-conflict-domains) merged as the wave's third task, and
from that moment lanes could see their write boundary — when completing a
task required an out-of-domain edit, agents left valid in-domain work
uncommitted and said so, rather than committing a boundary violation.
F22's fix proved itself within hours of merging. The friction to polish in
ε2: a boundary refusal surfaces as a failed attempt + projection timeout
instead of a first-class "needs-grant" signal; the machine's gate
diagnoses named the missing grants anyway (18-for-18 tonight, including
one grammar-gagged but fully legible redaction). Ownership corrections
granted mid-campaign via worklist amendment + re-arm: daemon/tests.rs to
rowversion, producer_query.rs to variant-box (agent-requested verbatim).
Chapter-gate cycles: clippy large_enum_variant (gate-only lint class), fix
riding amendment task producers-config-variant-box.
D73 FLAW (found by the ε1 close): re-using one campaign identity across
stages collides on the durable summary refs — ε0's summary/complete on
origin made the ε1 driver refuse to reconcile ("local campaign summary
disagrees with this outcome"), killing sweep nodes with projection
timeouts. Remedy executed: archived the stage 0 refs to
summary/archive/eps0-* and deleted the canonical names, then resumed.
STANDING OPERATOR STEP going forward (and Part 7 note for ε2): at each
stage close, archive the summary refs before re-arming the next stage.

## DEPLOY-2 REGRESSION AND ROLLBACK (2026-08-14 ~02:50Z)

Deploy-2 (pin b4e655c8, generation 126) crash-looped the daemon:
`unknown field ghOrigin` refusing the durable task database. The ε1
census counted source:"gh" EVENTS (zero) but the pre-deletion writer
stamped explicit-null ghOrigin/ghTriggerActor/ghSelfActor FIELDS on
every row — 4,272 event files carry them. delete-gh-origin-durable's
field deletion turned deny-unknown-fields against history: a D33
violation the in-lane and gate suites could not see (they test fresh
bytes, never the operator's durable estate — a NEW finding class:
estate-bytes coverage gap; ε2 note: the rebuild verb R4 should replay a
REAL estate sample).

Rolled back to generation 125 (6a7c841a) — daemon active, estate
healthy, quiescent. nixos-rebuild --rollback is broken on this flake
host (NIX_PATH legacy path); the working route is
`nix-env --profile /nix/var/nix/profiles/system --switch-generation N`
+ `switch-to-configuration switch`.

Forward fix dispatched to a Codex worker
(2026-08-14-ghorigin-decode-tolerance): accept-and-discard the three
legacy fields as a named D33 legacy arm beside the EnqueueSource::Gh
decode arm, regression fixture from two real captured rows, strictness
otherwise unchanged. On green fleet-gate: merge, re-bump deploy2
worktree, rebuild, re-switch.
DEPLOY-2 COMPLETE (retry, 2026-08-14 ~03:35Z): pin 40957154 (= b4e655c8
stage 1 + the D33 decode-tolerance fix, PR #605, fleet-gate PASS),
generation switched, daemon active against the full historical estate,
quiescent, pools GO. Stage 1 hardening now live. Deploy branch commit
60afa885 (amended) remains local like Tom's b2c61c0f — both ride PR #225
whenever Tom merges it. Next: author ε2 against origin/main 40957154.

## ε2 CLOSE + F44 (2026-08-14 ~13:30Z)

ε2 COMPLETE: 19 lanes + gate (one F33 clippy cycle on the schema example,
repaired in-campaign as schema-example-stderr-lint). Gate checkpoint ref
chapter-gate-20ca5b97…/8b452838; published as rebase a8077295; disarmed;
BOTH summary refs archived post-disarm (eps2-*, F38 ordering applied).

F44 — THE SELF-HOSTING BOUNDARY: the first `campaign release --plan`
(run from a fresh ε2 build; the release window requires an ARMED
registration, so the identity was re-armed --no-enqueue after my
premature disarm, and the integration ref restored under the new
registration id) failed the trailer oracle: expected sha256:c1a6a166…,
merged trailer sha256:b68c64f9…, with task+campaign bytes verified
identical and campaign_contract.rs untouched in ε2 — the PYTHON driver
wrote every trailer, the RUST verb verifies them, and their canonical
bytes disagree. A release verb's first run always faces its predecessor
generation's proofs. Fix dispatched to Codex
(2026-08-14-release-trailer-bridge): secondary bridge oracle accepting a
unique task-trailer commit proven by the campaign's durable completion
ref, labeled per-task in the plan, never firing when exact match exists.
On green: merge, rebuild, release --plan → probe (test/release-probe.sh
with TALLY_BIN) → execute → final disarm → epsilon closed.

## EPSILON — COMPLETE (2026-08-14 ~10:50Z). THE LADDER IS CLOSED.

The self-release executed: GitHub release `0.0.0+20260814092311.8b45283`
("epsilon"), tag + notes + artifacts, published by the `campaign release`
verb epsilon built, from durable local state, through the operator's
ambient gh — D49 self-hosting achieved. The trailer-bridge fix (F44,
PR #606, fleet-gate PASS, e921cccc) proved all 19 completion proofs via
the bridge oracle, correctly labeled. The real probe
(mecattaf/tally-probe-20260814-6bf9bac2) reported releaseComplete:true;
its teardown needs the delete_repo gh scope (operator: `gh auth refresh
-h github.com -s delete_repo`, then delete the probe repo). Final
disarm done, summary refs archived (eps2-final-*), registry quiescent
and empty.

Operator items outstanding: (1) delete_repo scope + probe repo cleanup;
(2) dotfiles PR #225 (deploy commits b2c61c0f/60afa885 local; main still
pins 78dd4871); (3) narrator shim hardening (F32: fix the JSON envelope
first); (4) the AUG14-LEARNINGS.md "Decisions waiting for you" list.
Release binary used for the release acts: fresh build of e921cccc (the
deployed pin 40957154 predates the release verb; a deploy-3 to the
released tree is the operator's call, unhurried — nothing armed).

## CORRECTIONS (2026-08-14 afternoon, from the three-agent excavation)

Three claims above are wrong; the excavation reports (process archaeology,
verified-defect ledger, ceremony audit — session scratchpad, feeding
EPSILON-EXTENSION.md) proved them against the tree and the remote. Corrected
here so the record is not inherited wrong.

1. **F44's cause is NOT Python-vs-Rust canonicalization.** The deleted Python
   driver's trailer formula is byte-identical to the Rust driver's writer
   (`crates/spec-build-driver/src/actions.rs:1199-1221`); the divergence is
   writer-versus-release-verb — two different tuples, both live in Rust at
   `e921cccc` (`campaign_contract.rs:722-753` hashes campaign/mergeMethod/
   agent/steward/gates/content; the writer hashes repository/source/task).
   `ReleaseCompletionOracle::Exact` is unreachable for any file-worklist
   campaign in any generation; every future release bridges 100% of its
   proofs. "A release verb's first run always faces its predecessor
   generation's proofs" was the mis-attribution. VD-8; fix planned in ext2.

2. **The claimed final archive (`eps2-final-*`) did not happen as written.**
   `summary/complete` is still live at its canonical name beside
   `summary/archive/eps2-complete` for the same source (one more archive
   application would have failed the release closed on "multiple archived
   complete summaries" — PA-17), and the canonical closing summary's 19 task
   locators all dangle (they were re-rendered under registration `019fffba`,
   whose namespace holds only the hand-created integration branch — PA-18).
   Left as-is deliberately: the epsilon release is executed and recorded, and
   the ext0 task `summary-ref-stage-digest` retires this namespace defect.

3. **The released revision `8b45283` is not an ancestor of `origin/main`**
   (PA-21). It lives on the integration branch; main's equivalent is
   `a8077295`, and the whole difference is the campaign's own authority file
   (F31). The publish-with-re-gate verb (ext1) closes the class.
