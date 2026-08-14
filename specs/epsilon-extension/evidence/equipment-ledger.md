# August 14 — the equipment ledger: what it means to equip a tally run so that
# no mid-run judgment is ever needed

Written 2026-08-14 against the epsilon record (armed 2026-08-13 15:23Z, release
executed 2026-08-14 10:49:29Z, `main` at `e921cccc`). Findings prefix **EQ-**.
Sources read in full: `process-archaeology.md` (PA-01..47), `verified-defects.md`
(VD-1..31), `ceremony-audit.md` (CA-1..14), `AUG13-RUN.md` (incl. CORRECTIONS),
`AUG14-LEARNINGS.md` (F27–F43), `AUGUST-12-LEARNINGS.md` (F18–F26), all 34
attempt receipts verbatim, both steering logs verbatim,
`silent-factory-worklists/epsilon.json` at `e921cccc` and its 12 amendment
commits, `skills/assign-tally/SKILL.md`, `skills/campaign-operator/SKILL.md`.

**The question.** The operator's standing directive: the supervisor should be an
intern; asynchronous smarter-model steering is not wanted; the flow and worklist
must be expressive enough from the start. Therefore every mid-run intervention in
the record is read as a defect in the pre-run equipment, and this document names
the exact pre-run artifact that would have prevented each one.

**Classes** used throughout:

| class | meaning |
|---|---|
| **A** | authoring-census step — knowable by inspecting the observed tree before arming |
| **B** | template/doctrine text — standing guidance the brief, worklist template, or skill should always carry |
| **C** | schema expressiveness — the worklist/flow cannot currently express it; the missing construct is named |
| **D** | machinery defect — no equipment could prevent it; the tool must change |
| **E** | genuinely unforeseeable — the escape hatch earns its keep |

**The one-paragraph verdict.** Of epsilon's ~40 reactive operator acts, the
equipment classes divide cleanly: **~24 retire under class A** (they were
answerable by grepping the observed tree, rehearsing an argv, or reading the gh
auth scopes before arming), **~8 under class B** (standing sentences the brief or
skill should always carry — including the gate-set template, whose omission of
clippy was re-committed *after* clippy's absence had already been priced at 74
minutes), **2 under class C** (a campaign-level standing-discipline field and a
structured `needs-grant` outcome — and only those two; everything else that looks
like schema is really text), **~10 under class D** (races, the summary-ref
namespace, the release/disarm trap, the missing publish verb — already itemized
in the sibling reports), and **exactly 1 under class E** (the D77 design pivot,
which is an operator scope change, not a run demand). On this record, a
well-scoped and well-equipped epsilon runs with **zero ownership corrections,
zero gate cycles, zero steers, and at most 4 of its 10 pardons** — and every
surviving act is either a fixed-order ceremony an intern can execute from a
checklist or a design ruling the operator initiates.

---

## 1. THE LEDGER

Grouped by intervention type. Cross-references avoid double-counting: a pardon
that exists only because of a grant is retired by whatever retires the grant.

### 1.1 The twelve worklist amendment commits

The record of what `epsilon.json` lacked at each arm, in chronological order.

