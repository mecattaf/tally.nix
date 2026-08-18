# August 10, midday session — tally-self-host: the first run where tally built tally

Third mission of the day, and the first whose workload was **this repository**.
Two campaigns on pin `8n1ihbds…` against `mecattaf/tally.nix` as spec repository,
code repository, and issue repository simultaneously:

- `tally-self-host-probe` (#472) — **complete, 1/1**, 16:5xZ → 17:07:36Z.
- `tally-self-host-ch1` (#473) — **7 of 8 settled**, armed 17:08:08Z, closed
  `outcome=quiescent` 21:22:23Z (4h14m). One task blocked: the acceptance
  checkpoint, on a true positive. Deliberately left escalated.

The two earlier missions of 2026-08-10 (crm-call-drain, dotfiles#163;
dcal-calendar, dotfiles#210) proved tally could build *something else*. Every
change to tally.nix itself, up to `089d000`, was made by improvised waves of
hand-launched agents. This run retires that practice. The operating rule was
absolute: **all worker code through campaign dispatch — zero manual terminal
starts, zero steer-codex fallback.** It held for the entire session. No product
code was written by the operator or by the supervising session.

The question the run exists to answer is **can tally carry its own
development**, and its answer gates the v0.0.1 ratchet.

## Score

| Layer | Verdict |
|---|---|
| tally core (admission, preflight, gates, publish, rebase, merge, completion) | 100% — 8 pull requests merged, zero false verdicts, zero manual starts |
| self-hosting: lane worktrees of a repo the daemon serves | works; one real race surfaced (#494), no containment failure |
| self-hosting: fleet-gate ladder as gate argv | viable — ~316s cold ladder, far cheaper than feared |
| pre-arm validation (the freeze ritual) | earned its keep three times before a single dispatch |
| tally steering/diagnosis layer | **1 of 5 diagnoses delivered** — up from dcal's 0 of 5, still the weak link (#455) |
| escalation lifecycle | correct: one true-positive escalation, bounded, with accumulated diagnoses |
| codex adapter | strong: 9 attempts for 8 tasks, one failure and it was a test race, not a session death |
| operator (me) | 1 self-inflicted incident: idled ~1h on a supervision question the handoff had already answered |

## Dispatch metrics

- **8 implementation tasks merged** (1 probe + 7 chapter 1), **+3,895 / −491 across 56 files**.
- **Zero manual terminal starts. Zero operator interventions** until the final escalation.
- 2 arms, **0 re-arms** — so the marker-walk tax (#459) was never paid this run.
- 9 worker attempts for 8 tasks. The one retry was `marker-walk-tax`, on a test race.
- 13 GitHub issues filed: #471, #481, #482–#491, #494.
- 1 escalation, 1 quiescence, both correct.

Per-task cadence, chapter 1:

| PR | Task | Merged | Diff | Elapsed |
|---|---|---|---|---|
| #476 | `driver-two-repo-baserev` (#471) | 17:37:44Z | +10/−2 | 29.6m |
| #477 | `steward-diagnosis-grammar` (#455) | 18:02:39Z | +134/−120 | 24.9m |
| #478 | `steward-forbidpaths-semantics` (#458) | 18:24:54Z | +145/−38 | 22.3m |
| #479 | `steering-prep-race` (#461) | 19:05:26Z | +720/−27 | 40.5m |
| #480 | `checkpoint-output-capture` (#457) | 19:46:16Z | +791/−61 | 40.8m |
| #492 | `marker-walk-tax` (#459) | 20:48:42Z | +864/−191 | 62.4m (one retry) |
| #493 | `quiescence-and-pardon` (#456) | 21:14:28Z | +1214/−48 | 25.8m |

## Pre-arm validation found three things before a single agent ran

The freeze ritual feels like ceremony until it pays. It paid three times, and two
of the three could not have been produced by reading.

### 1. `main` was red, and had been since #453

`nix flake check` — the stage `test/fleet-gate.sh` runs as this repository's merge
criterion — failed on `main` at `089d000`. `spec-build-two-repo` errored 2 of 19
tests: `bfc080a` added a `baseRev` read to `github_pull_request` (the marker-only
detection that produces the `[marker] ` title prefix) without updating the test
fixtures. The production path validates `baseRev` before that call, so it was
fixture drift, not a product defect.

**The deeper finding is how it got in.** The merge criterion was an *unenforced
self-report*: a worker pasting a fleet-gate transcript. A red flake check reaches
`main` whenever that transcript is not actually produced. Filed as #471, armed as
chapter 1's first task, merged as #476.

### 2. The acceptance checkpoint died in one second under the hardened tier

A bare `nix flake check -L` under `systemd-run --user` at the job hardening tier
(`ProtectHome=read-only`, `PrivateTmp`, `ProtectSystem=strict`, `NoNewPrivileges`,
`RestrictAddressFamilies=AF_UNIX`) fails immediately with `attempt to write a
readonly database` on `$HOME/.cache/nix/fetcher-cache-v4.sqlite`. The Nix
**client** needs a writable home cache.

Same shape as dcal's exit-127 Go-toolchain lesson; same cure — redirect
`XDG_CACHE_HOME` and `XDG_STATE_HOME` to the unit's private `/tmp` inside the
argv. Two sandbox iterations to green. Filed as #484.

*Generalization: any checkpoint argv invoking a tool with a `$HOME`-rooted cache
is a 2am failure waiting to happen, and the offender list is long — nix, go,
cargo, npm, pip, uv. A second trap sits beside it: `PrivateTmp` makes a rehearsal
lane staged under `/tmp` invisible, so a naive rehearsal passes for the wrong
reason.*

### 3. `[marker]` was already shipped on `main` but not in the pin

#459's cheapest tier was implemented by #453 and visible in the tree, while the
installed pin still lacked it. A brief written from the issue text alone would
have sent a worker to re-implement live code and burned an attempt. The brief was
corrected to scope tier 2 only.

**Rule: when the campaign's workload is the campaign's own mechanism, read the
tree, not the issue.** Issue bodies age against `main` at the speed the campaign
merges, and the pin is a third version behind both.

## Item-by-item

1. **The probe was not throwaway work.** Doctrine requires a one-task probe
   before the first arm on a new host, pin, or repository. Rather than invent
   filler, the probe *was* the smallest real task in the backlog (#460, a
   Rust-only error-message change). It merged as #475 with zero interventions and
   proved the whole self-hosted path — reconcile, preflight, agent, gates,
   publish, PR, merge — before chapter 1 armed. This is the pattern to keep.

2. **`project` retires the `:end` marker footgun entirely.** dcal's first
   incident was an arm rejected for closing manifest blocks by repeating the
   *begin* markers. That class disappears when the master body is produced by
   `tally campaign project`: it writes `tally:campaign:v1`/`:end` and
   `tally:campaign-worklist:v1`/`:end` correctly by construction. *Skill residue
   deletion: replace the `:end` rules with "never hand-author a master body."*
   One consequence to know: `project` **overwrites the title and body** of any
   issue it adopts. Originals were archived first, and each brief folds the
   motivating evidence back in.

3. **The gate ladder is cheap, and that answers the self-hosting cost question.**
   Cold detached lane at `main`, argv-verbatim: fmt 1s, no-stubs 0s, cargo-deny
   1s, `cargo test --workspace` **286s**, clippy **28s** — **~316s total**. Full
   `nix flake check` **100s**. The feared per-task cold-rebuild tax never
   materialized. Clippy is cheap because it runs after the tests gate in the same
   lane and inherits its artifacts, which is why gates are ordered
   cheap-fails-first, then the expensive build, then the inheritors. **Caveat that
   must travel with these numbers: warm Nix store and warm cargo registry. On a
   cold machine both would be dramatically worse.**

4. **The gate ladder was also too weak, and it cost the chapter.** It ran fmt,
   no-stubs, deny, tests, clippy — but **not `nix flake check`**, which I deferred
   to the end-of-chapter checkpoint to avoid paying it per task. That was a
   mispricing. `nixfmt-check` is a flake check, so a formatting violation
   introduced by #480 merged cleanly and was caught only at the checkpoint, four
   hours later. **+100s against a 316s ladder would have red-gated it at merge
   time.** Second time in one day the same gap produced a red `main` (#471 was the
   first). Filed as #488.

5. **The escalation was a true positive and the machinery handled it correctly.**
   `chapter1-acceptance` failed twice, the campaign accumulated diagnoses, posted
   one bounded escalation, and stopped — no looping, no false merge. The blocked
   task blocked nothing else because nothing depended on it.

6. **#455 reproduced live, on the pin, while its own fix sat merged on `main`.**
   Three of four steward diagnoses for the checkpoint were rejected: *"diagnosis
   omits the failing check id 'chapter1-acceptance'"*. Both machinery-retry slots
   burned on `diagnosis-contract` faults rather than on the work. The fourth
   passed only because it happened to contain that literal string. This is the
   sharpest possible demonstration of the defect: **the fix for it merged in this
   very chapter and could not help the run that produced it.**

7. **Every rejected diagnosis was substantively correct.** They named the check,
   the file and line, the originating commit `27302f6`, and the cure — including
   *"do not retry unchanged"* and *"add this as a dependency"*. #455's central
   claim holds exactly: **the knowledge exists, the pipe is clogged.**

8. **Machine steering delivered once — the run's positive control.** On
   `marker-walk-tax` attempt 1 the steward correctly identified a **concurrent Git
   worktree-metadata race** in tally's own test suite ("task 4 and task 6 prepared
   linked worktrees concurrently… the reported missing-file message is expected
   negative-test noise"), that diagnosis *passed* the grammar, published as
   steering, and attempt 2 succeeded. Filed as #494. It is also a self-hosting
   reflexive edge: a lane running `cargo test --workspace` creates git worktrees
   while the campaign concurrently manages lane worktrees off the same repository.

9. **A worker has no channel to report findings (#481).** Two briefs deliberately
   asked their worker to report a judgement in the pull request body. Neither
   answer exists anywhere: PR bodies are pure template, commit bodies are empty,
   task threads carry zero comments. Root cause, established from the campaign's
   own agent node rather than inferred: **the implementation node declares no
   captures at all** (`capture keys: []`), unlike `adapter smoke` which declares
   `["sessionRef","finalMessage"]`. The final message is never retained, so there
   is nothing to publish. A brief instructing "state X in the PR body" is silently
   unsatisfiable.

10. **Two observability defects specific to self-hosting.** `tally query run`
    labels a node `tally-self-host-ch1/<task>` while `tally query jobs` uses
    `<registrationId>/<task>` — the campaign name appears in neither taskRef, and
    grepping for it returns nothing (#482). I briefly concluded from this that
    agent nodes were unrecorded; they were not, and I was wrong. Separately, the
    documented campaign journal filter is **unsound when the workload is tally**:
    `cargo test --workspace` emits `TALLY_POOL=campaign-agent`,
    `TALLY_AGENT=codex`, `TALLY_EVENT=evidence_pass` from fixture tasks, making a
    lane journal-indistinguishable from the campaign dispatching it (#483). The
    suites *do* bind their own temp sockets and cannot reach the live daemon — a
    legibility defect, not a containment one.

## Rulings settled this session

- **Merge control: option B, no exceptions.** `main` changes only through a
  campaign merge; hand merges are not permitted, including by the operator. This
  supersedes the standing rejection of #128 item 5 in `CONTRIBUTING.md:146–151`.
  The reasoning behind that rejection — "agents could bypass a GitHub control" —
  is sound against *evasion* and does not address *omission*, which is the failure
  that actually occurred. The remedy chosen adds no control; it removes the
  unverified path. Recorded with verbatim replacement text in #489, which must
  itself be landed by a campaign.
  *Why no escape hatch is safe: a campaign executes the **pin**, not the tree. A
  broken driver on `main` cannot disarm the machinery. The only true deadlock is a
  broken pin, whose remedy is a generation rollback — a deploy, not a merge.*

- **Python: keep it, don't port.** Deliberated by a separate agent against the
  code. Three stages: recharter out of `examples/` (#485), single-source the
  contract corpus from Rust (#486), language-entry policy enforced by path and
  flake check (#487). A per-action Rust port is stage 3, conditional, triggers
  named. Key finding: **the JS/Python seam is load-bearing** — the Boa flow layer
  is deliberately effect-free, every side effect must be an out-of-process
  witnessed job, and porting the driver into JS would require giving the Boa host
  git/gh/filesystem verbs, exactly what #470 forbids. The accident is not that
  Python exists; it is that an 8,088-line production engine is filed under
  `examples/`. Also corrected: the canonicalization-skew class is **3** issues
  (#429, #444, #446), not 5 — and it already received its structural fix, which
  worked. What remains is the four-place contract maintenance tax, of which #471
  is the fixture leg.

- **Documentation is gated** behind a codebase-hardening program (#491), because
  #463–#466 are claims about behavior and `campaigns.md` has not been reconciled
  with the code.

## What held from the earlier missions, verbatim

Bounded escalation, forge-derived counters, the admission door, lane preservation,
and checkpoints-as-defect-finders all behaved as documented. The survival steering
baked into every brief (no shell `rm`, removal in Python, small patches, lane
commits per milestone) again produced **zero codex session deaths** across nine
attempts. The marker-walk tax was never paid because the run needed no re-arm.

## Open follow-ups

- **#474 is deliberately left escalated.** Recovery is graph surgery (an
  implementation task reformatting `flake.nix:~4590`), re-arm, then **always**
  `tally campaign resume` — re-arm does not pardon (#456, fixed on `main`, not in
  the pin). Expect the marker-walk tax on that re-arm: 7 completed tasks re-walked
  with live agent dispatches. Full analysis is a comment on #474.
- Filed and ready to assign: #481 (worker findings channel), #482 (task identity),
  #483 (journal filter), #484 (hardened-argv preflight), #485/#486/#487 (Python
  chain), #488 (gate ladder), #490 (revision-isolation claim), #491 (doc-as-oracle
  sweep), #494 (worktree race). #489 needs Tom's ruling landed by campaign.
- Skill residue drained into `skills/campaign-operator/SKILL.md` (new §0 never
  hand-author a master body, §3b `$HOME`-cache checkpoint rule, §8 read-the-tree-
  not-the-issue, journal-filter unsoundness) and `skills/assign-tally/SKILL.md`
  (smoke under the manifest's real policies, cold gate measurement, self-hosting
  section). Held uncommitted by ruling — working material, not merged.
- **Standing question still open:** does machine steering deliver once #455's fix
  is in the *pin*? This run moved it from 0-of-5 to 1-of-5 and cannot answer it,
  because it executed the pin carrying the bug. The next run on a deployed pin is
  the first that can.
- Unattended-readiness bar unchanged: a stormy-class campaign completing with zero
  operator interventions. This run came closer than any before it — seven merges,
  zero interventions — and stopped on a true positive rather than a machinery
  fault.

---

## Postscript: what happened after this session ended

Recorded 2026-08-11 from forge facts. The sections above describe the supervised
session and are left as written; several of their open items resolved within the
hour, and two resolved in ways that change earlier conclusions.

**Chapter 1 completed 9 of 9 at 22:08:04Z.** The recovery ran exactly as
prescribed on #474: implementation task #495 (`flake-fmt-capture-assert`) wrapped
the `hasInfix` assertion, merged as PR #496 (+7/−1) at 21:55:29Z; the campaign was
resumed at 21:45:21Z with counters pardoned; `campaign-complete` posted at
22:08:04Z. The escalation was recoverable by the book.

**The re-arm paid no marker-walk tax at all — and this resolves #490.** The graph
surgery added a task and rotated the digest, and the expectation from both dotfiles
missions was seven completed tasks re-walked with live agent dispatches. **Zero
marker pull requests were opened.** Between 21:30Z and 23:00Z exactly one pull
request exists (#496, the new task's own).

This matters more than the saved time. The re-arm ran on the **old** pin
`8n1ihbds…` — the deploy came later — so the absence is *not* #492's fix taking
effect. It is pre-existing shipped behavior, which means `campaigns.md`'s claim
that "adding or editing an unrelated task… leaves existing task revisions intact"
**was already true**, and #490's question is answered in the doc's favour. The
crm-call-drain and dcal-calendar marker-walk measurements must therefore have had
a different cause — most plausibly that those surgeries edited *existing* task
briefs or global execution policy, both of which legitimately do rotate every
revision. **The marker-walk tax is a consequence of editing existing tasks, not of
adding new ones**, and #459's tier-3 scope should be re-read in that light. A
prediction stated confidently in the sections above, and in two prior run reports,
was wrong.

**Chapters 2 and 3 completed clean.** `tally-self-host-ch2` (#497, the hardening
chapter — #481–#491 and #494) settled **14 of 14** at 04:01:54Z.
`tally-self-host-ch3` (#513, completing the doc-as-oracle census) settled **4 of
4** at 05:58:41Z. Neither master thread carries a single diagnosis, machinery
retry, escalation, or quiescence comment. Every issue this session filed — #481,
#482, #483, #484, #485, #486, #487, #488, #489, #490, #491, #494 — is closed.

**The standing question is still not answered, and the reason is a good one.**
Chapters 2 and 3 ran after the deploy, on a pin carrying #455's fix, so this was
the first opportunity to see whether machine steering delivers. It never fired:
nothing failed. That is a better outcome than a passing test of the steering path,
but it is not a test of it. The question — *does machine steering deliver once the
pin carries the fix* — carries forward unchanged to the first run that produces a
real failure on a fixed pin.

**Still open at the time of writing:** #518 (campaign preflight rehearsal of
argvs), #519 (campaign-scoped journal key), #520 (the two remaining contract
hand-copies), #521 and #522 (rule and complete the doc-as-oracle divergences), and
#523 — which carries forward this session's unanswered question about whether
merge control covers operator lineage documents, this file among them.
