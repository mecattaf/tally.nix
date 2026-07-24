# tally product specification

This document describes the behavior implemented by the current Rust workspace and Nix modules.
For exact module types and generated units, see [NIX-SPEC.md](NIX-SPEC.md).

## 1. Product boundary

tally arbitrates contention and emits proof. It never decides what work should run next.

The inputs to tally are already-decided work descriptions: direct argv, requested resource pools,
priority, evidence requirements, and optional provenance. The outputs are admission decisions,
execution state, durable verdicts, and query projections. Domain policy remains in the caller.

This boundary has practical consequences:

- payloads are opaque argv, not workflow objects;
- model names, manifest hashes, and evidence classes are preserved rather than interpreted;
- producers narrow declared observations into ordinary enqueue payloads;
- adapters render and scrape process envelopes but do not reason about task content;
- journald is observational and cannot override a witnessed terminal fact; and
- an external unit may append an advisory attestation but cannot forge a job verdict.

## 2. Components

The workspace builds one `tally` binary from two crates:

- `tally-core` owns configuration, leases, durable enqueue events, execution, evidence, recovery,
  adapters, producers, journald records, query projections, and ledger verification.
- `tally` exposes the CLI, Unix-socket RPC client/server entry points, hidden systemd exit-record
  support, and producer/adapter dispatch entry points used by generated units.

The package also installs a `tallyd` symlink to the same binary. There is no second daemon
implementation.

At runtime, the daemon owns:

1. a Unix request/response socket;
2. the sole lease engine over its configured logical pools;
3. deterministic `tally-job-<uuid>.service` transient units, either local or on named daemonless
   workers;
4. durable enqueue events and lease events in the state directory;
5. a canonical witness ledger and a separate attestation ledger in the data directory; and
6. a TaskChampion SQLite projection that can be rebuilt from durable facts.

## 3. Configuration and admission

Configuration is strict JSON. Unknown fields fail decoding. Nix module configurations are rendered
to JSON and validated by the production parser during evaluation/build, so an invalid graph fails
before activation.

An enqueue payload contains direct argv, one or more pool names, a priority, an adapter name, an
optional named executor, and optional deduplication, provenance, evidence, runtime, consumption,
credential, and advisory metadata. The daemon validates the complete payload and every pool,
adapter, and executor reference before acknowledging it. Omitting the executor selects local
execution on the coordinator.

The serialized key remains `pool`. A singleton is accepted and emitted as its legacy scalar;
multiple pools are emitted as an array sorted lexically. The same scalar-or-array rule applies to
persisted rows and ledger records, while string-only Taskwarrior UDA and environment values use a
JSON array string for multiple pools. This preserves every previously expressible record byte for
byte. Multi-pool payloads require a daemon that implements the array form.

Job-originated enqueue is capability-limited:

- `depthCap` limits parent-to-child depth;
- `fanoutCap` limits accepted children for one parent;
- `requireDedupKey` requires a stable existence key by default; and
- `noEnqueue` removes the capability for an advisory leaf.

These checks apply at the socket boundary. A caller cannot bypass them by selecting a producer or
adapter.

## 4. Resource pools and leases

Pools are named logical resource gates owned by one coordinator daemon. Ownership identifies the
daemon that arbitrates contention; it is not an execution-placement attribute. Resource kinds are
`vram`, `build-slot`, `cpu-slot`, `budget`, and `mutex`.

The co-residency predicate admits at most `capacity` simultaneous holders. A VRAM pool with more
than one holder may additionally constrain the sum of declared `budgetGb` demands. A mutex is a
co-residency pool with capacity one.

The windowed-consumption predicate is valid only for `budget`. It admits an authoritative
`consumptionEstimate` while the sum inside `windowSec` remains at or below `consumptionCap`.
Debits are keyed by stable admission identity so restart does not double-charge one attempt. An
optional external usage meter can reduce reported headroom; it cannot increase the authoritative
cap.

Every request names a non-empty set of known pools with no duplicates. tally sorts that set
lexically before admission and uses the canonical order in persistence, events, queries, and
witness material. Multi-pool requests are atomic: tally either grants every requested pool or
queues the request without holding a subset.

Priorities have stable numeric ranks:

| Priority | Rank |
|---|---:|
| `interrupt` | 1000 |
| `high` | 100 |
| `medium` | 50 |
| `low` | 10 |

Higher-ranked queued work can request that a lower-ranked holder yield. The holder observes the
request through `tally lease status` or an adapter yield hook. Pools are cooperative by default.
When `hardPreempt` is enabled, a holder that does not yield within `yieldGraceSec` is reclaimed and
receives a `preempted` verdict.

An interrupt request against a pool with `hardPreempt = false` is non-destructive. It sorts ahead
of lower-priority queued work but remains pending for any current holder, regardless of elapsed
grace. A worker thermal sensor can therefore enqueue a coordinator-local `sleep 1800` against
`worker-gpu`: it waits for active GPU work, holds the logical pool for 30 minutes, and releases on
normal exit without terminating an LLM process.

