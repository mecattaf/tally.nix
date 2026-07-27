# Producers

A producer observes one declared source of intent and narrows an observation
into an ordinary enqueue payload. After that handoff, the job follows the same
admission, pools, executor, evidence, and witness path as a direct CLI job.

The producer registry is deliberately closed over five kinds:

| Kind | Observation |
|---|---|
| `calendar` | a systemd calendar firing |
| `events-dir` | bounded ingress files awaiting the daemon drain |
| `gh` | identity-scoped GitHub notifications or searches and explicit triggers |
| `build-effect` | distinct Nix store paths seen in a roots directory, JSONL file, or post-build-hook stream |
| `pool-reachability` | hysteresis-confirmed loss or return of a configured pool |

A declarative flow with a calendar is rendered through the `calendar` kind. It
does not create a sixth producer protocol.

## Why the registry is closed

Producers do privileged narrowing. They validate observations, derive durable
identities and deduplication keys, attach origin, and sometimes acknowledge or
mutate an external system. Adding a kind therefore requires audited Rust and
Nix behavior, not merely an executable name.

Adapters are different: they only turn an already admitted job into direct argv
and advisory capture rules, so their registry can remain open.

The Home Manager module renders producer services and timers. The NixOS system
module runs the coordinator but intentionally does not generate producer,
meter, or flow units.

## Idempotence belongs at intake

Calendar deduplication keys may contain bounded `strftime` expansion.
Build-effect identity is the observed store path. Events-directory claims use
atomic, recoverable file moves. Pool reachability requires consecutive
observations before emitting a transition and permits only one producer to own
each probed pool.

The GitHub producer has the largest narrowing surface. A source must be scoped
by declared repository/owner/item identity, then an observation must match an
explicit comment, mention, assignment, or label trigger. The trigger actor is
kept distinct from the authenticated identity. Event and comment identities
back durable receipts, so a retry can acknowledge the same intake without
launching another job.

GitHub completion effects are also explicit policy: receipts, evidence
comments, gate summaries, review requests, and item state changes are separate
choices. `neverMutate` is an absolute override. Canonical completion remains in
the witness ledger even when every GitHub mutation is disabled or an external
API call must be retried.

## Observe before promoting

The CLI provides read-only or non-enqueuing diagnostics for the most sensitive
producer:

```console
$ tally producer preview github
$ tally producer explain github --item https://github.com/OWNER/REPO/issues/123
$ tally producer test github \
    --item https://github.com/OWNER/REPO/issues/123 \
    --event command-comment \
    --actor alice \
    --no-enqueue
```

`poll --no-enqueue` exercises live intake without admitting work. `test
--promote` is the explicit mutating diagnostic and should be treated as a real
enqueue.

The strict registry, intake claims, receipts, and effect state machines live in
`crates/tally-core/src/producers.rs`. Inventory projection is in
`crates/tally-core/src/producer_query.rs`; unit rendering is in
`nix/modules/home-manager.nix` and `nix/modules/common.nix`. The
`producer-registry` and `stock-host-activation` flake checks prove the rendered
five-kind surface. Tests
`registry_is_strict_open_by_name_and_closed_over_the_in_scope_kinds`,
`github_comment_receipts_ack_one_accept_one_duplicate_and_a_later_job`, and
`fleet_conformance_network_blip_and_true_vanish_are_distinguished_by_hysteresis`
pin the main runtime boundaries.

Query effective configuration and the last durable runtime observation
together:

```console
$ tally query producers | jq '.items[] | {name, kind, enabled, schedule, runtime}'
```
