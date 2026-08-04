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
  lifecycleHorizon = "30d";
  lifecycleMaxBytes = 268435456;
  onCalendar = "daily";
};

services.tally.storage = {
  pollIntervalSec = 60;
  dataDir = {
    warningBytes = 34359738368;
    hardBytes = 68719476736;
    warningFreeBytes = 17179869184;
    minimumFreeBytes = 8589934592;
  };
  stateDir = {
    warningBytes = 34359738368;
    hardBytes = 68719476736;
    warningFreeBytes = 17179869184;
    minimumFreeBytes = 8589934592;
  };
};
```

`minimumFreeBytes` is the hard admission floor; `warningFreeBytes` must be larger and is the
early, durable warning threshold. Size these from observed `growthPerCompletion`, not merely from
the current store size. The gap from warning to hard should cover at least the maximum
completion growth possible during `storage.pollIntervalSec`, plus every already-admitted job
that may still finish after intake closes and an operator-response margin. The 16 GiB/8 GiB
defaults replace the former 256 MiB floor, which represented only a few pathological replica
rewrites. Lower them only when the largest admitted wave and measured write amplification fit
comfortably below the replacement floor.

**Upgrade consideration — the free-space floor moved from 256 MiB to 8 GiB.**
The defaults changed, not just the documentation. `minimumFreeBytes` went from 256 MiB to
8 GiB (`8589934592`) and `warningFreeBytes` to 16 GiB (`17179869184`). An existing deployment
that never set these options inherits the new floor at its first daemon start after the
upgrade.

On a host with less than 8 GiB free that is an immediate, total intake refusal. It is legible
— `tally query storage` and the refusal reason on every rejected submission name the observed
available bytes and the minimum, and already-admitted work is left to finish — but nothing
else announces it, so it arrives as a surprise on a constrained host that was running fine
under the 256 MiB floor.

Check available free space on the `dataDir` and `stateDir` filesystems before upgrading. If it
is below the new floor, either free space or set the options explicitly on **both** stores.
Intake closes when *either* store is Hard, so on a single-filesystem host — the usual case —
lowering only `dataDir` leaves intake refused through `stateDir`:

```nix
services.tally.storage = {
  dataDir = {
    warningFreeBytes = 2147483648;   # 2 GiB
    minimumFreeBytes = 1073741824;   # 1 GiB
  };
  stateDir = {
    warningFreeBytes = 2147483648;   # 2 GiB
    minimumFreeBytes = 1073741824;   # 1 GiB
  };
};
```

Size any replacement from the guidance above rather than from the old default. Note also that
recovery is hysteretic: once intake closes, it reopens only when availability rises at least
1 GiB above the configured floor, so restoring intake takes more free space than the floor
alone suggests.

The timer runs this command:

```console
$ tally gc --horizon 30d --collect \
    --capture-archive-horizon 30d \
    --events-done-horizon 180d \
    --events-rejected-horizon 30d \
    --events-rejected-max-count 10000 \
    --producer-marker-horizon 180d
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
7. prunes coordinator-side per-attempt capture archives, dead
   `<uuid>.capture.lock` files in both `capture-lock/` and the legacy
   `unit-exit/` location, retained `probe-*` commit-probe repositories under
   the given state directory's `adapter-smoke/`, and consumed/rejected
   producer-event files according to their independent policies; and
8. with `--collect`, runs `nix store gc`.

