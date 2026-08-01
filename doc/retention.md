# Retention and Nix store evidence

Tally keeps canonical witness records append-only, but it does not keep every Nix store path
named by those records alive forever. Store evidence has an age-based retention policy with a
witness-liveness floor: a path referenced by any witness inside the configured horizon remains
rooted even when an older witness also references it.

The Nix-root portion of this policy applies only to `store:<path>` evidence and derivation
references. The same sweep also owns structured brief bytes, capture archives, and consumed
producer ingress. It does not delete or rewrite `witness.jsonl`, lifecycle observations, durable
enqueue rows, or ordinary `artifact:<path>` files. Those stores have their own growth and
operating policies.

## What happens when a witness is appended

The daemon validates every `store:` item with `nix-store --check-validity`. A passing witness
records the paths in a sorted, unique `storePaths` array. A derivation witness also names its
derivation path and output paths in `drv`.

After the witness has been durably appended, the daemon registers each referenced path as an
indirect Nix GC root beneath:

```text
<dataDir>/gcroots/witness-<sequence>/<store basename>
```

Registration uses `nix-store --add-root <link> --realise <path>`. A registration failure is
reported as `tally: gcroot registration failed …`; it never changes the already-written witness.
At startup, the daemon verifies the ledger and retries registration for witnesses inside the
retention horizon. Witness append/registration and garbage collection share a lock, so collection
cannot take a stale live-set snapshot while a new witness is being committed.

## Structured brief bytes

Every admitted brief has one canonical copy under `<dataDir>/briefs/<sha256>.json`. Producers
write directly into that store and publish only the path in ingress; the daemon verifies and
reuses the same content-addressed file instead of making a second copy under `stateDir`.

The retention horizon is also the replay window for those bytes. `tally gc` retains a brief when
an acknowledged durable row has an unwitnessed current attempt, or when a witness carrying that
`briefHash` falls inside `now - horizon`. It removes unreferenced and older-terminal brief bytes;
the durable row and witness keep the hash, so identity and proof remain intact. Retrying a failed
job after its brief bytes have expired is rejected explicitly and the work must be enqueued fresh.

Brief admission holds a shared brief-store lock from materialization through durable publication.
GC takes the exclusive side before reading the witness and row sets, preventing collection from
acting on a stale snapshot. The sweep also recognizes the legacy `<stateDir>/briefs` location
created by tally 0.1.0 after #250: live/recent files and files younger than the horizon stay, while
older duplicates and orphans are removed. New producer dispatches write no state-directory copy.
`--skip-state-dir` skips brief collection because the durable row live set is unavailable.

## Scheduled retention

The Home Manager module exposes exactly these options:

```nix
services.tally.retention = {
  enable = true;          # default
  horizon = "30d";        # default; systemd-style timespan
  onCalendar = "daily";   # default
};
```

When enabled, a user timer runs:

```console
$ tally gc --horizon 30d --collect
```

The roots and canonical brief directories are fixed by `dataDir`; they are not separately configurable. The command first
verifies the complete witness chain. If verification fails, it leaves every root untouched and
does not collect the store. Otherwise it computes the set of paths referenced by witnesses whose
`transitionTimestamp` is at or after `now - horizon`. On a mutating run it retries those live root
registrations and fails closed before pruning if any cannot be secured. It removes an older
witness's root link only when that link's target is absent from the live set. Finally, `--collect`
runs `nix store gc`. The ledger itself is never edited.

Use `--dry-run` to inspect the counts without removing links or invoking Nix garbage collection:

```console
$ tally gc --horizon 30d --dry-run --collect
```

Because `nix store gc` is host-wide, it can collect any unrooted store object, not only objects
previously rooted by tally. Run the dry form first when changing the horizon, and account for the
host's other GC roots.

## Consequences for reuse

Store-backed reuse is intentionally cheap: tally checks that every declared path is still valid
and that the declared set exactly matches the governing witness's `storePaths`. Once retention and
Nix GC collect a path, this check becomes a normal dedup miss and the work runs fresh. Tally never
reconstructs a store path from witness bytes and never substitutes byte hashing for Nix validity.

For a flow `drv()` node, the declared derivation and outputs become the witness's `drv` and
`storePaths` fields. The node uses `drv:<drvPath>` as its cross-run dedup key and derives its
submitted task UUID from the flow-local run ID and ordinal. The latter is stable on replay but
distinct in a later flow run, so each run gets its own cheap witness. If all outputs are valid, the
daemon emits a `substituted` witness without admitting a row or leasing the reserved `build` pool.
Otherwise it admits `nix build --no-link <drvPath>^*` under that pool, then applies the same store
validation and root-registration rules.

`build-effect` remains an observation surface rather than an implicit callback from witness
append. To trigger downstream work for locally built outputs, configure the host's Nix
`post-build-hook` to write the stream watched by a `build-effect` producer (directly as
`post-build-hook`, or through its `jsonl` format). A substitution does not pretend that a build
occurred and therefore does not synthesize a hook entry; the cheap witness is the record of that
path through the flow.

For cross-host durability, publish store objects explicitly from a flow with an `attic push`
`sh()` step. There is no executor post-run push hook: cache publication remains visible work in
the flow rather than a hidden data plane.

## State-store pressure and recovery

The daemon samples `dataDir` and `stateDir` off-thread at the configured storage cadence. Each
store has allocated-byte warning/hard limits plus `warningFreeBytes` and `minimumFreeBytes` for
the filesystem that contains it. Queries use the cached tree sample. Every enqueue rechecks only
the cheap filesystem-free value before deciding admission; it never walks either tree. Hard size
or free-space pressure refuses only new intake. A measurement failure uses the distinct
`storage-monitor-unavailable` refusal.

The defaults warn below 16 GiB free and refuse below 8 GiB. Budget recovery is hysteretic (90%
for size thresholds; a free-space threshold plus the larger of 10% or 1 GiB), and warning/hard
severity changes share one campaign-receipt episode until full recovery. `storage-metrics.json`
is derived advisory state: invalid, foreign, inconsistent, or unsupported versions reset at
startup while the durable warning log preserves the sequence high-water.

Hard pressure does not override retention policy. Deleting the TaskChampion projection left any
existing `taskdata/` directory and its `taskdata.pre-rebuild-*` archives inert: no retention lane
sweeps them, but they still count against the data-store byte budget. Removing them is a manual
operator action:

```console
$ rm -rf /var/lib/tally/data/taskdata /var/lib/tally/data/taskdata.pre-rebuild-*
```

`tally gc` omits `--collect` by default, so an ordinary sweep does not launch host-wide Nix GC.
Symlink targets such as
GC-root-pinned Nix closures are not charged to the directory byte budget, and campaign
`workspaceRoot` trees are workload-owned. Monitor those external lanes separately; the
free-space floor covers them only when they share the tally store's filesystem.
