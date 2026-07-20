# tally implementation sequence

This is a reader's map of the implemented system, arranged in dependency order. It is not a
project diary or a roadmap. Each stage names the code that owns the behavior and the proof that is
kept in the repository.

The product invariant across every stage is: contention and proof, never content or control.

## 0. Workspace, configuration, and executable surface

`Cargo.toml` defines the `tally-core` library crate and the `tally` binary crate. `flake.nix`
packages one executable, installs the `tallyd` alias, supplies the development shell, and exports
the NixOS and Home Manager modules.

`crates/tally-core/src/config.rs` is the strict runtime configuration shape. It rejects unknown
fields, invalid pool graphs, malformed adapter envelopes, and producer cross-references. The Nix
modules render this same shape and invoke `tally --mode check-config` during the build.

Proof:

- Rust configuration tests cover defaults, strict decoding, pool constraints, adapters, and the
  five producer kinds.
- Flake checks reject malformed JSON, invalid pools, missing producer kinds, and absent option
  surface.

## 1. Hash-chained witnesses and attestations

`crates/tally-core/src/witness.rs` defines canonical verdict records, advisory attestation records,
SHA-256 chain construction, locking, append durability, and offline verification.

Each record has a monotonic `seq`, a `prev_hash`, and a hash of its canonical JSON with `hash`
temporarily cleared. Verification reports parse errors, invalid shape, hash mismatch, broken
linkage, bad order, gaps, and duplicates.

The two chains have different authority:

- `witness.jsonl` is written by the daemon's terminal transaction and is authoritative for verdicts
  and accounting.
- `attestations.jsonl` accepts advisory external-unit facts and cannot create a job verdict.

Proof:

- `test/fixtures/ledger/valid.jsonl` verifies successfully.
- `test/fixtures/ledger/tampered.jsonl` is rejected with a hash mismatch.

## 2. Durable rows, wire protocol, and admission guardrails

`crates/tally-core/src/taskdb.rs` maps durable enqueue events and witness facts into TaskChampion.
The SQLite database is a rebuildable query projection, not the sole source of truth.

`crates/tally-core/src/wire.rs` defines newline-delimited JSON RPC over a Unix socket, bounded frame
sizes, request validation, and response errors. `crates/tally/src/main.rs` maps public CLI commands
onto that protocol.

Admission narrows all sources into one payload shape and enforces parent depth, parent fanout,
dedup-key requirements, pool/adapter references, credential safety, and the one-hop `noEnqueue`
capability.

Proof:

- unit tests cover wire framing and adversarial payloads;
- `crates/tally/tests/cli_rpc.rs` exercises the real CLI/server boundary; and
- `test/scenarios/fanout-guardrail.sh` sends concurrent clients through the real socket.

## 3. Local leases and cooperative yield

`crates/tally-core/src/lease.rs` owns lease epochs, the durable lease-event log, co-residency,
windowed consumption, atomic multi-pool grants, deterministic queue order, preemption planning,
release, cancellation, and restart reconstruction.

Every daemon start bumps the epoch. A stale epoch cannot reclaim a current grant. Co-allocation is
all-or-nothing, so a blocked job never holds a subset of its requested resources.

Priorities are `interrupt=1000`, `high=100`, `medium=50`, and `low=10`. Higher-ranked work can ask a
holder to yield. Cooperative status is visible through `tally lease status`; an opted-in pool can
hard-reclaim after `yieldGraceSec`.

Proof:

- lease tests cover fairness, capacity, co-allocation, window rebuild, stable debits, yield, and
  epoch fencing; and
- an ignored live test checks a real transient user unit when explicitly enabled.

## 4. Systemd execution, evidence, and deduplication

`crates/tally-core/src/executor.rs` owns deterministic transient-unit names, direct argv rendering,
systemd properties, proof-bearing environment, credential references, capture files, timeout
classification, exit records, invocation-pinned inspection, adoption, and reclaim.

`crates/tally-core/src/evidence.rs` parses exit/artifact/hash requirements, performs bounded
no-follow artifact reads, combines hashes, synthesizes verdicts, probes existence-based dedup, and
classifies retry eligibility.

The production daemon requires systemd ownership. A transient unit is the durable execution handle
that lets a restarted daemon distinguish running, exited, stopping, absent, and indeterminate work.

Proof:

- pure executor tests use controlled `systemd-run` and `systemctl` doubles;
- evidence tests cover path attacks, unstable files, combined hashes, verdicts, and dedup misses;
- `crates/tally/tests/executor_live.rs` is an explicit opt-in smoke against a real user manager.

## 5. Restart and return recovery

