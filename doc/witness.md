# Witness records: facts outlive encodings

This chapter records the ruling made in
[issue #74](https://github.com/mecattaf/tally.nix/issues/74) and put into final form by
[issue #84](https://github.com/mecattaf/tally.nix/issues/84), including its two amendments.
Immutability protects facts, not an early encoding chosen before tally found its current shape.

## The ruling

Tally has one canonical witness schema and one canonical ledger, `witness.jsonl`. Records carry
`schemaVersion: 2` as ordinary forward-evolution metadata. There are no epoch-named files, dual
record types, cross-epoch references, boundary records, or parallel verification paths in the
implementation.

The clean cut was deliberate:

- `witness.rs` was rewritten in place around the final camelCase record shape, required native
  provenance, native pool arrays, store evidence, derivation results, and authorship binding.
- The daemon emits only that shape. `tally witness verify` verifies only that shape.
- A first boot over predecessor ledger bytes or a non-empty predecessor events directory fails
  closed with an actionable archive-aside error. Tally does not interpret, convert, or delete
  those bytes.
- Once the operator moves predecessor state to the path named by that error, the current ledger
  starts as an ordinary genesis chain at sequence 1.

Archived predecessor state is inert. It is retained as operator-owned forensic material, but no
current tally command reads it and verifying it is not a supported operation. If the obsolete
reader were ever genuinely needed, its implementation remains available in Git history. This is
a permanent clean-cut rule, not a deferred migration.

## Canon and attestations

The encoding changed; the authority model did not. The coordinator's verdict ledger is the
single-writer canonical proof. Adapter scrape attestations and per-executing-host
`exec-attestations.jsonl` chains remain advisory and unauthenticated by construction.

Execution attestations use the task UUID, attempt, and lease epoch to derive a common execution
identity independently on the coordinator and worker. `tally witness compare` verifies every
input chain before comparing exit status, artifact hashes, store paths, and payload identity.
This makes honest cross-machine agreement and divergence visible without pretending that an
unsigned advisory log is an identity system.

## Forward schema evolution

History written after the clean cut matters. Witness records are never rewritten, and the
durable-row migration registry carries the repository's schema-evolution law:

> Any field a canonicalizer can derive must be tolerated absent on disk. Every durable-schema
> change ships as an explicit, ordered, versioned migration with a literal previous-version
> fixture.

Unknown additive witness fields remain hash-covered through raw JSON verification. A change that
would alter existing canonical bytes or required-field meaning is not silently normalized into
old history.

## TaskChampion is a view

TaskChampion is a local projection for Taskwarrior-compatible access, not a source of truth. Its
storage format may change without a witness-schema ceremony. The authoritative inputs are the
acknowledged durable events under tally's state directory and the verified records in
`witness.jsonl`.

Rebuild the projection while the daemon is stopped:

```console
$ tally view rebuild --yes
{"rebuilt":true,"rows":42,"witnessRecords":37}
```

Use `--data-dir DIR` when rebuilding a non-default tally data directory. The durable events still
come from the normal tally state directory selected by `XDG_STATE_HOME` (or its default). Without
`--yes`, an existing projection requires interactive confirmation.

The command takes the same `daemon.lock` used by the daemon and refuses with an error naming that
path when the daemon or its replica writer still owns it. If `taskdata/` exists, the command moves
it to `taskdata.pre-rebuild-<RFC3339 timestamp>` before constructing a new projection; it never
deletes the old replica. The rebuild then verifies the complete current witness chain and projects
all acknowledged rows, terminal statuses, attempts, lease epochs, and labor classes from durable
facts. Its `witnessRecords` count is a single plain-schema count, not an epoch map.

Store-path lifetime is independent of this disposable projection. See
[Retention and Nix store evidence](retention.md) for the witness-liveness floor and GC-root rules.
