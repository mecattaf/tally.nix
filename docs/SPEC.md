# tally.nix — Specification

> **The one law:** tally tracks **contention** and **proof** — never **content** or **control**.
> It arbitrates who may use a scarce resource and records verifiable evidence of what
> happened. It never inspects what a job produces, and it never originates or drives work.

tally.nix makes a machine's impure, contended, evidence-bearing labor — LLM runs, GPU
holds, builds, API-budget draws — into declarative NixOS infrastructure. You declare the
resources and the workloads; tally leases the resources, spawns the work as systemd
transient units, and appends a hash-chained, independently verifiable ledger of every
verdict.

This document is the authoritative product specification. It is complete and settled.

---

## 1. Load-bearing properties

These are the invariants a conforming implementation guarantees. Everything else in this
document serves them.

- **Contention, not content.** tally arbitrates access to scarce resources and records
  proof of outcomes. It never reads, stores, or reasons about the payload of a job.
- **Proof is durable and verifiable.** Every terminal verdict is a line in an append-only,
  hash-chained ledger that any party can verify offline with `tally witness verify`, with
  no access to tally's internal state.
- **The daemon never originates work.** All work enters through declared producers or
  explicit enqueues. The daemon arbitrates and executes; it never invents a job.
- **The ledger is the truth; everything else is a rebuildable cache.** The witness chain
  plus the `events/` records are the sole source of truth. The embedded task database is a
  derived cache, reconstructable from the ledger at any time.
- **Leases are enforced, not advised (where the fleet supports it).** A VRAM lease is
  backed by a real cgroup memory ceiling on the host that serves the resource, read back
  from the kernel rather than trusted.
- **Recovery is workflow-agnostic.** Reboots, coordinator switches, and resource-pool loss
  are survived by three generic primitives, not by any workflow baked into the binary.
- **Offline-first.** tally requires no network for its core function. Host-to-host
  coordination is the only networked path, and it degrades cleanly when absent.

---

## 2. What tally is NOT

This list is normative. A conforming implementation does not grow these capabilities.

- **Not a work originator / scheduler.** The daemon never invents jobs. Work is produced by
  declared producers or enqueued by callers. There is no cron-in-the-daemon.
- **Not a container or effect sandbox.** Jobs run as ordinary systemd transient units on the
  host. There is no containerized effect runtime and no state-file effect API.
- **Not a remote-execution engine.** A job-owning unit is always coordinator-local. A remote
  worker holds a lease token and a resource cgroup — never a spawned tally job.
- **Not a message bus.** There is no bespoke pub/sub delta stream. journald is the stream;
  the socket carries only request/response RPC.
- **Not a secrets manager.** Credentials are passed through by reference via systemd
  `LoadCredential`. tally never reads secret bytes.
- **Not a terminal/session manager.** There is no terminal, pane, or agent-state detector in
  the core. A session reference survives only as an opaque scraped string.
- **Not a model registry.** Model identifiers are recorded verbatim. There is no
  normalization against any external model catalog.
- **Not a general coordination plane.** The only coordination surface is the resource box
  (leases, barriers, admission). tally does not attempt to be a fleet control plane.
- **Not a driver of interactive harnesses.** tally launches argv processes and scrapes their
  captured output. Approval-gated or streaming-interactive harnesses are explicitly out of
  scope.

---

## 3. Architecture

### 3.1 One flake, one repo, one binary

tally.nix is a single flake at `github.com/mecattaf/tally`. It ships two layers together
(microvm.nix precedent):

1. A single self-contained Rust binary that is **both the daemon and the CLI**.
2. A pure-Nix module layer (`nixosModules.tally`, `homeManagerModules.tally`) that declares
   resources, producers, adapters, and workloads.

The two crates are `tally-core` (the library) and `tally` (the binary).

### 3.2 The binary is daemon and CLI

There is no client/daemon binary split and no trust boundary between them. `tally daemon
run` is a subcommand of the same binary. A `tallyd` `argv[0]` symlink and
`SyslogIdentifier=tally` are cosmetic conveniences for service management.

