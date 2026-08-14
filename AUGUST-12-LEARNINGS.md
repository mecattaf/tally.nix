# August 12–13 learnings — the silent-factory ladder: four seams of one
# half-landed mechanism, and the ownership contract nobody wrote down

Context: the silent-factory ladder run as forge-native tally campaigns in
`mecattaf/tally.nix` — ch0 (#536, 10 tasks), ch1 (#556, 6 tasks), ch2 (#568, 18
tasks), starting 2026-08-12 afternoon on pin `13307c67` and running through the
night. **Result at the time of writing: ch0 and ch1 CLOSED with
`tally:campaign-complete:v1` receipts and green chapter gates; ch2 at 11 of 18
merged and running. 25 campaign PRs merged. Six out-of-band repairs. Fourteen
worklist-authority corrections. Eighteen operator resumes.**

The session opened on a deadlocked ladder: every implementation lane on #536 was
dying at `steering:recheck`, and the previous steward correctly stopped rather
than hand-edit tally source. This document is what the recovery and the two and
a half chapters after it taught.

**The headline is not the deadlock.** It is that **six of nine findings are the
same defect wearing different clothes: a task must own every file that its
change makes false, and the worklist schema cannot express or check that.**
Findings are numbered continuously with `AUG13-RUN.md` (F18–F26) so they stay
linkable.

## Score

| Layer | Verdict |
|---|---|
| tally core (admission, pools, lanes, gates, merge criterion, restart recovery) | sound — no defects observed in 3 chapters |
| tally diagnosis layer | **best component in the system.** Correct on every escalation, ~12 for 12 |
| flow engine JS→JSON boundary | 1 fleet-wide defect (F18), silently corrupted every integer above `i32::MAX` |
| repo-scoped campaign pools (D62) | landed in 4 layers, worked in 0 — see F19/F20 |
| campaign liveness | 1 defect (F23): a campaign can rest with dispatchable work and not wake |
| chapter-gate economics | structurally sound, but costs one full cycle per chapter (F14 confirmed twice) |
| **worklist ownership contract** | **the dominant cost of the entire run** |
| codex adapter (worker survival) | no adapter-level deaths observed across ~40 attempts |

## Headline measurements

- **3 campaigns, 34 tasks settled** (ch0 10/10, ch1 6/6, ch2 11/18 in flight).
- **6 out-of-band repairs**, all authored by workers under the full gate ladder,
  none hand-edited by the orchestrator: `8e6285d5`, `ef679342`, `78dd4871`,
  `2cc08bec`, `43f0a747`, `3ee84031`.
- **14 worklist-authority commits.** Nine of them were the ownership defect.
- **18 resumes** (5 on ch0, 5 on ch1, 8 on ch2). Every one was operator-driven;
  none could have been issued by the machinery itself.
- **Both chapter gates that have run failed once and passed on retry.** Both
  failures were stale assertions living only inside `nix flake check`.
- One chapter-gate repair, instructed to hunt behind the first failure, found
  **two more stale fixtures** that would each have cost another gate cycle.

---

## F18 — Boa holds integers as `i32`; every GitHub comment ID crosses the JSON boundary as a float

The deadlock. `value_to_json` (`crates/tally-flow/src/engine/interop.rs`) was a
pass-through to Boa's `JsValue::to_json`. Boa stores small integers as
`Integer(i32)` and everything larger as `Rational(f64)`, which serde_json emits
as `5266404097.0`. Every GitHub comment ID today is ~5.27 × 10⁹, past
`i32::MAX`. The driver's `steering_comment` requires `isinstance(id, int)`, so
**every prepared steering comment reached the driver as a float and every
implementation lane died.**

The shape is what makes this severe: a campaign runs perfectly until it speaks
once on its own master issue — and tally's own `campaign-summary` receipt is
usually the first comment. From that instant the campaign cannot steer, and
therefore cannot repair itself. The ladder's own repair lane was killed by the
defect it was dispatched to fix.

