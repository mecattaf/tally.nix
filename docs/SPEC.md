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
2. a local lease engine over configured pools;
3. deterministic `tally-job-<uuid>.service` transient units;
4. durable enqueue events and lease events in the state directory;
5. a canonical witness ledger and a separate attestation ledger in the data directory; and
6. a TaskChampion SQLite projection that can be rebuilt from durable facts.

## 3. Configuration and admission

Configuration is strict JSON. Unknown fields fail decoding. Nix module configurations are rendered
to JSON and validated by the production parser during evaluation/build, so an invalid graph fails
before activation.

An enqueue payload contains direct argv, one or more pool names, a priority, an adapter name, and
optional deduplication, provenance, evidence, runtime, consumption, credential, and advisory
metadata. The daemon validates the complete payload before acknowledging it.

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

Each daemon start durably bumps the lease epoch. The epoch fences stale local facts during
recovery.

## 5. Execution

The production daemon requires systemd ownership. It launches direct argv with a deterministic
transient unit using `systemd-run --wait --collect`; it does not turn argv into a shell string.

Each unit receives proof-bearing environment such as the job ID, task UUID when present, pool set,
lease epoch, priority class, attempt, enqueue capability, socket, and credential names. Optional
fields are explicitly unset when absent so inherited environment cannot impersonate them.

The executor applies CPU and memory limits, an optional `RuntimeMaxSec`, private capture files for
stdout and stderr, and an `ExecStopPost` helper that writes a durable invocation-linked exit
record. Credential sources are passed to systemd through `LoadCredential=`; only names enter tally
metadata.

A direct-process fallback exists for focused executor tests, but the crash-survivable daemon path
refuses work it cannot later adopt or reclaim through systemd.

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
unit state, exit records, and confirmed producer transitions.

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
hysteresis and can emit declared transition work. GitHub producers use explicit notification or
search sources, exclude the configured actor, deduplicate immutable item IDs, and optionally make
an idempotent completion mutation only after durable success. Events-directory producers accept
ordinary files from external scanners.

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

## 12. Security and failure posture

- Strict configuration rejects unknown keys and invalid cross-references.
- Direct argv avoids an implicit shell boundary.
- State, captures, exit records, and ledgers use private permissions and durable rename/append
  patterns.
- Credential values remain outside argv, capture metadata, journals, and ledgers.
- Unit adoption and reclaim pin the exact invocation ID rather than trusting only a reusable unit
  name.
- Indeterminate unit state fails closed instead of launching duplicate work.
- Bounded file and subprocess reads prevent an intake or helper from consuming unbounded memory.
- A failed durability stage fails the request; it is never reported as acknowledged success.

The complete list of design surface that is intentionally absent appears once in the
[deferred section of NIX-SPEC.md](NIX-SPEC.md#10-deferrednot-implemented).
