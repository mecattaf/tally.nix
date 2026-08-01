# Retention and growth

tally has targeted retention for Nix store evidence, structured brief bytes,
per-attempt capture archives, and consumed/rejected producer events. It does
not have a general artifact retention engine.

`store:<path>` evidence and `drv()` records receive Nix GC roots. The Home
Manager and NixOS timers drop expired roots and invoke Nix garbage collection.
Ordinary `artifact:<path>` files, lifecycle observations, durable enqueue rows,
current captures, and the ledgers are outside the age-based pruners and Nix-root
mechanism.

Structured briefs have one canonical copy under `<dataDir>/briefs`. Producers
write there directly; daemon admission verifies and reuses that file. The GC
horizon retains bytes referenced by an unwitnessed attempt or a recent witness,
then prunes unreferenced and older-terminal bytes while preserving `briefHash`
in durable rows and witnesses. The same sweep ages out legacy duplicate/orphan
copies under `<stateDir>/briefs`. Admission and GC share a brief-store lock, so
the live-row snapshot cannot race a new producer or daemon admission.

## The managed path: Nix store evidence

When a passing witness is durably appended, the daemon takes a shared
registration lock and registers a GC-root symlink for every path in the
witness's `storePaths` array. A derivation witness also roots its derivation
path and all output paths. The layout is fixed:

```text
<dataDir>/gcroots/witness-<sequence>/<store-basename>
```

Registration runs:

```console
$ nix-store --add-root <link> --realise <store-path>
```

The root directory is derived from `dataDir`; it is not independently
configurable. Startup verifies the ledger and retries root registration for
witnesses inside the current horizon.

Witness append wins over root registration. If registration fails after the
witness was written, tally keeps the canonical witness and logs the exact
failure:

```text
tally: gcroot registration failed for witness <seq> path <path> link <link>: <reason>
```

Treat that log as an incident. Until startup or `tally gc` secures the live
root, an unrelated host-wide Nix GC can collect the path.

## Configure the horizon

Both the Home Manager and NixOS modules render the retention service and timer:

```nix
services.tally.retention = {
  enable = true;
  horizon = "30d";
  captureArchiveHorizon = "30d";
  eventsDoneHorizon = "180d";
  eventsRejectedHorizon = "30d";
  eventsRejectedMaxCount = 10000;
  projectionArchiveHorizon = "30d";
  lifecycleHorizon = "30d";
  lifecycleMaxBytes = 268435456;
  onCalendar = "daily";
};

services.tally.storage = {
  pollIntervalSec = 60;
  dataDir = {
    warningBytes = 34359738368;
    hardBytes = 68719476736;
    minimumFreeBytes = 268435456;
  };
  stateDir = {
    warningBytes = 34359738368;
    hardBytes = 68719476736;
    minimumFreeBytes = 268435456;
  };
};
```

The timer runs this command:

```console
$ tally gc --horizon 30d --collect \
    --capture-archive-horizon 30d \
    --events-done-horizon 180d \
    --events-rejected-horizon 30d \
    --events-rejected-max-count 10000 \
    --projection-archive-horizon 30d
```

The NixOS service runs the same command with its configured `--data-dir`
(default `/var/lib/tally/data`) under the dedicated service account. The Home
Manager service uses its configured XDG data directory. Both timers are enabled
by default.

The GC command:

1. parses the systemd-style horizon;
2. takes the exclusive brief-store lock and then the GC-root lock;
3. verifies the complete witness chain and acknowledged durable rows;
4. builds live sets for structured briefs and every store path whose witness
   `transitionTimestamp` is at or
   after `now - horizon`;
5. on a mutating run, re-registers every live root and stops before pruning if
   one cannot be secured;
6. removes expired brief bytes and an expired witness's root only when absent
   from the corresponding live set;
7. prunes coordinator-side per-attempt capture archives, consumed/rejected
   producer-event files, and immutable `taskdata.pre-rebuild-*` projection archives according to
   their independent policies; and
8. with `--collect`, runs `nix store gc`.

The daemon separately checks `lifecycle.jsonl` at the storage poll cadence. Once it exceeds
`lifecycleMaxBytes`, tally rewrites only the contiguous prefix older than `lifecycleHorizon` and
records the exact truncation boundary. If every record is recent, the log remains above the byte
trigger rather than discarding recent observability. `retention.enable = false` disables this
automatic compaction as well as the timer. The offline `tally history compact` command remains an
explicit maintenance path.