Each daemon start durably bumps the lease epoch. The epoch fences stale local facts during
recovery.

## 5. Execution

The production daemon requires systemd ownership. It launches direct argv with a deterministic
transient unit using `systemd-run --wait --collect`; it does not turn argv into a shell string.

Each unit receives proof-bearing environment such as the job ID, task UUID when present, pool set,
lease epoch, priority class, attempt, enqueue capability, socket, and credential names. Optional
fields are explicitly unset when absent so inherited environment cannot impersonate them.

For producer-launched GitHub jobs, a versioned origin carries repository, item number and URL,
item kind, immutable PR head SHA, node ID, trigger kind and actor, event/comment IDs, and distinct
item-author/self-actor/notification-reason fields. The executor exposes only bounded scalar
identity as `TALLY_GH_*`; arbitrary title, body, label, assignee, and comment text stays in a
validated private JSON context file. Direct enqueue has no GitHub environment.
Native intake never substitutes the item author for an unavailable trigger actor. It attributes a
notification's latest comment only when the API comment identity and timestamp link it to that
notification; candidates without authoritative actor data fail closed as
`trigger-actor-unavailable` without stopping unrelated candidates.

The executor applies CPU and memory limits, an optional `RuntimeMaxSec`, private capture files for
stdout and stderr, and an `ExecStopPost` helper that writes a durable invocation-linked exit
record. Credential sources are passed to systemd through `LoadCredential=`; only names enter tally
metadata.

A direct-process fallback exists for focused executor tests, but the crash-survivable daemon path
refuses work it cannot later adopt or reclaim through systemd.

A configured SSH executor places that same execution envelope on a worker without a worker tally
daemon. The coordinator runs an explicitly configured OpenSSH binary with an explicit host, user,
port, identity file, pinned known-hosts file, no ambient SSH configuration or agent, and all
forwarding disabled. The only remote command is the configured tally binary's fixed hidden helper.
Opaque job argv and execution metadata are carried in bounded JSON on stdin, never interpolated
into the SSH command. Credential source paths in a remotely selected job refer to the worker
filesystem and are still handed to systemd with `LoadCredential=`.

The worker helper is a short-lived executor client for its user systemd manager. It implements
idempotent ensure, exact probe/adopt, and exact reclaim operations over the deterministic unit name.
An operation is correlated by unit UUID, attempt, lease epoch, and—after launch—systemd invocation
ID. If a connection fails after dispatch, the coordinator retries the identical operation; the
worker either observes the existing unit or its durable exit record instead of launching again.
The worker fsyncs a generation marker before unit creation. An absent unit and absent exit record
with that same marker is an indeterminate prior launch and is never replayed; `stateDir` therefore
must survive worker restarts.

The coordinator is always the admission authority and keeps the complete logical lease set while
remote state is indeterminate. It releases leases only after a validated terminal reply. Remote
artifact evidence is evaluated against worker paths, then the actual exit record, evidence result,
bounded captures when available, and executor name are incorporated into the coordinator's
canonical terminal processing. A malformed response, mismatched generation/invocation, or
ambiguous unit state fails closed.

## 6. Evidence, verdicts, and deduplication

Evidence specifications are:

- `exit:<0..255>` for an exact exit status;
- `artifact:<absolute-path>` for a required regular artifact; and
- `hash:sha256` or `hash:sha256:<digest>` to hash the ordered artifact set and optionally compare
  it with a fixed digest.

Artifact checks use bounded no-follow reads, reject non-regular files and unstable metadata, and
hash only bytes read after execution. Multiple artifact hashes are combined deterministically.

Canonical verdicts are `pass`, `clean-exit-no-artifact`, `failed`, `cancelled`, `reused`,
`pool-vanished`, `preempted`, and `runtime-exceeded`. A successful process does not imply `pass` if
required evidence is absent.

Deduplication is existence-based. A matching prior `pass` must have the same dedup key and the
current artifact set must rehash to the witnessed value. A hit records `reused`; ambiguity or a
changed artifact fails closed and runs fresh work.

Only eligible fresh attempts contribute canonical usage. Reused or recovered work, cancellation,
pool loss, and preemption do not inflate fresh-work accounting.

## 7. Durability and acknowledgement

The witness ledger is append-only JSONL. Every record contains a sequence number, the previous
record hash, and a SHA-256 hash of its own canonical JSON with the `hash` field cleared. Verification
checks JSON shape, record hash, previous-hash linkage, sequence ordering, gaps, and duplicates.

The daemon has three fsync-before-ack stages:

1. admission—the durable enqueue event exists before enqueue acknowledgement;
2. lease grant—the durable lease fact exists before the grant is exposed; and
3. verdict witness—the canonical witness exists before a terminal result is acknowledged.

TaskChampion is updated as a non-detachable replica of those facts. If a process stops after a
durable acknowledgement but before the projection commit, startup rebuild repairs the cache.

The separate attestation ledger uses the same chain mechanics for advisory external-unit facts.
Its records are never read as canonical verdicts or authoritative usage.

## 8. Restart and return recovery

