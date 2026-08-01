# July 31 learnings — the first codegen campaign

On 2026-07-31 tally drove code-writing for the first time: the crm build
campaign (a Go CLI, spec frozen in `mecattaf/crm`, codex as the implementing
agent). The run failed on its first agent node, and the failure plus the shape
we had built around it taught more than a success would have. This file is
the record. Upstream work items minted from it: #232, #233, #234, #235.

## What we built (and why it was the wrong shape)

The campaign serialized the repo's `tasks.md` into 19 GitHub issues, wired a
gh producer with an at-mention trigger per issue, and had a supervising agent
puppeteer the sequence: mention issue N, watch the flow, review, merge, close,
mention N+1. Three defects, in increasing order of importance:

1. **Ceremony scaled with campaigns, not with work.** One campaign needed a
   bespoke producer block, a dispatch wrapper, two flow scripts, a pool, an
   issue-minting pass, and three fleet redeploys — none of it reusable as-is
   for the next repo.
2. **A supervisor in the dispatch loop defeats the purpose.** The goal is:
   start a run, return hours later to a finished artifact or a precise
   blocker. An agent posting per-issue mentions and merging by hand is
   orchestration outside tally — unwitnessed, unreplayable, and redundant
   with what flows already do. Merge-on-green needs no judgment: the
   witnessed gates are the merge criterion.
3. **GitHub became the work source.** The decomposition, ordering, and
   acceptance criteria already lived in the spec repo (`tasks.md`); minting
   them into issues made GitHub the task database and turned sequencing into
   GitHub round-trips. The work-graph's home moved away from the artifact
   that defined it.

## The role split that survives

- **The spec repo is the work source.** A frozen spec-kit-shaped corpus
  (constitution, spec, data model, plan, tasks, style-transfer references)
  carries everything an implementing agent needs. The campaign flow's first
  node *witnesses* the task list out of the repo — the cookbook's
  bounded-fan-out-over-a-witnessed-worklist pattern — and every later node
  derives from that witnessed result.
- **GitHub is intake, steering, and projection — never the work source.**
  Intake: one labeled campaign issue per repo; an explicit at-mention on it
  is the doorbell that starts the whole run (this is exactly what the gh
  producer is designed for; the mistake was 19 doorbells). Steering: humans
  comment on the campaign issue; agents read comments at task boundaries —
  the sanctioned way to redirect a failed attempt before replay. Projection:
  receipts, evidence comments, per-task PRs, progress updates — a view of
  witnessed work, driven by it, never driving it.
- **tally is the workflow engine.** spec-kit ships its own orchestration
  (`specify` workflows); we deliberately do not use it. Flows were designed
  to cover the same graph — spec → plan → tasks → implement — with
  admission, pools, witnesses, and replay that spec-kit does not have.