The binary shells out to **exactly two** external programs, total: `systemd-run` and `gh`.
There are no other runtime binary dependencies.

### 3.3 Rust core with embedded TaskChampion

The core embeds `taskchampion::Replica<SqliteStorage>` **in-process**. There is no
`task`-binary shell-out and no FFI wall.

The replica is a **rebuildable cache** of pending/derived state — never a second source of
truth. It is reconstructable at any time from the witness chain (the terminal-transition
record) plus the `events/` directory. External read access to the task database is
ReadOnly-enforced purely to avoid two writers racing on one SQLite file, not to protect
authority — authority lives in the ledger.

### 3.4 The lease-grantor is the unit-spawner

There is no separate broker process. The queue, the lease engine, and the witness append
are one transactional path in the daemon. Admitting a job and granting its lease are a
single in-process decision; the lease and the job's cgroup are co-created.

### 3.5 The executor

The executor owns the `systemd-run` invocation. It spawns transient units with:

- Deterministic names: `tally-job-<task_uuid>`.
- Durable exit records written by `ExecStopPost` under `unit-exit/`, so a unit's outcome
  survives even if the daemon was down when it exited.
- `CPUWeight` and `MemoryMax` **always**; `dmem.max` when the pool's enforcement backend
  supports it.
- Per-job captured streams: `StandardOutput`/`StandardError` are directed to
  `capture/<uuid>.out` (equivalently read back from the unit journal by invocation id) so
  a detached unit's output remains available to the scrape engine.

### 3.6 Storage layout

| Path | Location | Role |
|------|----------|------|
| `witness.jsonl` | `XDG_DATA_HOME` (`LogsDirectory=tally` for the system daemon) | Canonical verdict chain — durable proof |
| `attestations.jsonl` | `XDG_DATA_HOME` (`LogsDirectory=tally`) | Advisory attestation chain — durable proof |
| `taskchampion.sqlite3` | `XDG_DATA_HOME` | Durable but **rebuildable** cache |
| `events/` | `XDG_STATE_HOME` (`StateDirectory=tally`) | Producer intake records |
| `unit-exit/` | `XDG_STATE_HOME` (`StateDirectory=tally`) | Durable unit exit records |
| `epoch` | `XDG_STATE_HOME` (`StateDirectory=tally`) | Lease-epoch fencing token |
| `capture/` | `XDG_STATE_HOME` (`StateDirectory=tally`) | Per-job captured output |

The NixOS system daemon uses systemd `LogsDirectory=tally` (proof) and `StateDirectory=tally`
(mutable state) rather than hand-resolving XDG paths.

---

## 4. Pools & enforcement

### 4.1 Pools generalize to every scarce resource

`pools.<name>` models any contended resource: `vram`, `build-slot`, `cpu-slot`, and
windowed budgets such as `api` or `sub:<acct>`. A pool has a capacity (`capacity = 1` by
default; set `> 1` for co-residency) and an admission predicate.

### 4.2 Admission predicates

`predicate ∈ { co-residency, windowed-consumption }`, default `co-residency`.

- **co-residency** admits up to `capacity` concurrent holders.
- **windowed-consumption** admits against a rolling budget and carries its own `windowSec`
  and `consumptionCap` parameters.

### 4.3 The `enforce` enum

`pools.<name>.enforce ∈ { cooperative | dmemcg-booster | dmem }`, set per host. This enum is
the stable contract — a concrete enforcement mode, not a roadmap.

- **`cooperative`** — the portable default. A lease is a token; holders honor it
  voluntarily. Always available on stock nixpkgs; a fresh flash activates cleanly here.
- **`dmemcg-booster`** — an intermediate mode backed by the `pkgs.dmemcg-booster` overlay
  derivation.
- **`dmem`** — real kernel-enforced VRAM ceilings (see §4.4). This is the target production
  enforcement for VRAM pools.

### 4.4 dmem: enforced VRAM leases on the fleet