A brief file whose bytes do not verify against the hash in its own name is
counted as `briefsUnverified` (or `legacyBriefsUnverified`) and skipped. It is
never pruned and never renamed: it is unaddressable, so it cannot satisfy any
live brief hash, and it is the one case the sweep cannot parse well enough to
act on. Removing it is an operator decision. A nonzero count is the signal —
it is reported on every run, and it no longer aborts the sweep before the
state-directory and projection pruners the way propagating the verification
error did.

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
| `flow-lineage.jsonl` | Latest 100,000 rollover records; the append that would cross the bound rewrites the file, keeping the newest | Automatic, and safe precisely because this store is an index rather than a proof chain. A compacted-away record no longer refuses replay of the run it retired — that run is 100,000 generations old, and its own rows and witnesses are long gone. Never hand-edit it while the daemon is running; repair a damaged line with the daemon stopped. |
| `flow-membership.jsonl` | Latest 20,000 run-membership records — one per admitted flow node. The append that crosses the bound rewrites the file down to 18,000, dropping **whole runs** least-recently-touched first; the ~2,000 records of headroom mean the next rewrite is thousands of admissions away rather than the very next one | Automatic, and deliberately conservative about what it will drop. Whole runs rather than individual records, because a half-present run would report a membership count lower than the truth. Least-recently-*touched* rather than oldest-born, because a run's first record ages while the run is still working — keying on birth would evict exactly the campaigns still under observation. Never a run holding an executing task, and never the run whose record is being written; if nothing is evictable the ledger exceeds its target rather than deleting membership that is in use, and says so on the daemon journal. A run that is dropped falls back to the durable-row scan — the pre-#380 answer — which for a row-less (`attached`/`reused`/`terminal`) node is nothing, which is why eviction stays away from live runs. The bound is sized by the one-time parse (~200 ms at 20,000 records) and resident index rather than by disk. Never hand-edit it while the daemon is running; repair a damaged line with the daemon stopped. |
| `changes.jsonl` | Latest 4,096 change records | Automatic; invalid or foreign contents reset to an empty feed at startup, and slow readers receive `cursor-expired`. |
| `<dataDir>/briefs` | Live attempts plus witnesses inside `retention.horizon`; hashes remain durable after bytes expire | Automatic through `tally gc`; enqueue fresh work when an expired failed job can no longer be retried. |
| Current captures | One `.out` and raw `.adapter.err` generation per task identity; a failed generation also has a bounded `.err` projection of at most 2 KiB | Do not remove active-generation files. `.adapter.err` is the sole byte-authoritative stderr stream. |
| Archived captures | Per-attempt `.out` and raw `.adapter.err`; failed attempts may include the bounded `.err` projection | Coordinator `tally gc` prunes files older than `captureArchiveHorizon` (30 days by default). Witnesses do not pin them. Remote-worker state needs its own policy. |
| Worker `stateDir` | Captures, launch markers, exit records, and execution attestations accumulate | Preserve live/ambiguous generations. No worker-side GC is shipped. |
| Ordinary `artifact:<path>` files | Owned by the workload; no tally GC root | Apply a workload-specific policy only after accepting the reuse and audit consequences below. |
| Producer events | Pending files are durable recovery inputs; consumed `events/done` defaults to 180 days, rejected files to 30 days/10,000 | Let `tally gc` prune only the managed done/rejected sets. |
| Producer markers (`producers/gh-triggers`, `gh-completed`, `gh-comments`, `gh-storage-warnings`, `gh-orphaned`) | One `<key>.json` per dispatch; the first four make a forge mutation idempotent, `gh-orphaned` records a projection that can never be applied. All expire under `producerMarkerHorizon` (180 days by default) | Automatic through `tally gc`. Collecting an idempotency marker costs at most a re-publication that the thread's own marker scan already collapses. A `gh-orphaned` record guards nothing and is read only by the startup report and `tally producer orphaned`; it can only reach the horizon after the acknowledged event it describes has left `events/done`, so collecting one does not resurrect it. A per-marker `<key>.lock` is collected only together with its marker and only when no writer holds it; the directory-wide `mutations.lock` is never collected. |
| Inert `taskdata/` and `taskdata.pre-rebuild-*` directories | Left in place when the live task-database projection was deleted; no pruner reads or writes them | Nothing depends on them. Delete them by hand to reclaim the space. |
| Unit-exit state | Durable recovery input; no general pruner. One exception: legacy `<uuid>.capture.lock` files expire under `captureArchiveHorizon` | Do not prune exit records or `<uuid>.capture.json` generations by age. `tally gc` removes only a `.capture.lock` that is both older than the horizon and unheld, proven by a non-blocking `flock` it takes before unlinking. Nothing creates a lock here any more, so this population only drains. |
| Retained commit probes (`<gc --state-dir>/adapter-smoke/probe-*`) | One throwaway git repository per failed `tally adapter smoke --assert-commit`; expires under `captureArchiveHorizon` | A verified probe deletes itself; a failed one is the evidence and is kept. `tally gc` removes only `probe-*` directories older than the horizon, and only under the `--state-dir` it is given, so run the smoke with `--state-dir` naming the same directory. It reports `adapterProbesExamined`/`adapterProbesPruned`. Anything else under `adapter-smoke/`, and any probe seeded elsewhere via `--probe-root`, is left for an operator. |
| Capture locks (`capture-lock/`) | One `<uuid>.capture.lock` per dispatched execution; expires under `captureArchiveHorizon` | Same two-check rule as above. The daemon no longer mints a lock for a task whose capture generation is already gone, so a swept lock stays swept instead of being re-created at the next startup reconcile. |
| In-memory barrier tracker | At most 64 unclaimed drain snapshots; connected waits scale with active calls, and disconnected waiters are evicted on the next tracker operation | Automatic and restart-local. |
| In-memory parent guardrails | Terminal parents retire after their outstanding-child count reaches zero | Automatic; rebuilt from active durable rows. |

### Reclaim an inert `taskdata/` projection

Hard pressure never launches GC implicitly. Retention horizons are evidence policy, and the
timer's `--collect` action is host-wide; a daemon must not silently shorten either policy merely
to admit another job.

Deleting the live task-database projection left any existing `<dataDir>/taskdata/` directory and its
`taskdata.pre-rebuild-*` archives on disk and inert. Nothing reads or writes them, no retention
lane sweeps them, and they still count against the data-store allocated-byte budget — on the
#252 incident host that inert tree is where the 270 GiB actually sits. Removing them is a manual
operator action:

```console
$ du -sh /var/lib/tally/data/taskdata /var/lib/tally/data/taskdata.pre-rebuild-*
$ rm -rf /var/lib/tally/data/taskdata /var/lib/tally/data/taskdata.pre-rebuild-*
```

The cached allocated-byte view records recovery on its next
single-flight sample (within `storage.pollIntervalSec` after the previous sample completes). The
free-space axis is rechecked on the next intake as well as on that sample; restart the daemon only
if an immediate fresh tree measurement is operationally necessary.

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