| commit | what happened | what the operator supplied | the pre-run artifact that would have made it unnecessary | class |
|---|---|---|---|---|
| `1953bb49` (ε0 authoring) | stage 0 authored | 4 tasks + gate, shakedown design | nothing — this *is* equipment being authored; F42's staged-authoring-against-the-observed-tree doctrine is the run's best idea and is deliberate | **B** (keep as doctrine) |
| `6a7c841a` (policy section) | epsilon gains its `campaign` section post-D77 | hand edit + push | none available: the arm surface itself changed mid-run (D77 deleted `local_campaign_declaration_from_document`). Downstream of the E-class ruling in §1.6 | **D** (arm-surface churn), downstream of **E** |
| `19bd53af` (`gate-local-audit`) | ε0 chapter gate failed 2 attempts on the **predicted** defect — `fleet-gate.sh` queries GitHub for a local unpublished HEAD (F28; receipts 1–2: *"No source fix is indicated"*) | new amendment task + re-arm + pardon 4 | **checkpoint-argv rehearsal**: run the chapter-gate argv verbatim in a pristine worktree against a local un-pushed head before arming. F13b already recorded that *"checkpoint argvs have no rehearsal at all"* — and `AUG13-RUN.md` calls this failure "the PREDICTED defect" (ε0 finding 3). A defect predicted at authoring time and armed anyway is an authoring miss by definition; the repair task should have been in `1953bb49` | **A** |
| `3f3f8525` (ε1 authoring) | stage 1 authored | deletion-wave design | deliberate, as `1953bb49` | **B** |
| `663de5bc` (grant: `daemon/tests.rs` → rowversion) | lane refused/died: a restart-stability test loads the fixture the task deletes (receipt 8 names it exactly) | read diagnosis → edit → commit → push → re-arm | **deletion-consumer grep**: for every file/fixture the task deletes, `grep -rn` the tree for its name (`legacy-no-origin.enqueue.json` → `crates/tally-core/src/daemon/tests.rs`, one hit). Every hit's file enters `conflictDomains` or a prerequisite task | **A** |
| `482ff524` (`producers-config-variant-box`) | ε1 gate failed 2 attempts on `clippy::large_enum_variant` — a lint class **no lane gate runs** (F33) | new amendment task + re-arm | **gate-set template**: `cargo clippy --workspace --all-targets -- -D warnings` as a per-lane gate. One JSON line; D77 made it a worklist commit. The deleting lane owned `crates/tally-core` wholesale and would have boxed the variant in-lane, in its own worktree, with zero worklist changes | **B** |
| `1324eaa4` (grant: `producer_query.rs` → variant-box) | the amendment lane refused across its boundary, naming `producer_query.rs:283` verbatim | grant "adopted verbatim" | same as `482ff524` — with clippy in the lane set the amendment task never exists, so neither does its grant. (Standalone census answer: enumerate every constructor site of a variant being boxed — `grep -rn 'Calendar('` — but the B fix subsumes it) | **B** (via gate template) |
| `c848d491` (`final-bar-stage1-reseat`) | ε1 gate failed 2 attempts: **12 of 24 final-bar cases** asserted pre-deletion contracts (receipt 16 enumerates all four repairs) | new amendment task + re-arm + pardons 18/19 | **deletion-consumer grep extended to the final bar**: the cases call CLI surfaces (`enqueue` → `queue enqueue`) and fixtures (`services.tally.campaignForge`) that ε1 deletes; `grep -rn` of `test/final-bar/cases/` for each deleted surface finds all 12 before arming. The reseat belongs in `3f3f8525` as authored ACs of the deleting lanes | **A** |
| `4309acc1` (ε2 authoring) | stage 2 authored — **without clippy in `campaign.gates`, ~1–2 hours after clippy's absence cost the 74-minute ε1 cycle** | build-wave design | see §2 — this is the sharpest scoping failure of the run | **B** (the authoring itself is doctrine; the gate omission is the B defect) |
| `ef0443f8` (grant: `Cargo.toml`/`Cargo.lock` → 5 port lanes) | `port-fold-half` publish failed: *"adding `tally-core` regenerated `Cargo.lock` outside the `crates/spec-build-driver` write boundary"* (receipt 25); F40 calls the class textually unfindable | cohort grant, one commit | **toolchain side-effect rehearsal + standing rule**: F40's own conclusion — *"dependency additions update the lockfile by construction"* — is a standing sentence: *any lane whose crate manifest may change owns the workspace `Cargo.toml` and `Cargo.lock`*. It needs no lint and no schema; the final `epsilon.json` proves it is expressible today (five lanes carry both files). One `cargo add` rehearsal in a scratch worktree demonstrates it in 60 seconds | **B** (rule) / **A** (rehearsal proves it) |
| `05aec25d` (grant: `crates/tally`, `crates/tally-flow`, `nix/lib` → delete-python) | lane refused with the **complete** missing set (receipt 29: 5 files across 3 unowned trees) | grant "per its own boundary refusal" | **deletion-consumer grep**: every one of the five files references the Python driver **literally by path or name** — `nix/lib/spec-build-driver.nix`, `nix/lib/campaign-drivers.nix` package it; `campaign.rs`, `flow_live.rs`, `spec_build_failed_agent_gate.rs` invoke it. `grep -rn 'spec_build_driver\|drivers/' --include='*.rs' --include='*.nix'` finds all five before arming. Unlike the `gitAi` case (semantic consumers, F22 §2), these are textual | **A** |
| `aa9f6213` (`schema-example-stderr-lint`) | ε2 gate failed 2 attempts on clippy: 5 stderr macros in `generate-flow-args-schema.rs` — a file **written by ε2's own lane** (`argsschema-single-source`) | new amendment task + re-arm + pardon 34 | **gate-set template only** — no census can see a file that does not exist yet. With clippy as a lane gate, the authoring lane fails its own gate and fixes it in-lane. This row is the proof that the gate template retires what the census cannot | **B** |

**Amendment scorecard: 3 deliberate (staged authoring, keep), 1 downstream of
the E-class ruling, 4 class A, 4 class B. Zero class E.** Not one grant or
amendment task required information unavailable before its stage armed.

### 1.2 The ten pardons

Receipt sequence numbers from `attempt-receipts-v1.jsonl`; reasons quoted in
PA-05/CA-2 and verified verbatim.