On this fleet — NixOS on an AMD host — VRAM leases are enforced first-class through the
kernel `dmem` cgroup controller. This is how tally.nix confines VRAM:

- The kernel is built with `CONFIG_CGROUP_DMEM` and the `amdgpu` driver participates in the
  `dmem` controller.
- systemd applies the ceiling via `DeviceMemoryMax`, supplied by a pinned patched-systemd
  overlay.
- `nixosModules.tally` auto-wires `Delegate=yes` and the `dmem` `subtree_control` on the
  relevant slice whenever any declared pool sets `enforce = "dmem"`.
- The ceiling is **verified, not trusted**: tally reads back `dmem.current` from the kernel
  rather than assuming the write took effect.
- `enforce = "dmem"` asserts its prerequisites at startup and **fails loudly** on a host
  that cannot honor it, rather than silently degrading to a no-op.

`cooperative` remains the portable fallback for hosts without this kernel/systemd stack.

### 4.5 Remote pools & negotiated enforcement

A pool may be served by another host. Remote addressing folds into `pools.<name>.remote` (a
nullable submodule carrying `remote.host`); there is no parallel remote-pool namespace and
no `role`/`conductorHost` concept — reachability is emergent from `remote.host`. The
host-to-host port is a typed option defaulting to `7331`.

The HTTP seam exists **only** host-to-host, behind a `LeaseBackend` trait with `Local` and
`Remote` implementations. Admitting a job and granting its lease remain a single in-process
decision on the owning host.

Enforcement of a remote pool is the **broker's** contract, negotiated at grant time — never
a local check. Because the VRAM-consuming process (e.g. a long-running llama-swap service)
is a foreign service tally did not spawn:

- The coordinator **must not** stamp `DeviceMemoryMax` on its own transient unit for a
  remote pool — that would confine nothing while appearing to.
- The worker declares `pools.<name>.servingSlice`, the systemd slice the foreign serving
  process runs under. The worker's `nixosModules.tally` wires `Delegate=yes` on that slice,
  and the worker's own tally instance writes `DeviceMemoryMax` to the delegated cgroup
  out-of-band from any transient unit.
- The `LeaseBackend` handshake carries the broker's advertised enforcement backend plus a
  live capability token (`DeviceMemoryMax` present, `dmem.current` readable). A coordinator
  **refuses or downgrades** a `dmem`-enforced remote grant whose broker does not advertise
  the capability.

Thus `enforce = "dmem"` is a local kernel/systemd startup assertion for a local pool, and a
negotiated capability check at grant time for a remote pool.

### 4.6 Credentials

Every pool, producer, and job schema carries a `credentials` name→path map, spliced onto the
transient unit as `--property=LoadCredential=<name>:<path>`. tally passes credentials by
reference; it never reads or stores secret bytes.

---

## 5. The witness ledger

### 5.1 Two chains

tally maintains two independent hash-chained ledgers:

- **`witness.jsonl` — the canonical verdict chain.** Only the in-process transactional core
  writes verdict-class lines here. These lines are the sole input to canonical metering
  (`charge`, `gpu_seconds`, `standup`).
- **`attestations.jsonl` — the attestation chain.** Foreign units and advisory leaves wire
  into it via `tally witness append` (and the exported read-only wrapper store path
  `tally-witness-emit`). Attestation lines are advisory and observational. They are
  **excluded** from canonical metering, and `tally witness verify` reports them on their own
  chain as unauthenticated-by-construction.

This split closes the forgery path: a foreign unit cannot mint a `pass` verdict that
poisons metering, because it can only write to the attestation chain, never the verdict
chain.

### 5.2 The canonical chain format

The canonical verdict chain is byte-stable and preserved exactly: the record shape and
field insertion order, `canonicalHashInput`, the `sha256:`+hex hash formula,
`prev_hash`/`seq`, `GENESIS_PREV_HASH`, the four-pass verify walk, the `VerifyProblem`
taxonomy, and the `O_APPEND`+`fsync` framing.