Fixed at `8e6285d5`: normalize integral `f64` back to JSON integers at the one
choke point, for values within ±2^53, recursively through arrays and objects.

Asks:
1. Keep a test pinning a real GitHub-comment-magnitude id (`5266404097`) across
   the boundary. The class of bug returns whenever a new host call is added.
2. Audit for other `to_json` call sites that bypass `value_to_json`
   (`natives.rs` had several near the diagnostics path).

## F19/F20 — D62's repo-scoped pools landed in four layers and worked in none

`campaign-pool-namespace` (ch0, `90f99d00`) introduced `campaign/OWNER/REPO`
runner mutexes, minted on demand by the lease layer, and the nix module was
taught to **refuse** configuring them (`nix/modules/common.nix`: "repository
campaign mutexes are minted on demand"). Every layer was updated except the ones
between:

| layer | verdict on `campaign/mecattaf/tally.nix` | |
|---|---|---|
| CLI `validate_campaign_runner_pool` | accepts | ✅ shipped |
| lease layer (mint on demand) | mints | ✅ shipped |
| daemon enqueue RPC | **`unknown pool`** | ❌ F19 |
| flow `meta.argsSchema` pool pattern | **regex rejects the `/`** | ❌ F20 |

So the merged mechanism was unreachable end to end: no campaign using the new
pool form could dispatch at all. ch0's own lanes never caught it because ch0 was
armed under the *old* pin with the *old* host-wide `campaign` pool — the tasks
that build a mechanism are never the tasks that exercise it.

Fixed at `78dd4871` (daemon admission for enqueue and named pause/resume, with
dynamic pools carrying no configured credentials) and `2cc08bec` (widen only the
manifest runner-pool schema, keeping host-configured pool fields on the plain
alphabet).

Asks:
1. **A seam test is worth more than the four unit test suites that passed.** One
   end-to-end case — arm a campaign whose manifest names a `campaign/OWNER/REPO`
   pool on a host that does not configure it, and assert it dispatches — would
   have caught both.
2. When a change reserves a namespace, grep every `contains_key(pool)` and
   pattern in the tree in the same task. There were exactly two more.

## F21 — flake-only assertions merge green; the chapter gate is the only thing that runs them (F14, now confirmed twice)

ch0's gate failed on `checks.x86_64-linux.system-socket-execution`
(`KeyError: 'observed'`) because `poll-event-quality` had deliberately replaced
fleet-wide counters with schema-versioned per-registration events and the VM
test still read the old key. ch1's gate failed on
`spec-build-conflict-domains` because `corpus-divergence-vectors` had
deliberately dropped casefolded dedup per D38 and the test still asserted the
rejection.

Both were *correct* behavior changes meeting *stale* assertions. Neither could
have been caught by a lane gate, because **no lane gate evaluates the flake.**

Two operational consequences worth internalizing:

- **"Chapter gate fails once, then passes" is the normal shape, not an alarm.**
  Budget one gate cycle per chapter.
- **`nix flake check` stops at the first failing attribute.** The ch1 repair was
  explicitly told to hunt for more behind the first and found two additional
  stale fixtures (`spec_build_two_repo_test.py`, a `flake.nix` expectation).
  Always instruct repairs to sweep; otherwise each hidden one costs a full
  gate cycle of its own.

Asks:
1. Consider a cheap `nix flake check` subset as a lane gate — the non-VM
   attributes alone would have caught both, at a fraction of the QEMU cost.
2. Failing that, make the chapter gate report **all** failing attributes rather
   than dying at the first (`nix flake check --keep-going`).

## F22 / F24 / F25 / F26 — one defect: a task must own every file its change makes false

This is the dominant finding of the run, and the reason ch2 took the wall-clock
it did. All four wear different clothes:

| | the file the task could not legally touch | why the lint could not find it |
|---|---|---|
| **F22** | a test named verbatim in the task's own `goal` | *findable* — textual |
| **F24** | `examples/flows/spec-build.js`, whose closed task schemas (`additionalProperties: false`) reject a field the task adds to the driver payload | coupling is semantic, named nowhere |
| **F25** | `crates/tally/tests/migrate_cli.rs`, which launches `tally migrate` with no `--config` and therefore loads the operator's **real deployed** `$HOME/.config/tally/config.json` — still carrying the `gitAi` key the task makes illegal | host-state leakage; only a self-hosted campaign can hit it |
| **F26** | `crates/tally/tests/flow_live.rs`, which asserts merges publish to `origin/main` — exactly what D14–D15 replace with a local integration branch | the test asserts the behaviour being deleted |

Nine of the fourteen authority corrections were this. Each cost two agent
attempts (~30–40 min) plus a project+resume cycle before the boundary was wide
enough for the lane to deliver.

**Two sharp sub-lessons:**

1. **The projected task brief does not carry `conflictDomains` at all.** The
   agent receives goal, delivered behaviours, read-first and acceptance
   criteria — and never learns what it is permitted to touch. It discovers the
   boundary only by being rejected at the ownership gate, after doing the work.
   This is the single cheapest thing to fix in the whole list.
2. **The machine's file enumeration beats the operator's grep.** For
   `remove-gate-b-and-contract` a `grep -ci gitai` found 3 consumers; the
   machine, having actually compiled the tree, enumerated **9** — the flow
   crate's in-`src` campaign tests, its failed-agent gate, `campaign_poll`,
   `flow_live`, the two-repo test and `test/final-bar`, none of which name
   `gitAi` textually. **Take the machine's list verbatim. Do not re-derive a
   narrower one.**

Asks:
1. **Ship the boundary into the brief.** Render `conflictDomains` in the task
   brief so the worker can honour it, and so it can *report* an unavoidable
   out-of-boundary edit as a first-class outcome instead of dying.
2. **Add an ownership preflight.** A cheap textual pass at `project`/`arm` time
   catches the F22 class (a scratch implementation found the ch1 defect plus two
   more unstarted instances). It cannot catch F24–F26, so it should warn, not
   gate.
3. **Make "expand my boundary" an in-band request.** Today the only repair path
   is: operator reads the diagnosis, edits committed worklist bytes, re-projects,
   resumes. The machine already knows the exact answer — it prints it — but has
   no verb to act on it. This is the largest single unattended-operation gap in
   the system.
4. Document the rule in the worklist authoring guidance: *for every assertion
   your change falsifies, declare its file.*

## F23 — a campaign can come to rest with dispatchable work and refuse to wake

ch1 settled to `idle`: 3 done, 1 **pending with no dependencies**, 2 blocked
behind it, **zero job units**. `tally campaign poll --once` returned
`status: unchanged` and dispatched nothing — the cheap forge-observation
precondition is stable precisely *because* nothing is running to move it. Only
an operator `resume` cleared it.

The precondition is sound as "has the forge moved" and unsound as a liveness
check. Deadlock is not permanent, but an unattended overnight ladder sits there
indefinitely.

Asks:
1. A liveness arm in the poll: *dispatchable work exists and no nodes are live*
   ⇒ dispatch, regardless of observation digest.
2. Or a terminal-pass invariant: a pass must not end leaving unblocked,
   unstarted work.
3. Until then, the supervision signal is **armed + master open + zero job
   units**. Watching for close or deregistration alone misses it; this shape
   cost the previous session ~2 hours and this one ~15 minutes because it was
   being watched for.

## Operational notes that are not defects but cost real time

- **`campaign resume` preserves the registration's recorded flow/driver store
  paths.** A resumed campaign keeps executing the *old* pin's flow forever; only
  a fresh `arm` re-resolves packaged paths. This is why the F20 flow fix required
  `disarm` → `arm --flow` → `resume` rather than a plain resume.
- **Freezing the flow is the right posture for a self-hosting ladder.** Every
  chapter was armed with `--flow <frozen copy at 2cc08bec>` while the pin stayed
  at `78dd4871`. This let ch1's `worklist-task-revision` and ch2's local-canon
  tasks rewrite `examples/flows/spec-build.js` in the repo without destabilising
  the machinery grading them. Part 6 §6.3's "ch1–ch5 changes ride the repo, not
  the running pin" is correct and should be stated as a hard rule.
- **Beware the stale diagnosis.** Twice, a diagnosis timestamped ~1 minute
  *before* an authority fix looked like a fresh failure of that fix. Always
  compare the comment's `createdAt` against the fix commit before concluding the
  correction failed.
- **`campaign status` has two blind spots.** Between passes it reports
  `0 done, 0 running, 0 blocked, 0 pending` with "No reconciled task table is
  available for this run"; and after completion it can replay stale history
  (it showed ch1 `needs-attention` for a campaign whose master was closed and
  whose registration had been pruned). Neither is wrong, both read as alarming.
- **`conflictDomains` overlap — not `maxParallel` — is the binding scheduling
  constraint.** Five of ch2's tasks own `drivers`; they can never run
  concurrently no matter what `maxParallel` says. A free slot alongside pending
  tasks is usually correct behaviour, not a stall. Chapters that rewrite one
  large file are inherently serial, and should be estimated as chains.
- **A checkpoint task has no worker (F16, confirmed).** Every chapter-gate
  failure needs an out-of-band fix and a resume. Combined with F14 this makes an
  operator mandatory at every chapter boundary.
- **The `silent-factory-worklists/` directory was missing from `tallySource`,**
  so a test added by ch0 (`checkpoint-brief-render`) passed under `cargo test`
  and failed the package's check phase under nix. Fixed at `ef679342`. Any test
  reading repo files needs its inputs in the package source filter.

## What the machinery got right (keep verbatim)

1. **The diagnosis layer is excellent and should be trusted first.** On every one
   of ~12 escalations it named the correct file and the correct fix, twice
   including the *wrong* fix to avoid ("do not restore remote pushes; that
   contradicts D14–D15"; "delete the test instead of restoring `forge: github`
   support"). It out-performed the operator's own analysis repeatedly.
2. **Bounded escalation held.** No loop ever burned more than two attempts before
   stopping and asking.
3. **`project --issue` is genuinely idempotent.** Re-projected ~10 times across
   three chapters; converged on identical sub-issue numbers every time and never
   disturbed merged tasks or prose outside its markers.
4. **`resume` is the right recovery workhorse** — pardons counters, re-approves
   the graph digest, dispatches, and posts an auditable receipt naming the
   reason. Eighteen of them, zero surprises.
5. **Gate ordering (cheap-fails-first) paid off** — most failures surfaced in
   `fmt`/`tests` long before the expensive stages.

## Decisions waiting for you

1. **Ship the boundary into the brief** (F22 §Ask 1). Cheapest fix, largest
   return; it converts a whole failure class into something the worker can
   handle itself.
2. **Decide whether the ownership contract becomes mechanism or doctrine.** A
   preflight lint catches only the textual third. The honest alternative is an
   in-band "request boundary expansion" verb, which would make campaigns
   genuinely unattended for the first time.
3. **Decide the flake-gate trade** (F21 §Ask 1): a non-VM `flake check` subset as
   a lane gate, versus continuing to pay one chapter-gate cycle per chapter.
4. **F23 liveness**: poll arm or terminal-pass invariant.
5. The pin is deliberately frozen at `78dd4871` and the nightly fleet-deploy is
   skipped through 2026-08-18 (`skip-ladder-through-2026-08-17.conf`). Both need
   unwinding when the ladder finishes; the end-state deploy remains yours.