| seq | reason (gist) | retiring artifact | class |
|---|---|---|---|
| 4 | re-armed graph added dependency to escalated `chapter-gate` | transitively: the argv rehearsal (§1.1 `19bd53af`) removes the episode. Residual: this is *"literally R11's auto-pardon condition"* (PA-05) and the machine still demanded a human — tool debt | A (transitive), residual **D** |
| 7 | pardon after the commit-first steer | §1.3 steer 2's artifacts (A + B + C) | A/B/C (transitive) |
| 10 | woke the resting frontier (F23 shape; fix in this campaign, pin predates it) | none pre-run: the fix (`poll-liveness-arm`) was **cargo of this very campaign** and could not precede itself. Self-hosting tax; F41 confirms zero wake-pardons once deployed | **D** |
| 15 | stale-pass race: in-flight pass held the pre-grant snapshot | transitively: no grant (§1.1 `1324eaa4`) ⇒ no race. Residual: F37's race itself is tool debt | B (transitive), residual **D** |
| 18 | dispatched the gate's final run after the final-bar reseat | transitively retired by §1.1 `c848d491` | A (transitive) |
| 19 | pardon-race: pass snapshotted before the pardon landed | transitively retired with 18; residual race is tool debt | A (transitive), residual **D** |
| 20 | cleared stage-0 summary refs colliding with stage 1 (*"a D73 single-identity flaw for the record"*) | **no census, no template, no brief retires this under the current tool** (VD-4: the namespace is `sha256(campaign‖issue)[..24]`, no stage, no digest). The one *authoring-level* dodge: three stage worklists under three campaign names — which surrenders D73's single receipt ledger. Honest classification: machinery | **D** (authoring dodge exists, priced) |
| 27 | burned attempts predate the lockfile grant | transitively retired by §1.1 `ef0443f8` | B (transitive) |
| 30 | burned attempts predate the consumer-set grant | transitively retired by §1.1 `05aec25d` | A (transitive) |
| 34 | burned attempts predate the schema-example lint fix | transitively retired by §1.1 `aa9f6213` | B (transitive) |

**6 of 10 pardons retire with the authoring equipment; 4 are machinery (10, 20,
and the residual races behind 15/19).** The residuals are exactly CA-2's ask:
widen `amendment_pardon_plan` (`campaign.rs:4134`) beyond `dependencies`, and
stamp receipts with `armSerial`/`worklistSha` (CA-3) so "predates the amendment"
becomes a `<` comparison. Those are tool changes, not equipment.

### 1.3 The two steers