Provenance (`--parent`) is deliberately carried **off** the canonical chain — in the task
row (`parent_uuid` UDA), the `events/` payload, and the journald `TALLY_PARENT` field —
never in `canonicalHashInput`. Adding a field to the verdict record would change
canonicalization; provenance is recovered by joining the row/journal, not the chain.

### 5.3 Verdicts

The verdict set:

| Verdict | Meaning | Metered? | Re-presentable? |
|---------|---------|----------|-----------------|
| `pass` | Job completed and produced its declared artifact | Yes | — |
| `clean-exit-no-artifact` | Job exited cleanly but produced no artifact | Yes | — |
| `failed` | Job failed | Yes | — |
| `cancelled` | Job cancelled | — | — |
| `reused` | An existing artifact satisfied the request | Yes | — |
| `pool-vanished` | The remote pool/broker was lost mid-hold | **Excluded** from canonical `gpu_seconds` | On pool return |
| `preempted` | An interrupt-tier holder was hard-reclaimed after its yield grace | — | On resource return |

`pool-vanished` and `preempted` are set only on a **sustained** condition, never a single
blip (see §6.3, §7.2). Both remain eligible for re-presentation — never replay — of the
existing durable row.

### 5.4 The evidence gate

Every terminal transition passes through the evidence gate before its verdict is
synthesized. The gate distinguishes a real artifact from a clean exit that produced nothing,
yielding `pass` vs `clean-exit-no-artifact`.

---

## 6. Priority & preemption

### 6.1 Tiers

Priority has a reserved top tier above the ordinary levels:

| Tier | Rank |
|------|------|
| `interrupt` | 1000 |
| `high` | 100 |
| `medium` | 50 |
| `low` | 10 |

The tier set is a single canonical enum, reused by the clap `ValueEnum` and the Nix
`types.enum`, and validated by `tally --mode check-config`.

### 6.2 The `interrupt` tier

`interrupt` is best-effort cooperative by default: on an `interrupt`-tier admission the
daemon flags the lowest-rank same-pool holder to yield, then hard-reclaims the lease after
a bounded `yieldGraceSec` if the holder has not released. Per-pool **hard-preempt** is an
opt-in that skips straight to reclaim.

A holder reclaimed this way is recorded `preempted` and is re-presentable.

### 6.3 The cooperative yield channel

A leaf learns it must yield by **polling its lease status** — an optional
`adapters.<name>.yieldHook` probe the harness runs at checkpoints, on a bounded
`yieldPollSec`. If the holder does not release within `yieldGraceSec`, the lease is
hard-reclaimed. Both timeouts are typed options.

---

## 7. Recovery

Recovery is workflow-agnostic. tally exposes exactly three generic primitives:

1. The resource-loss verdict (`pool-vanished`, §5.3).
2. The `interrupt` tier (§6).
3. A pool-reachability probe (a producer kind, §8).

Any resume workflow is expressed with these primitives; none is baked into the binary.

### 7.1 recover() is a pure planner

`recover()` is a pure function: durable facts in, a `RecoveryPlan` out. It runs on every
daemon start. Because the job-owning unit is always local (§9), recover() plans over local
facts only. Its invariants:

- **witness_lsn reconciliation.** `witness_lsn := the witness seq`, the sole monotone truth.
- **ACK-gated retry.** Only acked work is considered durable.
- **lease-epoch zombie fencing.** A stale epoch cannot resurrect a dead lease.
- **undeleted-row re-presentation** — never replay.
- **bounded requeue.**

The task database is rebuilt from the acked witness `seq` plus `events/`, so a crash in the
post-ack window loses nothing durable.

### 7.2 Pool-return and auto-resume

Pool reachability is monitored with hysteresis: `pool-vanished` is set only after N
consecutive failed probes, and pool-return is confirmed only after a sustained
transition — never a single blip, avoiding the false-positive double-run.

On a confirmed pool-return, recover() **re-presents the existing durable `pool-vanished`
row** — row re-presentation, never origination. This is gated behind a per-pool
`autoResume` flag, which defaults **ON** for resource-loss. Dedup-by-existence plus epoch
fencing guard against a double-run if a blip were ever misclassified.

