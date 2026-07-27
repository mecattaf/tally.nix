# Priorities and preemption

Priority orders runnable work; it does not bypass pool policy. The four
classes have fixed ranks:

| Priority | Rank |
|---|---:|
| `interrupt` | 1000 |
| `high` | 100 |
| `medium` | 50 |
| `low` | 10 |

Among jobs whose complete pool set can be granted, higher effective rank is
considered first. A job that cannot fit does not block a later job whose
different pool set can fit.

## Aging is one step

Once a pending job has waited strictly longer than the configured aging
threshold, tally raises it exactly one class:

- low competes at medium rank;
- medium competes at high rank;
- high competes at interrupt rank;
- interrupt stays interrupt.

The promotion is computed from the original priority and admission time. It
does not recur every threshold interval and does not rewrite the job's durable
priority.

At the same effective rank, tally braids scheduling groups. Siblings from one
flow run form a group; ordinary children share their parent group; parentless
jobs share one standalone group. The sorter takes the first member of each
group before its second members, then their thirds, with group age and enqueue
sequence as deterministic tie-breakers. A wide flow therefore cannot fill the
entire ready queue merely by submitting its nodes first.

## Cooperative yield

Only a job submitted with actual `interrupt` priority asks lower-priority
holders to yield. Aging a high job to interrupt rank changes ordering but does
not manufacture an interrupt request.

A checkpoint-aware workload observes the yield request through the adapter's
yield hook or `tally lease status`, saves whatever state its own protocol
understands, and exits. tally does not infer a safe checkpoint from process
state.

Yield is initially cooperative. If every blocking pool involved in an
interrupt request has `hardPreempt` enabled, a holder that has not yielded by
the configured grace deadline becomes eligible for hard reclaim. With
`hardPreempt` disabled, the request remains queued; tally does not kill the
holder. Multi-pool requests choose blockers without releasing a partial set,
and a windowed budget that cannot admit the interrupt does not trigger
unrelated yields.

A reclaimed job receives the canonical `preempted` verdict. Recovery and
same-row retry policy decide whether it later runs again; preemption never
rewrites the prior attempt as if it had completed normally.

The ranks are defined in `crates/tally-core/src/config.rs`. Aging, group
braiding, yield demand reconciliation, and hard-reclaim eligibility are in
`crates/tally-core/src/lease.rs`; daemon-side process reclaim is in
`crates/tally-core/src/daemon.rs`. The tests
`fleet_conformance_fairness_braids_400_node_flow_siblings_and_standalone_work`,
`every_priority_uses_the_normative_single_step_aging_map`, and
`later_hard_preempt_request_upgrades_an_existing_soft_yield` pin the boundary.

Compare queue order and current pool pressure together:

```console
$ tally query jobs --pool local --state queued | jq '.items[] | {taskUuid, priority, parentTaskUuid, orchestration}'
$ tally query pools | jq '.pools[] | select(.pool == "local")'
```