Startup recovery joins durable enqueue events, witness records, the current lease epoch, systemd
unit state, exit records, and confirmed producer transitions. For a row with a named executor that
could still have work in flight, the unit probe and any subsequent adoption or reconciliation run
against that worker. A final non-retryable witness is already authoritative and does not require
the historical worker to be reachable. For every other remote row, startup waits through transport
loss rather than opening admission with unknown remote work.

For each durable row, recovery can:

- queue an admitted row that never started;
- adopt the exact still-running unit without replaying argv;
- reconcile a durable exit record;
- wait for a stopping unit to be collected;
- re-present the same row and increment its attempt after an eligible trigger; or
- leave an ineligible or exhausted row terminal.

Eligible retry triggers are pool return after `pool-vanished`, resource return after `preempted`,
and a bounded requeue after `runtime-exceeded`. Automatic action is policy-controlled and attempt
bounded. Pool-return recovery preserves the task UUID, switches to the adapter's resume argv when
captures support it, and records recovered rather than fresh labor.

Pool loss becomes authoritative only after the configured reachability hysteresis. The loss intent
is durable before affected work is reclaimed, so restart cannot forget who owns the resulting
verdict.

## 9. Producers

The producer registry is closed over exactly five kinds: `calendar`, `build-effect`,
`pool-reachability`, `gh`, and `events-dir`.

Calendar producers emit a configured payload for a systemd calendar event. Build-effect producers
observe bounded Nix store-path feeds. Pool-reachability producers confirm loss and return through
hysteresis and can emit declared transition work. GitHub notification and search sources carry
per-source repository/owner, label, state, assignee, item-kind, notification-reason, raw-query, and
explicit-item constraints. A source without a repository, owner, or item identity scope matches
nothing. There is no implicit `involves:@me` query.

GitHub intake recognizes only configured exact command comments, mentions, assignments, and label
events. Authorization uses the triggering actor. Each current origin preserves the event,
comment, timestamp, trigger value, item state, and bounded context snapshot. Receipt, dedup, and
deterministic task identity use the comment ID for comment/mention triggers and the event ID for
assignment/label triggers, independent of whether notifications or search observed it. Accepted,
filtered, and first-duplicate outcomes receive idempotent marker-tagged acknowledgements; filtered
remote text does not disclose policy detail. Self-trigger admission remains explicit. After durable
`Pass`/`Reused`, evidence posting and item closing are separate policies; closing is impossible
unless evidence posting is enabled.

`producer preview`, `producer poll --once --no-enqueue`, and `producer explain --item` resolve and
report candidates without writing receipts or ingress. `producer test --item --event --actor` is
also non-mutating by default; `--promote` is the sole opt-in to ingress and acknowledgement. A job
whose true source is not GitHub may store a validated `relatedTrigger` receipt reference while
retaining its actual source.
Events-directory producers accept ordinary files from external scanners.

Ingress files are claimed without following links, bounded, parsed through the same enqueue
narrower, and archived only after acknowledgement. There is no producer-specific execution lane.

## 10. Adapters

Adapters form an open named map. A fresh template is a direct argv prefix. A resume template may
refer to any named `%<capture>%`. Captures select stdout or stderr and use either regex or RFC 9535
JSONPath. Model and session values are preserved verbatim.

Adapter environment cannot override reserved proof-bearing variables. Scraped values are advisory:
they can drive a later resume template or query projection, but they cannot alter admission,
evidence, verdict, charge, or canonical usage.

The built-in Nix presets are `shell`, `pi`, `claude-code`, and `codex`. New programs that fit this
structured envelope are configuration, not new Rust variants.

## 11. Journald and queries

Lifecycle events can be emitted as one bounded JSON line on stdout or, when `journald.native` is
enabled, as a native journal-protocol datagram. Read-time parsing accepts both forms but gives
canonical witness facts precedence.

The CLI exposes status, log, render, standup, and pool projections. Queries are observational:
they do not mutate job state, and pruning journal history cannot erase a witnessed terminal result.
GitHub-backed rows project captured repository, item number, and URL into `RowFact` and standup
completed/in-flight entries without adding scheduling state or changing witness records.

## 12. Security and failure posture

- Strict configuration rejects unknown keys and invalid cross-references.
- Direct argv avoids an implicit shell boundary.
- State, captures, exit records, and ledgers use private permissions and durable rename/append
  patterns.
- Credential values remain outside argv, capture metadata, journals, and ledgers.
- Unit adoption and reclaim pin the exact invocation ID rather than trusting only a reusable unit
  name.
- Remote ensure, probe, adoption, and reclaim retain deterministic task/generation identity across
  transport retries; indeterminate state fails closed instead of releasing a lease or launching
  duplicate work.
- Bounded file and subprocess reads prevent an intake or helper from consuming unbounded memory.
- A failed durability stage fails the request; it is never reported as acknowledged success.

The complete list of design surface that is intentionally absent appears once in the
[deferred section of NIX-SPEC.md](NIX-SPEC.md#10-deferrednot-implemented).