A separate, purely advisory assessor (task-0) may write an **attestation** line on return
via the pool-reachability producer's `onReturnAttest` field. task-0 carries the per-leaf
`noEnqueue` capability and never fans out; the resumed job is reached by the row
re-presentation, not by task-0 enqueuing anything.

### 7.3 Coordinator-switch recovery

A nightly `nixos-rebuild switch` restarts the daemon; recover() fires. Local `--user`
transient units are re-adopted via `systemctl --user show` — no net-new mechanism for local
units.

Cross-host leases are a **distinct** path. During the switch the coordinator is down for
seconds and a `RemoteLease` heartbeat lapses; without protection a worker's reaper would
regrant its single-capacity lease while the coordinator's `--collect`-kept leaf still holds
VRAM. Therefore:

- The worker holds a **reaped-but-not-regranted** lease through an epoch-keyed grace window.
- On restart the coordinator re-adopts by presenting its **bumped `lease_epoch`** — bumped
  on every daemon start, graceful switch included, not only on crash.
- The worker refuses to regrant a lease whose adopted leaf is provably still live
  (boot-id-style epoch fencing).

### 7.4 The transactional core boundary

The fsync-before-ack transactional core touches **exactly** three things: the admission
decision, the lease grant, and the verdict-witness `fsync`. The taskchampion replica commit
is **outside** the barrier (post-ack) and is crash-safe because the replica is a rebuildable
cache.

Consequently a slow SQLite fault (WAL checkpoint, viewer lock) degrades to
queued-not-stalled: the socket keeps accepting, the merged process never crash-loops under
`WatchdogSec`, and no acked work is lost.

Lease liveness for **local** leases is systemd unit-liveness (`unit-exit/` reconciliation +
`systemctl show`) — there is no bespoke heartbeat. An actual heartbeat, with
`remoteHeartbeatSec`/`remoteReapSec` timeouts, exists **only** for the cross-host
`RemoteLease` path.

---

## 8. Producers — the intake registry

All work enters through one kind-tagged registry, `producers.<name>`, with a `kind`
discriminator dispatching to a per-kind submodule. Every producer kind emits an `events/`
record; `events/` is trigger-only ingress that becomes an ordinary in-daemon enqueue.

`kind ∈ { calendar, build-effect, pool-reachability, gh, events-dir, r2 }`

- **calendar** — time-window intake.
- **build-effect** — a Nix build outcome triggers work, exactly-once-per-key. Watched via
  `watch ∈ { gc-roots-dir, jsonl, post-build-hook }`.
- **pool-reachability** — the recovery probe of §7, carrying `onReturnAttest`.
- **gh** — GitHub intake, complete with the mutation half; `actorExclude = "self"` by
  default and `sources` enforced.
- **events-dir** — the directory-drain sensor. Default `pollIntervalSec = 60`.
- **r2** — R2 object intake.

The GitHub, calendar, and R2 sources are peers feeding the one queue — sensors, not
privileged control paths.

**Hercules parity is bounded.** tally takes the trigger class only: build→effect with
exactly-once-per-key. It refuses, in this specification, the containerized effect sandbox,
the state-file effect API, and any coordination plane beyond the resource box. Offline-first.

---

## 9. Execution locus

A job-owning transient unit is **always coordinator-local**. Even an ssh-exec-bridge leaf
that runs OCR on a worker is owned by a coordinator-side transient unit that shells to the
worker. The worker owns **only** the VRAM cgroup and the lease token — never a spawned tally
job.

This is what keeps recover() a pure local-facts planner (§7.1) and what makes the remote
enforcement negotiation of §4.5 necessary.

---

## 10. Jobs enqueuing jobs — the one-hop shape

The load-bearing law is only that the **daemon** never **originates** work. Jobs **may**
enqueue jobs — this is exactly the OCR-firehose workload (a research job that discovers
twelve papers enqueues twelve OCR jobs).

The admission path applies **server-side guardrails** to a job-originated enqueue, detected
via the `TALLY_JOB_ID` the caller carries:

