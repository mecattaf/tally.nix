# Retention and Nix store evidence

Tally keeps canonical witness records append-only, but it does not keep every Nix store path
named by those records alive forever. Store evidence has an age-based retention policy with a
witness-liveness floor: a path referenced by any witness inside the configured horizon remains
rooted even when an older witness also references it.

This policy applies only to `store:<path>` evidence and derivation references. It does not delete
or rewrite `witness.jsonl`, lifecycle observations, captures, event history, or ordinary
`artifact:<path>` files. Those stores have their own growth and operating policies.

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

The roots directory is fixed by `dataDir`; it is not separately configurable. The command first
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
