# Pools and leases

A pool is a named admission policy for a scarce logical resource. Jobs ask for
pool names; tally grants a lease only when every requested pool permits the job.
The lease, rather than a guessed property of the process, is what accounts for
contention.

`resource` classifies the policy as VRAM, build slot, CPU slot, budget, or
mutex. It does not discover hardware or allocate memory. `enforce` currently
accepts only cooperative enforcement. The operator declares the real resource
boundary and ensures that workloads honor it.

## One atomic pool set

A job may request several pools. tally canonicalizes that set and either grants
all of it or queues the job; there is no partial hold-and-wait state. This is
the reason a job that needs both a build slot and a GPU should name both on one
enqueue rather than acquire them piecemeal.

For the `co-residency` predicate, `capacity` is the maximum number of live
holders. A mutex is the strict special case: it must use co-residency with
capacity one. Other resource labels use the same counted-holder mechanism.

There is an uncomfortable shipped edge worth stating plainly. `budgetGb` is
type-checked and allowed only on a multi-holder co-resident VRAM pool, but the
lease engine never reads it and jobs carry no VRAM-size request. Current
admission counts holders only. Set `capacity` to the concurrency that is
actually safe; do not treat `budgetGb` as an enforced memory sum.

## Windowed consumption

A `windowed-consumption` predicate belongs to a budget pool. Admission requires
`consumptionEstimate`, refuses a request that would exceed the rolling cap, and
records the debit with the durable grant. An admission key prevents a replay
from charging the same attempt twice. On restart, tally reconstructs the
window from grant events and the verified witness ledger.

An external `usageMeter` may report observed utilization. Without one, tally
can derive a meter observation from an adapter's scraped `usage` object:
`total_tokens`, or the sum of `input_tokens` and `output_tokens`. In that
built-in mode, `consumptionCap` and the estimate are token-denominated. The
meter can only reduce apparent headroom relative to tally's own debits; a low
or malformed observation cannot grant budget.

That token denomination is current behavior, not a generic unit abstraction.
Older Nix prose left it unstated.

## What flows hold today

Every generated flow runner is admitted to the reserved `flow` pool. A flow
may additionally name one typed `workloadMutex`: a capacity-1 co-residency
mutex held for the runner process lifetime. It is not an arbitrary extra-pool
list, and runner death releases it before replay. A replay blocks behind the
next holder while already-created children remain durable. Direct manual flow
runs hold no lease, so a mutex flow must enter through an admitted parent job.

A flow's `budgetPool` field is checked only to ensure the named pool exists; it
is not added to the runner's pool set, exported through another channel, or
held for the duration of the run. Nodes acquire their own declared pools as
ordinary jobs. The shipped flow `NodeSpec` deliberately has no
`consumptionEstimate`; configured flow checking excludes
windowed-consumption pools by design. Priorities control contention between
flow workloads. The kernel mechanism remains unchanged for direct and
producer enqueues.

## Lease lifetime

The daemon acquires the lease before launching the deterministic execution
unit. The task UUID, attempt, and lease epoch bind that grant to the launch.
The lease remains coordinator-owned even when the process executes over SSH.
It is released after a canonical terminal outcome, or reconciled through the
recovery path when launch state is uncertain.

Pool parsing and the `budgetGb` validation are in
`crates/tally-core/src/config.rs`. Atomic admission, durable budget debits,
capacity counting, and restart reconstruction are in
`crates/tally-core/src/lease.rs`. Flow runner pool rendering, the typed
`workloadMutex` assertions, and the existence-only `budgetPool` assertion are
in `nix/modules/common.nix`. Tests
`workload_mutex_replay_waits_behind_the_next_process_holder`,
`rolling_window_rebuild_reads_events_and_verified_witness`,
`restarted_admission_debits_a_stable_attempt_only_once`, and
`built_in_usage_feeder_routes_tokens_and_can_only_clamp_headroom_downward`
exercise the two budget paths.

Read current holders, queue depth, and budget signal from the daemon:

```console
$ tally query pools | jq '.pools[] | {pool, capacity, held, queued, remainingBudget, signal}'
```