`crates/tally-core/src/recovery.rs` joins durable rows, witnesses, unit facts, exit records, the
current epoch, and confirmed return triggers. It produces explicit actions: queue, adopt, reconcile,
wait for collection, re-present the same row, wait for a trigger, or mark retries exhausted.

Recovery never replays argv for a unit it adopts. Re-presentation preserves the durable task UUID,
increments the attempt, records recovered labor, and uses an adapter resume template only when the
required captures exist.

The daemon persists pool-loss intent before reclaiming affected units. This makes a confirmed loss
restart-safe and gives one owner authority to publish `pool-vanished`.

Proof:

- recovery tests exercise every action and stale-epoch fence;
- daemon tests cover adoption, exit reconciliation, exact-row re-presentation, and crash gaps; and
- `test/scenarios/pool-vanished-return.sh` provides an optional real-host reboot proof.

## 6. Journald and read-time queries

`crates/tally-core/src/journal.rs` renders one validated lifecycle shape either as bounded JSON
stdout or native journal protocol. It parses both forms without allowing message content to shadow
native fields.

`crates/tally-core/src/query.rs` joins journal observations with durable rows and witnesses into
status, log, render, standup, and pool projections. A witness wins over observational journal data,
and a newer attempt is not hidden by an older terminal attempt.

Proof:

- journal and query unit tests cover validation, both encodings, pruned history, attempts, and
  canonical totals; and
- `crates/tally/tests/journal_live.rs` is an explicit opt-in round trip through a real user journal.

## 7. Daemon transaction and supervision

`crates/tally-core/src/daemon.rs` composes the preceding stages. The acknowledgement boundary has
three durable checkpoints: admission, lease grant, and verdict witness. A socket reply is not sent
before its applicable checkpoint is fsynced.

Long-running replica and post-ack tasks are supervised and joined during shutdown. The daemon sends
systemd readiness, watchdog, and stopping notifications. A drain barrier snapshots admitted work
and resolves only after every member reaches an acknowledged terminal result.

Proof:

- daemon tests inject failures at each durability boundary and restart between durable fact and
  SQLite projection;
- `test/scenarios/slow-sqlite.sh` holds the real SQLite commit path and proves the socket remains
  usable and restart rebuild repairs the projection; and
- ignored daemon live tests cover watchdog survival, adapter capture, contention, and adoption.

## 8. Structured adapters and the five producers

`crates/tally-core/src/adapters.rs` validates open-map adapter definitions, substitutes named resume
captures, scrapes regex or RFC 9535 JSONPath from bounded captures, and preserves model/session
values verbatim. Adapter data may affect argv and advisory projections; it cannot affect evidence,
verdicts, or accounting.

`crates/tally-core/src/producers.rs` implements exactly `calendar`, `build-effect`,
`pool-reachability`, `gh`, and `events-dir`. Every emitted item becomes an ordinary ingress file and
passes the same admission path. Claim/archive operations are bounded and durable; reachability
transitions are hysteresis-confirmed; GitHub completion mutation follows durable success.

Proof:

- adapter tests cover fresh/resume rendering, multi-capture substitution, both scrape modes,
  reserved environment, and capture bounds;
- producer tests cover strict registry validation, each observation type, global ingress locking,
  GitHub narrowing/completion, and reachability transition ownership; and
- flake checks render and execute the Nix-defined presets and all five producer configurations.

## 9. Declarative modules and end-to-end gates

`nix/modules/common.nix` owns typed options, cross-field assertions, checked JSON rendering,
credential rendering, and shared exports. `nix/modules/home-manager.nix` owns the user daemon,
producer/meter units, event drain, autonomous timers, and stale-unit cleanup.
`nix/modules/nixos.nix` owns the hardened system daemon and its state/log directories.

The stock-host VM boots both module forms, waits for their daemons, and proves autonomous first
firings for the event drain and an events-directory producer. In particular, recurring timers have
an `OnActiveSec` first trigger as well as `OnUnitActiveSec` cadence.

The repository's complete ordinary gate is:

```console
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command cargo test --workspace
$ nix develop --command cargo clippy --workspace --all-targets -- -D warnings
$ nix develop --command cargo fmt --all --check
$ nix flake check -L
$ nix develop --command test/scenarios/run fanout-guardrail
$ nix develop --command test/scenarios/run slow-sqlite
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command \
    test/scenarios/run pool-vanished/return
```

The last command must report `SKIP` and exit zero when no second host is configured. The explicit
multi-host run and ignored live-system tests are additional opt-in evidence, not prerequisites for
a stranger's ordinary local suite.

The exact Nix surface and the single list of intentionally absent design work are in
[NIX-SPEC.md](NIX-SPEC.md).
