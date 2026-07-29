# Retention and growth

tally has artifact garbage collection now, but only for Nix store evidence. It
does not have a general retention engine.

`store:<path>` evidence and `drv()` records receive Nix GC roots. The Home
Manager and NixOS timers drop expired roots and invoke Nix garbage collection.
Ordinary `artifact:<path>` files, captures, lifecycle observations, enqueue
events, and the ledgers are outside that mechanism.

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
  onCalendar = "daily";
};
```

The timer runs this command:

```console
$ tally gc --horizon 30d --collect
```

The NixOS service runs the same command with its configured `--data-dir`
(default `/var/lib/tally/data`) under the dedicated service account. The Home
Manager service uses its configured XDG data directory. Both timers are enabled
by default.

The GC command:

1. parses the systemd-style horizon;
2. takes the exclusive GC-root lock;
3. verifies the complete witness chain;
4. builds a live set from every witness whose `transitionTimestamp` is at or
   after `now - horizon`;
5. on a mutating run, re-registers every live root and stops before pruning if
   one cannot be secured;
6. removes an expired witness's root only when its target is absent from the
   live set; and
7. with `--collect`, runs `nix store gc`.

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
| `lifecycle.jsonl` | Explicit policy string `unbounded` | No supported compaction command exists. Do not hand-edit it. |
| `changes.jsonl` | Latest 4,096 change records | Automatic; invalid or foreign contents reset to an empty feed at startup, and slow readers receive `cursor-expired`. |
| Current and archived captures | Files accumulate per execution generation; query reads at most 16 MiB | Do not remove active-generation files. Archiving old captures sacrifices trace and scrape reconstruction. |
| Worker `stateDir` | Captures, launch markers, exit records, and execution attestations accumulate | Preserve live/ambiguous generations. No worker-side GC is shipped. |
| Ordinary `artifact:<path>` files | Owned by the workload; no tally GC root | Apply a workload-specific policy only after accepting the reuse and audit consequences below. |
| Enqueue events and unit-exit state | Durable recovery inputs; no general pruner | Do not prune by age. |
| In-memory barrier tracker | At most 64 unclaimed drain snapshots; connected waits scale with active calls, and disconnected waiters are evicted on the next tracker operation | Automatic and restart-local. |
| In-memory parent guardrails | Terminal parents retire after their outstanding-child count reaches zero | Automatic; rebuilt from active durable rows. |

Use ordinary filesystem accounting to locate growth:

```console
$ du -sh "$XDG_DATA_HOME/tally" "$XDG_STATE_HOME/tally"
$ du -sh "$XDG_STATE_HOME/tally"/capture/*
```

On the NixOS system module, inspect `/var/lib/tally/data` and
`/var/lib/tally/state` instead. Inspect each remote executor's configured
worker `stateDir` separately.

There is no sanctioned command that compacts lifecycle history, captures,
enqueue events, and exit records together. Under space pressure, quiesce the
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