| steer | content | retiring artifact | class |
|---|---|---|---|
| `019ffbb8` seq 1 (`final-bar-local-forge-repair`) | scope ruling: *"delete the dead `--allow-test-local-forge` arguments rather than reintroducing any compatibility flag… changes outside test/final-bar are not [in scope]"* | the breakage was **known before arming** — `AUG13-RUN.md` (ch2 close): *"Known pre-existing breakage riding to ε0: four `test/final-bar` call sites still pass the deleted `--allow-test-local-forge`, and no gate covers final-bar."* A known breakage becomes an authored task **with the scoping ruling written into its `goal`** at authoring time. (CA-10 notes this steer was partly a deliberate verb shakedown — fine; but the *content* was available at authoring) | **A** |
| `019ffc34` seq 1 (`squash-rowversion-ladder`) | *"Commit FIRST, verify second… run the taskdb suite with its output tamed — `cargo test -p tally-core taskdb 2>&1 \| tail -30` — the flood is the likely killer of the two prior sessions"* | three artifacts, decomposed: (1) **A — acceptance-argv output audit**: run each AC argv once during authoring; any argv emitting more than a few KB gets `2>&1 \| tail -30` written into the worklist argv itself (the machine's own diagnoses already prescribe this shape — receipt 9). The flood was measurable before arming. (2) **B — brief doctrine**: *commit, then verify, then amend* (F43 ask 1 asks for exactly this). (3) **C — the carrier**: see §3; the steer's insight was **lost at the ε1→ε2 boundary** (PA-04: steering is registration-scoped, ε2's log is empty) even though ε2 contains the same suite | **A + B + C** |

The verb itself is the one honest human channel (CA-10) and stays. But on this
record, both steers' *content* was derivable at authoring time. A run that
needed zero steers was available.

### 1.4 The eight escalation episodes (16 diagnoses, F36)

Each cost 2 burned attempts + an escalation latch; each maps to a §1.1 row.

| ep | task / cause | retired by | class |
|---|---|---|---|
| 1 | `chapter-gate` ε0 — forge-native gate vs local head | checkpoint-argv rehearsal (`19bd53af` row) | **A** |
| 2 | `squash-rowversion-ladder` — sessions died between patch and commit (flooding suite) | AC output audit + commit-first brief text (§1.3 steer 2) | **A + B** |
| 3 | `squash-rowversion-ladder` — `daemon/tests.rs` loads the deleted fixture | deletion-consumer grep (`663de5bc` row) | **A** |
| 4 | `chapter-gate` ε1 — clippy `large_enum_variant` | gate-set template (`482ff524` row) | **B** |
| 5 | `producers-config-variant-box` — `producer_query.rs` out of bounds | same (task never exists) | **B** |
| 6 | `chapter-gate` ε1 — 12/24 final-bar cases stale | final-bar consumer grep (`c848d491` row) | **A** |
| 7 | `port-fold-half` — `Cargo.lock` regenerated outside the grant | lockfile cohort rule (`ef0443f8` row) | **B/A** |
| 8 | `delete-python-driver` — 5-file consumer set unowned | deletion-consumer grep (`05aec25d` row) | **A** |

**8 of 8 episodes retire. Zero escalations were unforeseeable.** The separate
fact that a refusal and a crash are the same signal (F35) stays as the flow's
class-C gap (§3) — it made episodes 2/3 and 7/8 cost a *second* attempt before
the real cause surfaced.

### 1.5 The four hand-run fleet gates

| run | context | retiring artifact | class |
|---|---|---|---|
| D77 run 1 (14:26:46Z, FAIL after ~17 min on `CHANGELOG.md exists but no open pull request…`) | out-of-band repair, gate hand-run | **partially D**: the changelog stage is a 200 ms metadata predicate evaluated *last* (PA-09 — ceremony that reversed the cheap-fails-first rule) and forge-native on a local-first tool (PA-36). Ordering fix + local-audit arm are tool changes. The *existence* of a hand-run gate follows from the out-of-band repair itself — see §1.6 | **D** |
| D77 run 2 (14:44:52Z, PASS) | ditto | ditto | **D** |
| ghorigin run (01:07Z) | repair 2's gate | retired transitively if repair 2 never happens (§1.6 → estate census) | **A** (transitive) |
| bridge run (10:30Z) | repair 3's gate | retired transitively if repair 3 never happens (§1.6 → seam AC) | **A/B** (transitive) |

Plus the 3 PRs (#604–#606) opened **solely** as gate fodder (PA-36) — same
classes as their gates.

### 1.6 The three out-of-band repairs (~45 min each, PA-10)

| repair | cause | retiring artifact | class |
|---|---|---|---|
| `2026-08-13-arm-self-contained` (D77, PR #604) | operator ruling *"remove that roundabout way"* — the prepared `services.tally.campaigns.epsilon` dotfiles mechanism was rejected and deleted | **none — this is the run's one honest class E.** A design pivot the operator initiated, judged by F27 *"the single most consequential decision of the run."* Scope changes the operator chooses are not mid-run judgment the run *demanded* | **E** |
| `2026-08-14-ghorigin-decode-tolerance` (F39, PR #605) | deploy-2 crash-looped the daemon: `unknown field ghOrigin` against 4,272 historical event files. The ε1 census counted `source:"gh"` **events** (0 of 3,859); the writer had stamped explicit-null ghOrigin **fields** on every row | **estate-bytes census**: before any task deletes a serde field on a `deny_unknown_fields` struct, grep the operator's real durable estate for the field name — `grep -rl ghOrigin ~/.local/state/tally \| wc -l` → 4,272, in seconds, before arming. F39 ask 2 states the rule: *"count the fields the writer emitted, not the values the reader cares about."* Plus the B rule (F39 ask 3): such a task carries a named accept-and-discard arm **as a delivered behavior with an estate-fixture AC**, not as a follow-up | **A** (census) + **B** (rule) |
| `2026-08-14-release-trailer-bridge` (F44, PR #606) | first `release --plan` failed the trailer oracle; real cause (VD-8, corrected in `AUG13-RUN.md` CORRECTIONS): writer and release verb hash **two different tuples**, both in Rust | **seam AC on a real artifact**: by ε2's authoring, 18 merged commits with real `Tally-Revision:` trailers existed from ε0/ε1. One acceptance criterion on `release-plan-render` or `adversarial-release` — *compute the completion revision for one real merged commit via the driver's writer and via the release verb's verifier; assert byte equality* — fails at lane time, inside the campaign, weeks before the close. This is the standing doctrine `AUGUST-12-LEARNINGS.md` already carried (*"A seam test is worth more than the four unit test suites that passed"*) and PA-35 counts as the week's 6th unapplied instance; VD-14's rule makes it general: *every delivered behaviour needs an acceptance criterion that fails without it* | **A** (executable here) / **B** (the rule) |

### 1.7 The close ceremony (~14 acts, 86 minutes, PA Part B)

| act | retiring artifact | class |
|---|---|---|
| premature disarm before `release --plan` | **close-ceremony checklist in `campaign-operator/SKILL.md`**, ordered: `quiescent` → `release --plan` → probe → `release` → `disarm`, with one sentence: *release requires the armed registration; disarm is the last act, after release.* The current skill (read at head) orders Release before Abandon but never states the dependency; the ε0 shakedown ledger's *"disarm is the operator's terminal act"* actively taught the trap (PA-16) | **B** (residual **D**: the trap itself — VD-16 — remains until release reads durable state) |
| re-arm `--no-enqueue` under new registration `019fffba` | transitively retired by the B line above | B (transitive) |
| integration-ref hand-restore (`git branch`, reflog 12:04:36 CEST) | ditto — the ref only vanished because the re-arm rotated the registration id (PA-22) | B (transitive), residual **D** |
| checkpoint-ref restore (census row 14 — *"appears in no record at all"*) | ditto | B (transitive), residual **D** |
| F44 chain: brief, worker, PR #606, hand gate, merge, rebuild (~40 min) | §1.6 seam AC | **A/B** (transitive) |
| probe `teardownComplete: false`, HTTP 403; orphan repo `mecattaf/tally-probe-20260814-6bf9bac2` still live | **gh-scope preflight at authoring**: a worklist containing release/probe tasks gets a preflight line — `gh auth status` must show `delete_repo` — run before arming, in milliseconds, before any real repository exists (PA-20 item 2: D75 *"never specified which scopes ambient auth must carry"*) | **A** (residual **D**: the probe should refuse at its own preflight and not conflate verdicts) |
| final disarm + claimed `eps2-final-*` archive that silently did not take (PA-17) | the checklist above **ends at disarm** — the final archive is pure ceremony whose next application would have failed the release closed (*"multiple archived complete summaries"*). Equipment here is a *shorter* list, not a longer one | **B** (deletion of a step) |
| 3 rebase-publishes (`6fdf108f`, `b4e655c8`, `a8077295`; `shas.txt` + `bodies.txt` + `@@@END@@@`) | **D — no publish verb** (PA-08, CA-8). Honest note: with zero mid-stage amendments (§1.1 retired), `main` never moves mid-stage, the integration branch never diverges, and the publish degenerates to a fast-forward an intern types — but the verb gap and the ungated-published-sha hole (CA-8) are tool debt | **D** (mitigated to trivial by A/B) |
| summary-ref archives at stage closes (6 writes + 6 deletes, botched at 2 of 3 boundaries) | **D** — VD-4's namespace carries no stage/digest; CA-7's verdict (digest in the ref name, delete the ritual) is the fix. Authoring dodge (per-stage identities) priced in §1.2 seq 20 | **D** |

### 1.8 Monitoring and deploy

| act | retiring artifact | class |
|---|---|---|
| supervisor rebuilt by hand at least twice; epsilon residue a 0-byte `jobs.json` (PA-11) | **committed supervisor fixture**: the stall predicate (`reg=1 && jobs=0` sustained 720 s) was *proven* at 16:11:25 CEST on Aug 12 — before epsilon armed — and then written into plan prose (§7.6) instead of into a script. Equipment = the proven predicate as a committed script in `test/` (or the repo's tooling dir), armed alongside the campaign. Prose playbooks get re-implemented; fixtures get executed | **B** (fixture; residual **D**: no block-until-condition verb) |
| gen 125 → 126 (crash-loop) → 125 (rollback) → 127 | crash-loop retired by the estate census (§1.6). The rollback scramble — `nixos-rebuild --rollback` broken on this host, working route discovered live at 02:50Z while the fleet was down — retires with a **rollback rehearsal preflight**: any run that will deploy rehearses one generation switch-and-back before arming, and the runbook line (`nix-env --profile /nix/var/nix/profiles/system --switch-generation N` + `switch-to-configuration switch`) is committed, not remembered. `AUGUST-11-OVERNIGHT.md`'s runbook said "rollback is one generation" and was wrong when it mattered | **A** (census) + **B** (rehearsed runbook) |
| deploy-skip drop-in stamping (inherited, retired mid-run by D63) | already fixed; the four-day gap between fix-specified and fix-live is the self-hosting tax (PA-39) | **D** (historical) |

---

## 2. THE SCOPING POST-MORTEM

The operator says some failures were poor scoping. Graded against the record:

### 2.1 What the ε2 census got right — and what kind of census it was

F42: the census counted the real driver at 7,492 LOC / 17 actions, the suite at
84 tests, the flow at 3,022 LOC / 56 `additionalProperties` sites — *"all five
figures reproduce exactly against `b4e655c8`."* Ownership corrections fell from
26% of tasks (ch0–ch2) to 11% (epsilon) to 2-of-18 for ε2 alone. The bet paid,
and the doctrine (author each stage against the observed tree) is the single
best scoping invention of the week.

But look at what the five figures are: **sizes**. LOC, action counts, test
counts, schema-site counts. It was a *magnitude* census — "how big is the thing
we port" — and it never asked the *edge* question: "who references the thing we
delete, and what does the toolchain regenerate when we build." The two
corrections it missed are both edge questions:

- the **lockfile** (`ef0443f8`): one `cargo add` rehearsal, or one standing
  sentence, answers it;
- the **cross-tree packaging refs** (`05aec25d`): `grep -rn 'spec_build_driver'`
  answers it — all five files reference the driver literally.

F42 called these *"the two things a census cannot see."* That is wrong, and the
distinction matters for epsilon-extension: **a census of magnitudes cannot see
them; a census of edges sees both in under a minute.** The same blindness, one
level down, is F39: the ε1 census counted event *values* (`source:"gh"` = 0) and
never grepped for the *field bytes* the writer had stamped on 4,272 rows. Three
misses, one shape: measured the object, never enumerated its edges.

### 2.2 ε1's near-serial wave under `maxParallel 3`

`AUG14-LEARNINGS.md` operational notes: *"`maxParallel 3` is honest for ε2 and
was dishonest for ε1. ε1's deletion wave is near-serial by domain overlap
regardless of the setting."* The overlap graph is computable from the worklist
alone at authoring time — pairwise `conflictDomains` intersection, widest
antichain of ready tasks. This cost no intervention (so it earns no equipment
item of its own — anti-ceremony), but it cost *estimation honesty*, and it is
the class of confusion the supervisor rebuilds chased ("free slot beside pending
tasks is normal, not a stall"). One computed number in the authoring output —
**effective width** — makes `maxParallel` a derived fact instead of a hope.
Fold it into the census, not into a new artifact.

### 2.3 The gate set: the run's sharpest scoping failure

Timeline, from the record:

1. Aug 13, 22:06→23:20Z: ε1's chapter gate burns **74 minutes** on
   `clippy::large_enum_variant` — a class F33 names precisely: *"gate-only lint
   classes"* — plus an amendment task, a grant, and a re-arm.
2. D77 is already live: *"changing a gate is a worklist commit, never a
   deploy"* (F27). Adding clippy costs one JSON line.
3. Aug 14, ~00:30–02:00Z: ε2 is authored (`4309acc1`) — the very next authoring
   act — and its `campaign.gates` is still exactly three entries: `driver-suite`,
   `cargo-tests`, `flake-eval`. `grep -n clippy epsilon.json` hits only inside
   the amendment task the class later caused (VD-6).
4. Aug 14, ~06:5x–08:0xZ: ε2's gate burns its cycle on clippy again (receipts
   31–34), in a file ε2's own lane wrote.

The author held the priced finding, held the one-line mechanism, and re-committed
the omission. Same shape as `flake-eval`: F21 ask 1 requested *"a cheap `nix
flake check` **subset** as a lane gate — the non-VM attributes alone would have
caught both"*; what shipped in the epsilon gate set was `--no-build` **eval**,
which VD-7 shows structurally cannot see the class that failed five of five
chapter gates (all were check derivations failing at *build* time). The gate set
is the highest-leverage authoring artifact in the system (VD-20: cap is 16,
epsilon used 3) and it had no template.

### 2.4 The complete authoring census — executable form

The list of questions an author answers against the OBSERVED tree before arming,
such that on this record zero ownership corrections and zero gate cycles occur.
Each step names its command shape and what it retired.

1. **Deletion/rename consumer grep.** For every file, fixture, symbol, CLI
   surface, or serde field a task deletes or renames:
   `grep -rn '<name-and-path-variants>' -- crates/ nix/ test/ examples/ flake.nix`.
   Every hit enters that task's `conflictDomains` or a prerequisite task; where
   the machine has previously enumerated a set, *take the machine's list
   verbatim* (F22 §2). — retired `663de5bc`, `05aec25d`, `c848d491`, episodes
   3, 6, 8, pardons 18/19/30.
2. **Assertion-inversion sweep.** For every behavior a task *changes*, grep the
   flake-only suites, VM tests, and `test/final-bar/cases/` for assertions of
   the old behavior (the F21/F26 class). The decisive triage test from the ch2
   audit: does the suite assert symbols the task deletes (`grep -c <symbol>
   test/...`)?
3. **Estate-bytes grep.** For any durable-format or serde change:
   `grep -rl '<field>' ~/.local/state/tally | wc -l`. Count **fields the writer
   emitted, never values the reader cares about**. Nonzero ⇒ the task carries an
   accept-and-discard arm as a delivered behavior with a real-sample fixture AC.
   — retired F39, the crash-loop, the rollback, PR #605, one hand gate.
4. **Toolchain side-effect rehearsal.** For each lane that may touch
   dependencies: one scratch-worktree build; any file the toolchain regenerates
   (`Cargo.lock`) enters the domains of **every** such lane at once (the
   `ef0443f8` cohort move, made pre-emptive). — retired `ef0443f8`, episode 7,
   pardon 27.
5. **Gate/checkpoint argv rehearsal.** Run every `campaign.gates` argv, every
   `preflightArgv`, and every checkpoint argv **verbatim** in a pristine
   worktree — including against a local un-pushed HEAD, which is the state the
   gate will actually meet in local mode. (F13b: checkpoint argvs had no
   rehearsal; the ε0 failure was *predicted* and armed anyway.) — retired
   episode 1, `19bd53af`, pardon 4, the ε0 gate cycle.
6. **Acceptance-argv output audit.** Run each AC argv once; any argv emitting
   more than a few KB gets `2>&1 | tail -30` written into the worklist argv
   itself. — retired episode 2, steer 2's trigger, pardon 7, two Codex session
   deaths.
7. **Seam ACs from real artifacts.** Every task that builds a verifier/reader
   of an existing writer gets an AC that runs **both sides on one real
   artifact** (a real merged trailer, a real estate row). Every delivered
   behavior gets an AC that fails without it (VD-14). — retired F44, PR #606,
   one hand gate, ~40 min of the close tail.
8. **Effective-width computation.** Pairwise domain-overlap; report the true
   parallel width beside `maxParallel`; estimate chains as chains (§2.2).
9. **Gate-set assembly from the priced template** (§3 below / equipment item 2)
   — not a census question but performed at the same sitting, against the same
   tree.
10. **External-authority preflight.** `gh auth status` scopes (incl.
    `delete_repo` when the worklist contains release/probe tasks); one rollback
    rehearsal when the run will deploy. — retired the probe 403 residue and the
    02:50Z rollback scramble.
11. **Known-breakage intake.** Anything already known broken at authoring time
    (the final-bar `--allow-test-local-forge` call sites) becomes a fully-scoped
    authored task with the scoping ruling in its `goal` — never a steer target.
    — retired steer 1.

Verification against the record: every grant (4/4), every amendment task (4/4),
every escalation episode (8/8), every gate cycle (3/3), and both steers map to a
step above. The claim "zero ownership corrections and zero gate cycles" holds on
this record. What the census does **not** retire: pardons 10 and 20, the races
behind 15/19, the summary-ref ritual, the publish rebases, the release/disarm
trap, and the narrator — all class D, all already itemized as tool asks in the
sibling reports.

---

## 3. CLASS-C HONESTY

Everything in the ledger that *looks* like missing schema, tested against "could
committed text express it today?" Only two survive.

**C-1 — campaign-level standing discipline, rendered into every brief.**
The `campaign` section is closed (name, maxTasks, maxParallel, mergeMethod,
runtimes, agent, steward, gates — F27) and carries no prose that reaches a lane.
Steer 2's content — commit-first, tame the flood — is *campaign-wide worker
discipline*, and today the only carriers are (a) per-task `goal` duplication
across 20 tasks, which decays and cannot be amended without touching every
task, or (b) a mid-run steer, which is registration-scoped and was **silently
discarded at the ε1→ε2 boundary** (PA-04: ε2's steering log is empty while ε2
contained the same flooding suite). Minimal construct: one optional field,
`campaign.discipline: [string]`, rendered verbatim into every task brief the way
`conflictDomains` now is (H1). It would have carried commit-first/tail-N from
arm time, and it gives future census output (step 6) a durable home. This is
the one place where B-text genuinely cannot do the job, because the defect is
the *carrier*, not the content.

**C-2 — a structured `needs-grant` outcome in the flow contract.**
`failureClass` (`examples/flows/spec-build.js:2101-2158`) has five classes and
none is a refusal (VD-1); the brief *instructs* refusal
(`conflictDomainsBoundary`, `:2184-2203`) and gives it no channel, so a refusal
is indistinguishable from a crash (F35) and costs a second burned attempt per
episode before the cause surfaces (episodes 2/3, 7/8). Minimal construct: a
sixth class `needs-grant`, produced when the final message parses as
`{outcome: "needs-grant", paths: [...]}`, priced like `deferred` (spends
nothing), surfaced beside `blocked`. Note honestly: with the §2.4 census, zero
refusals occur *on this record* — C-2 is the escape hatch for the census's
future misses, not a substitute for the census.

**Rejected as C (they are text or tool, not schema):**

- *Toolchain side-effect cohorts (lockfiles).* Expressible today: the final
  `epsilon.json` already lists `Cargo.toml`/`Cargo.lock` in five lanes'
  `conflictDomains`. One standing sentence in the authoring skill suffices. **B.**
- *Per-task ordering constraints.* `dependencies` already expresses ordering;
  steer 2's "commit first" is intra-attempt discipline → C-1's payload, not new
  structure.
- *Expected-weather declarations* ("this gate fails-then-passes"; "this suite
  floods"). The flood is fixed in the argv (A); fails-then-passes should
  *vanish* under the gate template rather than be declared normal — declaring
  it would codify the defect (the CA-7 mistake). Supervisor doctrine line at
  most. **B.**
- *Receipt authority stamping* (`armSerial`, `worklistSha`, timestamp — CA-3)
  and *summary-ref digests* (VD-4). Real, needed, and **D**: they are tool
  ledger schema, not authoring equipment; no author can supply them.

---

## 4. THE EQUIPMENT LIST

"To equip a tally run," as one ordered list. Each item names the recorded
interventions it retires; items that retire nothing were deleted (three
candidates were cut: an expected-weather field, a parallelism schema field, and
an archive-ceremony verb — the first two retire nothing, the third codifies a
defect).

1. **Run the edge census against the observed tree** (§2.4 steps 1–4, 8):
   deletion-consumer grep, assertion-inversion sweep, estate-bytes grep,
   toolchain rehearsal, effective-width. *Retires: all 4 grants, 1 amendment
   task, escalation episodes 3, 6, 7, 8, pardons 18/19/27/30, the F39
   fleet-down, the rollback scramble, PR #605, one hand-run gate, ~3 re-arms.*
2. **Assemble the gate set from the priced template**: per-lane
   `cargo clippy --workspace --all-targets -- -D warnings`; a **built** (not
   evaluated) non-VM flake-check subset naming its attributes; the final bar
   executed, not `--list`-ed; the driver suite; workspace tests; metadata
   predicates (changelog/PR) evaluated first, not last. One worklist commit
   under D77. *Retires: episodes 4, 5, amendments `482ff524`, `1324eaa4`,
   `aa9f6213`, grant `1324eaa4`, pardons 15/34, both clippy gate cycles
   (74 + ~70 min).*
3. **Rehearse every argv verbatim in a pristine worktree** — gates, preflights,
   and checkpoints, including against a local un-pushed HEAD. *Retires:
   episode 1, amendment `19bd53af`, pardon 4, the ε0 gate cycle.*
4. **Audit acceptance-argv output and write the taming into the argv**
   (`2>&1 | tail -30`). *Retires: episode 2, steer 2, pardon 7, two session
   deaths.*
5. **Author seam ACs from real artifacts** — verifier-vs-writer on one real
   trailer; delivered-behavior ACs that fail without the behavior (VD-14).
   *Retires: F44, PR #606, one hand-run gate, ~40 min of the 86-minute tail.*
6. **Write the standing brief text** (B): commit-then-verify-then-amend; take
   the machine's enumeration verbatim; the stale-pass-race expectation ("a
   rejection timestamped before the amendment commit is not a failure of the
   amendment"); the deployed-pin attribution rule ("diff the deployed store
   path before crediting or blaming any commit" — VD-13). *Retires: the
   mid-run re-derivation these cost in ch0–ε2; prevents the two recorded
   mis-attributions (F32-bang, F34-H1).*
7. **Intake known breakage as scoped tasks** with the ruling in the `goal`.
   *Retires: steer 1.*
8. **Preflight external authority**: `gh auth status` scopes (`delete_repo`
   for any release/probe worklist); one rollback rehearsal before a deploying
   run; commit the working rollback route. *Retires: the probe 403 + orphan
   repo, the 02:50Z live rollback discovery.*
9. **Commit the supervisor as a fixture**, not prose: the proven
   `reg=1 && jobs=0` stall predicate as a script in the repo, armed with the
   campaign. *Retires: both monitor rebuilds, the 0-byte `jobs.json`.*
10. **Write the close ceremony into `campaign-operator/SKILL.md` as an ordered
    checklist**: `quiescent` → `release --plan` → probe → `release` → `disarm`,
    and nothing after disarm. *Retires: the premature disarm, the re-arm, both
    hand ref-restores, the failed final archive, and the PA-17 near-miss —
    roughly half the 86-minute tail.*
11. **Add the two schema constructs and only those two** (C):
    `campaign.discipline` rendered into every brief (*retires: the cross-stage
    loss of steer 2's insight, PA-04*), and the `needs-grant` sixth
    failure class (*retires: the second burned attempt in every future
    boundary episode — the census's escape hatch*).

**What the list does not claim.** Items 1–11 retire every class A/B/C
intervention on the record — approximately 28 of the ~40 reactive acts. The
remaining ~10–12 are class D and belong to the tool, not the author: auto-pardon
width + receipt stamping (retires the residual race pardons), summary-ref
digests (retires the archive ritual and pardon 20), a publish verb with re-gate
(retires the rebase harness), and the release-reads-durable-state contract
(retires the trap items 10 only mitigates). Those are already ranked in the
sibling reports' asks. **The intern test therefore holds in two stages: with
items 1–11 alone, every remaining mid-run act on this record is either a
checklist command in a fixed order or a design ruling the operator initiates;
with the four D-class tool asks landed as well, the checklist itself shrinks to
arm, watch, release.**

**One caution, per the operator's own anti-ceremony rule.** Items 1–5 are a
sitting of a few hours against the observed tree; that is what "well-scoped"
costs and what it buys. Nothing in this list is a new standing process document:
ten of the eleven items are greps, rehearsals, one-line gate entries, sentences
in existing skills, and two small schema fields. The census dies with the
authoring sitting; only the templates and the two fields persist. That is the
difference between equipment and ceremony, and epsilon's record shows the
system punishing every violation of it in both directions.
