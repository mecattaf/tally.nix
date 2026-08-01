---
name: assign-tally
description: Hand an entire multi-task buildout to tally as one autonomous campaign — prepare the frozen work graph, verify every executable claim live, ring the doorbell once, then observe forge ground truth without steering. Use when the user says assign to tally, tally campaign, overnight buildout, spec-build run, or wants a repo built from a frozen spec while they sleep. Derived from steer-codex: tally is that contract with a witnessed backend. WIP — lives in tally.nix until stable, then graduates to dotfiles.
---

# Assign to tally without becoming its supervisor

## Operating contract

Tally owns the campaign from first mention to closed master issue. Claude is the
control plane before and after, never during: prepare the ground, prove it live,
ring the doorbell once, observe, and intervene only through the failure protocol.

Claude never writes product code, never merges, never re-reviews what witnessed
gates already merged, and never hand-patches tally mid-campaign — a mechanism bug
gets a tally.nix issue and (if blocking) a dispatched worker, then the campaign
resumes. The frozen spec is read-only for Claude and for the agents.

The prime directive inherited from the estate's verified-live doctrine: **nothing
is proven until its exact argv has executed on the exact host.** Every campaign
failure to date came from an executable claim (gate command, adapter policy,
trigger identity) that fixtures and flake checks could not see.

## Rule of residue

Code first. This skill may only contain rules the mechanism cannot yet enforce.
Every such rule below carries a `DEBT:` marker naming the mechanism change that
should absorb it. When that change ships, delete the rule here. A growing skill
is a failing mechanism; the ideal version of this file is the operating contract
above and nothing else.

## Prepare (the freeze ritual)

The campaign artifact is **data, not code**: a worklist of tasks with stable ids,
per-task briefs (goal, behaviors, read-first pointers, acceptance, dependencies),
and the gate argvs. Never author per-campaign orchestration scripts; the generic
spec-build flow is the only executor.

- Dependencies are a DAG, not a sequence. Encode real edges; do not linearize.
  Ordering constraints (e.g. "import runs last") must be dependency edges, never
  prose in a prompt. DEBT: frontier scheduling — until it ships, execution is
  serial in worklist order, so the topological order must also be a valid plan.
- Declare which files each task owns when tasks could run concurrently.
  DEBT: conflict-domain field in the worklist schema, required when parallel.
- Standing invariants ("no .db/-wal/-shm in any PR") must be gates — a file-glob
  gate costs five lines. Never leave an invariant as a post-merge human audit.
- Before arming, execute on the target host, in a real checkout: `tally adapter
  smoke` for every adapter on the critical path, **and every gate argv
  verbatim**, including with a representative dirty tree. A gate that has never
  run is a gate that fails at 2am. DEBT: campaign preflight verb (#248).
- The mention token is the operator's own GitHub handle, never a third-party
  name. DEBT: default + validation (#246).

## Launch

One master issue carries the campaign: the mention is posted there once, exactly
as configured. Per-task issues are public anchors that PRs close — never
triggers. One comment starts everything; a second comment mints a second run,
so never stack mentions.

Ad-hoc campaigns must not require a fleet deploy to tune: config changes that
force redeploy-plus-fresh-run are a weight-class error for one-shot buildouts.
DEBT: forge-native campaign container (config + DAG readable from the master
issue; briefs as sub-issue bodies) and forge-state re-entrancy, after which a
re-mention is always safe and always cheap.

## Observe

Monitor **ground truth only**: merged PRs, non-empty capture `.err` files, and
the runner unit's liveness. Do not build monitors on `tally query log
--flow-run` — it freezes silently on long runs (DEBT: #247). Silence is not
success: any watcher must fire on every terminal state, including "runner gone
with work remaining."

Known adapter noise such as "Reading additional input from stdin..." is retained
in `.adapter.err`. The conventional `.err` path is materialized only after a
failed terminal verdict, so it is a valid failure signal.

A healthy campaign gets zero intervention. Do not comment, do not steer, do not
"check in" on the agents. Wall-clock alone is never a reason to interfere.

## Failure protocol

The campaign must keep working unless it is genuinely blocked or done. There are
no approval pauses, no "phase done, awaiting operator" states, and Claude must
never introduce one.

1. On any failed node, read the bounded `stderrTail` in `tally query log` or the
   campaign failure receipt first. Read
   `~/.local/state/tally/capture/<task-uuid>.err` only when the tail is
   truncated or insufficient; `.adapter.err` is the raw adapter stream.
2. Transient (network, quota, wall-clock budget) → re-trigger. Until forge-state
   re-entrancy ships, know the replay rules: config or tally changes void the
   witnessed prefix (args/script hash) and require a fresh mention; never retry
   a dead runner.
3. Agent fell short → one precise, evidence-based steering comment on the master
   issue, then re-trigger. State the missing outcome, not an implementation.
4. Two failures on the same task with good steering, a spec contradiction, or a
   mechanism smell → stop, write the diagnosis, file the tally.nix issue, hand
   to the operator. This is the only escalation in the protocol.
5. Escalate only at quiescence: a blocked task blocks its dependents, nothing
   else. DEBT: frontier scheduling makes this structural; until then a failed
   node kills the serial run and step 2/3 applies.

## Do not inject

Unless the frozen spec itself requires them, never impose: human checkpoints or
approval gates; a supervisor in the merge path; per-campaign orchestration code;
extra validation ceremonies beyond the declared gates; commit/PR rituals; token,
turn, or wall-clock budgets beyond the configured ones; or any constraint on how
the coding agent implements its brief. Tally's witnessed gates are the entire
acceptance contract.

## After

Close the loop from forge facts, not agent prose: all tasks merged, per-task
issues closed by their PRs, master issue closed with a summary comment (counts,
wall-clock, failures, steering, mechanism lessons). File every mechanism
observation as an atomic tally.nix issue the same day. Write the campaign
post-mortem where the estate keeps lineage; local commit suffices.
