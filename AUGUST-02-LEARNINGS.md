# August 2 learnings — the day the verification bill came due

Companion to `JULY31-LEARNINGS.md` and `AUGUST-01-DESIGN.md`. Those recorded what
the first codegen campaign taught and the target model that followed. This file
records what the *close-out* of that model taught: how the shape of the work
changed as the codebase approached feature-completeness, why the last stretch
felt slower than the middle, and what that implies for building software this
way at all. Written by the wave-3 day-steward session, 2026-08-02, while the
last five lanes were still in flight. Nothing here overrides an issue's text.

## 1. The day in numbers, as the evidence base

Five issue loops fully closed (#310–#314, plus #315 implemented and in gate),
strictly sequential by operator instruction. Every loop ran the complete cycle:
implement → fleet gate → merge → adversarial eval on post-merge main → repair →
second gate → merge → close. The evals found **22 defects behind green gates**
(4 HIGH, 11 MEDIUM, 7 LOW). All 22 were consumed the same day — folded into
repair PRs or routed to the owning sibling lane — and **zero new issues were
filed**. The board moved 30 → 21 open, monotonically.

Loop anatomy, measured: implement 30–60 min; fleet gate 15–40 min, paid twice
(initial head, repair head); eval 20–40 min; repair 25–45 min. Verification —
gates, evals, repair cycles — consumed **60–70 % of wall clock**. In the July
waves the same ratio was well under half. Nothing went wrong to cause this. The
ratio is the finding.

## 2. Zeno's treadmill is real, and it has a mechanism

The feeling that each step toward "done" takes twice as long is not perception
error. Two curves cross as a codebase matures:

**Implementation cost scales with the diff. Verification cost scales with the
system.** Early on, a merge is cheap to prove: there is little to break. Issue
#30's gate ran in minutes. By issue #348 the gate ladder carries NixOS VM
checks with 900-second socket budgets, `flow_live` suites at 2½ minutes per
test, four language toolchains, and a changelog/PR-association stage — because
every one of those stages exists to hold an invariant some earlier issue
established. The gate is the accumulated memory of everything the project has
ever promised. It can only grow.

**The invariant surface compounds.** Every merged concept becomes a constraint
on every future diff. By today, a lane touching campaigns must thread: the
51-node flow pin (four assertion sites), ownership unions and conflict domains,
redaction rules on a public forge, the §9.2 tombstones, three proof axes it
must not disturb, and contract mirrors in Nix, Python, Rust, and JS that must
agree byte-for-byte on shared schemas. None of this shows up in the issue's
acceptance bullets. All of it shows up in the eval.

So late-stage defects change species. Early defects are "the code doesn't do
what the issue says." Late defects are "the code does exactly what the issue
says, and violates a contract established four issues ago." The second kind is
invisible to the author, invisible to the unit tests the author writes, and
frequently invisible to the gate — which is why the adversarial eval, not the
gate, became the real merge criterion.

## 3. Where model-written code actually fails, late in a project

Today's 22 findings cluster into a taxonomy worth keeping. Almost none were
regressions of old code. Nearly all were defects in code *the same PR
introduced* — specifically in its guards, seams, and edges:

- **The guard that guards the wrong case.** The `Assisted-by:` forgery guard
  matched case-sensitively; git trailers match case-insensitively. Its sibling
  guard three lines away knew this (`(?i)`); the new one forgot. A forged
  lowercase trailer would have landed on public main.
- **Atomicity dissolved by refactor.** Lane identity moved from one
  atomically-written JSON marker to nine sequential `git config` calls. Each
  call was correct; the sequence was interruptible, and the "heal" path for the
  interrupted state cemented the lane instead of healing it.
- **Ordering that loses a race it cannot retry.** The squash receipt pushed
  non-force *before* the base push; a retried squash mints a different oid; the
  second attempt could never succeed. One lost race = permanent wedge.
- **The proof-destroying side effect.** `git notes merge -s cat_sort_uniq` on
  structured authorship notes rewrote the *local* notes ref — the publish step
  of the fourth proof axis silently destroyed the third axis's records, and the
  receipt still said `bound`.
- **The fixture that is wrong in the same direction.** A regression test
  advanced its base linearly; production advances it with merge commits (then,
  after the squash default: the reverse). The test passed by constructing a
  topology production never produces. This class recurs enough here to have a
  name in the eval checklist.
- **The silent fallback where a loud refusal belonged.** The steward seam read
  only `argv` and dropped `env`; a correctly-configured narrator would fail
  twice, fall back to a template, and never say so. The repair's insight:
  refusing what you cannot honour beats pretending, and *documenting* the gap
  while still silently degrading would have been the same defect with better
  prose.

The generalization: model-generated first drafts are **locally correct and
globally naive**. The model implements the acceptance bullets faithfully and
tests what it thought about. What it did not think about — the distant
invariant, the case-fold, the interleaving — is exactly what an independent
adversarial pass, run by a session that "did not write this code and owes its
author nothing," is structurally positioned to find. Every eval this wave found
real defects behind a green gate. That is not an indictment of the workers; it
is the discovered cost model of correctness at this maturity.

## 4. Technical debt is a routing decision, not a fate

Yesterday's run filed one residual issue per lane (~0.9/lane) — honest, but the
board grew and the operator felt it: "every issue creates three more." Today
the same class of findings produced zero issues, because routing changed:

1. **Repair-now by default.** Findings fold into the repair PR of the lane that
   caused them, scoped by an explicit orchestrator ruling appended to the
   findings file. The implementing session is resumed with full context — it is
   the cheapest possible fixer.
2. **Ownership routing.** When a finding's clean fix lives in another lane's
   files (the #313 pruner was inert until the executor stopped re-minting
   locks — executor code, owned by #314), it becomes a binding addendum to that
   lane's dispatch, not an issue and not a misplaced fix.
3. **Explicit waiver.** What is consciously not fixed is recorded in the ledger
   with reasons, by name.

The deeper point: **an issue tracker measures debt only if filing is the
routing of last resort.** Boards also lie upward — this morning's "27 open"
contained three merged-but-unclosed lanes and seven superseded ancestors
double-counted beside their successors. Twelve of twenty-seven rows were zero
remaining work. The honest debt metric is findings-per-eval over time (stable
at 4–8 here, not rising) and the severity trend (HIGHs repaired same-day,
residue all LOW/MED hygiene) — not the row count.

## 5. What the "entirely vibecoded" method actually bought, and what it cost

Zero human-written lines, ~350 issues/PRs in, and the system holds. What made
that possible is visible in retrospect:

- **The spec is the product.** Issue text + acceptance bullets became the
  binding contract for three independent parties: the implementer, the gate,
  and the evaluator. Every eval opens with "verify EVERY acceptance bullet by
  running commands, not by reading the diff." Ambiguous bullets would have
  collapsed the whole verification chain. The operator's contribution was never
  code; it was specification precision, ordering, invariants, and rulings — and
  that contribution is the load-bearing one.
- **Doctrine files compound.** `AUGUST-01-DESIGN.md` §9's tombstone list turns
  "the model proposed a dead design again" from a debate into a pointer. Frozen
  decisions (enqueue kernel, no JS modules, no mid-run human gates) cost one
  line to cite and save a session of relitigating.
- **What still requires the human:** posture decisions with blast radius
  (flipping git-ai to `required`), estate boundaries (dotfiles, deploys),
  supersede-semantics rulings, and taste calls about what is worth building at
  all. Every one of those surfaced today and was correctly refused by the
  machinery until the operator ruled.

The cost: the method converts engineering time into *verification* time. The
operator experiences this as slowness precisely when the product is nearly
done — see §2 — and the honest answer is that the slowness is the quality bill
being paid on schedule, visibly, instead of after deployment, invisibly.

## 6. Operational physics, confirmed again today

All of yesterday's harness rules held; two earned reinforcement:

- **Contention fails safe, so budget for it asymmetrically.** A loaded host can
  only turn a gate red, never green. Overlapping a gate with one model session
  is therefore a sound trade (cost: an occasional wasted gate cycle); running
  two compile-heavy model sessions is not (cost: a worker chasing a ghost
  defect it manufactured itself). On any red, the first question is "what else
  was running?" — before reading the diff.
- **Disk pressure impersonates code defects.** Below ~16 GiB the suite fails in
  ways that read as real bugs (`gh_requests = 6, expected 2`). Today's sweep of
  closed-lane `target/` dirs reclaimed 167 GB mid-run. Every long campaign
  needs a reclamation step in its loop, not in its post-mortem.

## 7. The speed levers, and the principle behind them

Adopted at operator request for the remaining lanes, ordered by trust:

1. **Batch by subsystem.** Seven residual hygiene issues collapse into two
   lanes (campaign residue; infra/adapter residue). One worker, one PR closing
   several issues, one scoped eval. Loop overhead is per-lane, not per-issue —
   so make lanes own coherent subsystems, not tracker rows.
2. **Overlap the fail-safe stage.** Gates may run concurrently with exactly one
   model session, because a false red is cheap and a false green is impossible.
   Never overlap two sessions: their failure mode is wasted *reasoning*, which
   is expensive and fails unsafe.
3. **Scale eval depth to blast radius.** Full adversarial evals for mechanism
   lanes (the two-repo seam; the pin move). Scoped diff-radius evals for
   hygiene lanes. The principle: verification budget goes where wrongness is
   expensive, not uniformly.

## 8. Transferable, beyond this repo

- Verification cost tracks system size; implementation cost tracks diff size.
  Plan for the crossover, and stop reading it as "we got slow."
- A green gate means "nothing I thought to check broke." Pay a party who did
  not write the code to think of the rest.
- New code's dangerous defects live in its *guards and seams*, not its happy
  path — review the guard for the property it guards (case, atomicity,
  ordering, side effects on neighbours).
- Debt is a routing decision: repair-now, route-to-owner, or waive-in-writing.
  Filing a ticket is the last resort, and the board is not the debt metric.
- Refuse loudly what you cannot honour; a silent fallback plus accurate
  documentation is still a silent fallback.
- Fixtures must construct the topology production produces — a test can be
  wrong in the same direction as the code and pass forever.
- Write rulings down where the next actor will trip over them (tombstones,
  ledgers, addenda). Doctrine that costs one pointer saves one relitigation.