The target mechanism, one generic flow (`spec-build`): witness the worklist
from the repo → per task, strictly ordered: worktree from *current* main →
agent implements (scope = the task; steering = campaign-issue comments) →
deterministic gate nodes (per-repo commands, passed as args) → push + PR →
merge on green — then the next task's prep sees the merged result. Fail-fast
on the first red gate; replay reuses the witnessed prefix, so a stopped or
budget-exhausted run continues where it died (#234).

**Superseded on 2026-08-01 by #253:** campaign continuation is now a fresh,
bounded reconcile pass over marked merged pull requests, not replay of one
campaign-long runner. Dependency-ready tasks with disjoint declared conflict
domains may implement concurrently; successful publications integrate in a
deterministic sequence and any base-changing rebase is gated again before
merge. Replay remains the right mechanism for flows that genuinely require one
run identity, but not for forge-backed campaigns whose completion facts already
live in merged PRs.

### Where the work graph lives, and what each agent actually reads

A fair objection to "the spec repo is the work source": doesn't that force
every agent tally dispatches to read the entire project spec, where curated
issues gave each one a selected context? The objection names a real failure
mode but attributes it to the wrong layer. Three layers, separated:

- **The work graph is data.** Decomposition, ordering, dependencies, and
  acceptance criteria live in the spec repo's tasks artifact. It is authored
  once, at spec-freeze time, where the human effort belongs.
- **The flow is the interpreter, not the graph.** The generic `spec-build`
  flow knows nothing about any project. Its first node witnesses the tasks
  artifact into a schema-validated JSON worklist; every later node is
  derived from that witnessed result. Same flow for every campaign; only the
  data varies.
- **Each agent receives a per-task brief, never the corpus.** The worklist
  node projects the artifact into one brief per task — goal, delivered
  behaviors, read-first pointers into specific spec sections,
  style-transfer reference files, runnable acceptance criteria — and the
  agent node carries only its own brief through the structured brief
  transport (`TALLY_BRIEF`). The agent's context is its brief plus exactly
  the files the brief cites.

"Read tasks.md and do T05" would indeed be lazy context engineering — a
whole task file plus the spec in every context window. But the curated
issues never solved that problem; they inherited its solution. The issue
bodies were generated from tasks.md sections nearly 1:1 — the curation was
always authored in the tasks artifact, and GitHub was a copy of it. Brief
projection keeps the identical per-task selection while adding what issue
bodies never had: schema validation at the worklist boundary, witnessed
provenance, and no GitHub round-trip on the dispatch path. GitHub keeps the
roles it is good at — the doorbell (intake mention), the window (receipts,
evidence, PRs, progress), the margin notes (steering comments) — and stops
being the blueprint.

Separation of concerns, ruled 2026-07-31: the mechanism is completed and
validated **inside tally.nix** — generic flow + `services.tally.campaigns`
module (#235) proven against a fixture spec repo by this repo's own checks —
and consumer campaigns (crm first) restart on the finished mechanism. The
tool's roadmap never gates on a consumer project's schedule, and no consumer
runs on a throwaway estate prototype.

## The incident: first real agent node, dead in 89 ms

Task `019fb725-fc88-7fa1-86d3-6f94b6211eed`, adapter `codex`, the first codex
job this daemon ever executed. Worktree prep had passed; the codex node
failed with exit 1, 0.176 s witness span, zero stdout. The captured stderr
(`~/.local/state/tally/capture/`) had the whole story:

```
Reading additional input from stdin...
Not inside a trusted directory and --skip-git-repo-check was not specified.
```

The transient unit had `WorkingDirectory=` empty. The flow dialect excludes
raw `cwd` on purpose ("flows use structured workspace metadata instead") —
but nothing derives the executor cwd from `workspace.worktreePath`, so every
fresh agent node launches homeless. codex refuses to start outside a trusted
git directory; the byte-identical invocation run by hand inside the worktree
succeeds. The CLI even documents the intended semantics (`--cwd` "defaults to
the supplied workspace worktree") — the flow path silently diverged from the
documented contract. Fix: #232.

## Why verification missed it

Everything we checked was green: generation flake checks, `tally flow check`
on both flows, the producer unit polling, and
`tally producer test … --no-enqueue` answering `would-enqueue` with the exact
dispatch argv. All of it true — and all of it, by construction, stops short
of launching the adapter binary. `--no-enqueue` validates intake and
narrowing; flow check validates the dialect; neither executes anything.
`tally query jobs --pool codex-window` would have revealed that the codex
path had zero executions in its history.

**Doctrine: a pipeline is "verified live" only when each adapter on its
critical path has executed at least once on the target daemon.** Intake
diagnostics prove the doorbell, not the worker. #233 (`tally adapter smoke`)
makes that one-real-execution check a first-class verb that surfaces the
captured stderr — the incident's root cause was invisible in the journal and
lifecycle stream, and only present in the capture files.

Follow-up #249 closes that diagnostic gap locally: every failed lifecycle
event carries a bounded stderr tail. Raw adapter chatter is retained as
`.adapter.err`; the failure-only `.err` projection is absent on healthy jobs.
Forge publication is a separate trust boundary: campaign failure receipts are
off by default and require explicit `postFailureEvidence` plus
`postFailureStderr`; any published tail is conservatively redacted.

## Secondary observations worth keeping

- **Stream captures were the diagnosis.** At incident time, journal + lifecycle
  gave verdict and timing while only `capture/<task>.err` held the actionable
  error. Current operator reflex: read the lifecycle `stderrTail` first, then
  the retained failure capture when the tail is insufficient.
- **The producer behaved exactly as designed** — poll, narrow, receipt,
  dispatch with correct placeholder expansion, idempotent event identity.
  Intake needed zero fixes. The at-mention mechanism is keeper
  infrastructure; only its multiplicity was wrong.
- **Sequencing is a merge-ordering property, not a trigger property.** A
  dependent task's worktree must be cut only after its dependencies are
  observed merged. #253 refined the original serial conclusion: independent,
  conflict-disjoint tasks may prepare against the same base, provided their
  integrations are serialized and a head rebased onto moved main is gated
  again before merge. The dependency and conflict-domain tests are the
  enforcement, not human pacing.
- **Gates are the merge criterion.** Build, vet, race-enabled test, lint as
  witnessed nodes, then merge mechanically. An agent re-reviewing green
  gates before merge re-introduces the puppeteer. Spot-checking merged
  diffs is a steering input for later tasks, not a merge gate.
- **Frozen spec first, then assignment is cheap.** The expensive,
  handholding-heavy phase was producing the frozen corpus (scope rulings,
  spec, plan, task decomposition). Once frozen, assignment is mechanical —
  which is exactly why the assignment machinery deserves to be generic
  (#235) and the human effort should keep going where it is irreplaceable:
  the spec.

## Work items

| Issue | What | Status gate |
|---|---|---|
| #232 | Derive executor cwd from `workspace.worktreePath` | Blocks everything; first |
| #233 | `tally adapter smoke <name>` — close the verified-live gap | After #232 lands, smoke codex on the estate |
| #234 | Replay-as-continuation across the 24 h wall-clock budget: docs + test | Before campaigns are documented as arbitrarily long |
| #235 | `services.tally.campaigns` module + generic spec-build flow | Self-contained: fixture-driven checks in this repo; depends on #232 only |

Order of operations: #232 → #233 → #234/#235 land in this repository, each
proven by its own tests; then the crm campaign restarts as the first
consumer of the finished mechanism.