The witness ledger is never rewritten. A recent witness therefore forms a
liveness floor even when an older witness names the same path.

Inspect the result before changing policy:

```console
$ tally witness verify
$ tally gc --horizon 30d --dry-run --collect
{"horizon":"30d","dryRun":true,"collectRequested":true,"livePaths":…,"rootsExamined":…,"rootsPruned":…,"rootDirectoriesPruned":…,"collected":false}
```

Then run the mutating form:

```console
$ tally gc --horizon 30d --collect
```

`--dry-run --collect` neither removes links nor calls Nix GC. It reports what
the full command would do.

### The host-wide consequence

`nix store gc` is host-wide. It can collect any unrooted store object, not just
objects previously rooted by tally. In particular, the default-on Home Manager
and NixOS timers invoke it even when tally has no store-evidence roots. That run
may still collect unrelated unrooted Nix objects, so “no tally roots” does not
mean a literal no-op in the shipped implementation.

Account for the host's other roots, run the dry form before shortening the
horizon, and disable the timer if host-wide daily GC is not acceptable.

## Reuse after collection

Collection does not invalidate or edit an old witness. It changes whether the
referenced bytes are still available:

- A full-mode submission whose governing `storePaths` are no longer valid
  discloses `reusedRejected: "store-path-invalid"` and creates fresh work.
- A `drv()` node can substitute or build the derivation outputs again. A valid
  result gets a new witnessed path through the flow.
- A witness remains hash-chain verifiable even when the external store object
  it names is gone. Verification proves the record, not permanent
  availability.

Do not manually remove a root inside the live horizon. If the target is already
gone, the next mutating GC reports:

```text
cannot secure live GC root for witness <seq> path <path>: <reason>; expired roots were left untouched
```

That failure is deliberately before pruning.

## Cross-host store paths use Attic explicitly

tally moves no bytes between hosts. The exercised Attic handoff is visible
flow work:

1. a worker creates a Nix store path;
2. an explicit `sh()` node runs `attic push`;
3. Git carries only the store-path metadata to the coordinator; and
4. the coordinator realises the path through its configured substituter.

There is no adapter or executor post-run push hook. The `flow-multi-host` VM
check exercises this Attic path alongside an independent Git artifact handoff.

tally's root timer manages the local Nix store. It does not configure or prune
the Attic server's cache retention; that remains an Attic operator policy.

## What still grows

The current storage story is intentionally uneven:

| Store | Current bound or policy | Safe operator action |
|---|---|---|
| `witness.jsonl` | Append-only, unbounded | Never truncate; archive only as a complete, verified ledger during an explicit migration. |
| `attestations.jsonl` | Append-only, unbounded | Preserve if advisory history matters; it is a separate chain from verdicts. |
| `lifecycle.jsonl` | Byte-triggered prefix compaction; defaults to 256 MiB while preserving the newest 30 days | Set `lifecycleMaxBytes` and `lifecycleHorizon`; offline `tally history compact` remains available with the daemon stopped. |
| `changes.jsonl` | Latest 4,096 change records | Automatic; invalid or foreign contents reset to an empty feed at startup, and slow readers receive `cursor-expired`. |
| `<dataDir>/briefs` | Live attempts plus witnesses inside `retention.horizon`; hashes remain durable after bytes expire | Automatic through `tally gc`; enqueue fresh work when an expired failed job can no longer be retried. |
| Current captures | One `.out` and raw `.adapter.err` generation per task identity; a failed generation also has a bounded `.err` projection of at most 2 KiB | Do not remove active-generation files. `.adapter.err` is the sole byte-authoritative stderr stream. |
| Archived captures | Per-attempt `.out` and raw `.adapter.err`; failed attempts may include the bounded `.err` projection | Coordinator `tally gc` prunes files older than `captureArchiveHorizon` (30 days by default). Witnesses do not pin them. Remote-worker state needs its own policy. |
| Worker `stateDir` | Captures, launch markers, exit records, and execution attestations accumulate | Preserve live/ambiguous generations. No worker-side GC is shipped. |
| Ordinary `artifact:<path>` files | Owned by the workload; no tally GC root | Apply a workload-specific policy only after accepting the reuse and audit consequences below. |
| Producer events | Pending files are durable recovery inputs; consumed `events/done` defaults to 180 days, rejected files to 30 days/10,000 | Let `tally gc` prune only the managed done/rejected sets. |
| Active TaskChampion projection | No compaction in this feature; `query storage` exposes DB/WAL/SHM bytes and operation high-water, and the store hard budget gates new intake | Use the explicit offline `tally view rebuild`; issue #252 owns rebuilding efficiency. |
| `taskdata.pre-rebuild-*` archives | 30 days by default | Let `tally gc` prune only timestamp-valid immutable archive directories. |
| Unit-exit state | Durable recovery input; no general pruner | Do not prune by age. |
| In-memory barrier tracker | At most 64 unclaimed drain snapshots; connected waits scale with active calls, and disconnected waiters are evicted on the next tracker operation | Automatic and restart-local. |
| In-memory parent guardrails | Terminal parents retire after their outstanding-child count reaches zero | Automatic; rebuilt from active durable rows. |