- A mandatory `dedupKey`.
- A per-parent fan-out cap.
- A depth cap (default `3`).
- The gh actor-exclude rule.
- `--parent` auto-stamped from `TALLY_JOB_ID`, so every job-originated enqueue is audited.

When an enqueue payload and its producer both specify pool/priority/adapter, the **enqueue
payload wins**.

The "must never enqueue" constraint is **not** global. It is a per-adapter/per-producer
`noEnqueue` capability flag, carried only by the specific advisory recovery leaf (task-0,
§7.2). "One-hop" names that advisory workflow's shape — an assessor that may not fan out —
not an admission ban.

---

## 11. Adapters — declarative harnesses

Harnesses are declared as `adapters.<name>` in Nix. The former closed agent-kind union
(`pi`, `claude-code`, `shell`) is inverted into open declarative adapters. There are no
`conductor`/`receiver` roles; those are emergent.

An adapter is:

- A flat `argv` launch line.
- A nullable `resume` template (multi-variable) — the vehicle for resuming a re-presented
  job.
- A `scrape` block.
- An optional `yieldHook` (§6.3).
- `extraConfig`.

There is no shell-invocation adapter variant; the no-shell invariant is preserved.

### 11.1 The scrape envelope

`scrape.<captureName>` supports N named captures, a `jsonPath` extraction mode for harnesses
emitting structured JSON (e.g. claude-code), and a `stream` selector (`stdout` | `stderr`).
Scrape reads the per-job captured stream (§3.5), so a detached unit's output is available;
this is what makes `scrape.sessionRef` and adapter `resume()` work.

The expressible envelope is bounded and honest: **argv-launch + N-capture-scrape +
templated multi-variable resume** is in scope. Approval-gated and streaming-interactive
harnesses are **out** of scope and named as such. Model identifiers within a harness's
output are recorded verbatim; there is no normalization.

---

## 12. The Nix module layer

The module layer is pure Nix. It declares:

- `pools.<name>` — resources, capacity, `predicate`, `enforce`, `remote`, `servingSlice`,
  `credentials`, budgets (`budgetGb`, `consumptionCap`).
- `producers.<name>` — the kind-tagged intake registry (§8).
- `adapters.<name>` — declarative harnesses (§11).

`nixosModules.tally` auto-wires `Delegate=yes` and dmem `subtree_control` whenever any
declared pool sets `enforce = "dmem"`. Configuration is validated at build time by
`tally --mode check-config`, which shares the single canonical enum definitions with the
runtime.

---

## 13. CLI surface

The surviving frozen CLI surface is byte-stable. The additive surface is:

- `tally witness append` — the append verb (the exported wrapper helper name is `emit`;
  see the `tally-witness-emit` store path), writing the **attestation** chain.
- `tally witness verify` — verifies a ledger offline.
- `tally lease acquire` / `tally lease release` — the lease binding that replaces the cut
  `--session` binding.
- `tally daemon run` — runs the daemon.
- `tally --mode check-config` — the build-time configuration validator.
- The build→effect producer kind (§8).

The cut surface — the session/pane/agent detector groups, `--session`, `kitty_window_id`,
`pane_id`, the pub/sub broadcast seam — is gone. journald is the delta stream
(`journalctl -t tally -o json -f`); the socket carries only request/response RPC (CLI,
lease negotiation, barriers/wait-groups). `session_ref` survives only as an opaque scraped
string.

---

## 14. Acceptance & conformance

The dominant acceptance test: **the Rust `tally witness verify` passes, byte-for-byte, on a
ledger written by the reference implementation used as the golden-test oracle.** The
canonical verdict chain is preserved exactly for this reason.

The golden oracle diffs only surviving verbs and frames; cut surface is validated by
absence. Behavior that has no oracle counterpart — the cooperative-yield timing, the fleet
lease negotiation — is covered by dedicated conformance tests (e.g. a low-priority holder
yields within N seconds of an `interrupt`-tier admission; the socket keeps accepting under
an injected slow-SQLite fault).