### Recover from a hard projection-archive crossing

Hard pressure never launches GC implicitly. Retention horizons are evidence policy, and the
timer's `--collect` action is host-wide; a daemon must not silently shorten either policy merely
to admit another job.

An offline `tally view rebuild` deliberately leaves the former projection as a fresh
`taskdata.pre-rebuild-*` archive. If active projection plus that rollback copy crosses the data
budget, the default 30-day archive horizon will keep intake refused. Inspect the bounded archive
pass, explicitly accept losing those rollback copies, then run it without `--dry-run`:

```console
$ tally gc --horizon 30d --projection-archive-horizon 0s --skip-state-dir --dry-run
$ tally gc --horizon 30d --projection-archive-horizon 0s --skip-state-dir
```

Omitting `--collect` prevents this recovery pass from running host-wide Nix GC. It still applies
the chosen witness-root horizon, so use the configured `retention.horizon` in place of `30d`.
Only timestamp-valid, plain-tree projection archives are eligible; the active `taskdata`
directory is never touched. The cached storage view records recovery on its next sample (within
`storage.pollIntervalSec`); restart the daemon only if an immediate fresh sample is operationally
necessary.

### Ownership boundaries of the byte budgets

The data/state walks do not follow symlinks. In particular, a witness GC-root link is counted as
directory metadata, not as the target Nix store closure. That avoids crossing into a shared
global store and attributing deduplicated closure bytes to one daemon. The filesystem free-space
floor still sees Nix growth when `/nix/store` and the tally directory share a filesystem; when
they do not, inspect and retain the Nix store through its own GC-root policy.

Campaign `workspaceRoot` trees and ordinary `artifact:<path>` files are workload-owned, not tally
state, and are outside both directory budgets. Remote executor state is likewise measured on its
host, not by the coordinator. Give those lanes their own filesystem monitoring and retention
policy; do not raise tally's state-store budget as a substitute.

Use the daemon's self-metrics first, then ordinary filesystem accounting to locate individual
files:

```console
$ tally query storage
$ du -sh "$XDG_DATA_HOME/tally" "$XDG_STATE_HOME/tally"
$ du -sh "$XDG_STATE_HOME/tally"/capture/*
```

On the NixOS system module, inspect `/var/lib/tally/data` and
`/var/lib/tally/state` instead. Inspect each remote executor's configured
worker `stateDir` separately.

There is no sanctioned command that compacts every state class together.
Use `tally gc` for the managed archive/event sets and `tally history compact`
for lifecycle history. Under broader space pressure, quiesce the
coordinator, take a recoverable archive outside `stateDir`, and verify queries
and recovery before retiring that archive. Never present an ad-hoc `find
-mtime -delete` policy as tally GC.

## If an ordinary artifact is pruned

An `artifact:<absolute-path>` is not a Nix store object and receives no GC
root. If an operator or workload removes it:

- the existing witness and its recorded content hash remain in the chain;
- future full-mode reuse reports `artifact-unavailable` and runs fresh;
- changed bytes report `artifact-drift`;
- old provider trace or domain content cannot be reconstructed from the
  witness; and
- any external auditor that needs the original bytes has lost that evidence.

That may be a valid workload policy, but it is not transparent. Keep artifacts
for at least as long as their audit and reuse value, and use `store:` evidence
when content-addressed Nix retention is the intended lifecycle.

The implementation and its VM liveness proof are in
[`retention.rs`](https://github.com/mecattaf/tally.nix/blob/4c85563/crates/tally-core/src/retention.rs)
and the `retention-liveness-floor` flake check.
