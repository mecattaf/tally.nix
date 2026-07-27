# The witness ledger

The witness ledger is tally's canonical terminal history. It is one append-only
JSON-lines file named `witness.jsonl`, written by the coordinator. Current
records use `schemaVersion: 2` and `recordType: "verdict"`.

There are no epoch-named witness files, boundary records, dual record shapes,
or cross-epoch verification rules. If tally finds predecessor-format ledger
bytes or predecessor event state on first boot, it refuses to start and names
an archive-aside path. It does not interpret, convert, or delete that state.
After the operator moves it aside, the current ledger starts at an ordinary
genesis record.

Archived predecessor bytes remain forensic material only. No current command
reads or verifies them.

## What a record binds

A verdict record binds the terminal result to its task UUID, attempt and lease
epoch, ordered pool set, origin, executor and execution host, timing, labor
class, and any payload, brief, artifact, store, derivation, flow, completion,
charge, result-revision, or authorship facts that actually exist.

Optional facts are omitted when absent; canonical records do not use top-level
`null` placeholders. Known fields have a fixed order. Additive unknown fields
are permitted for forward evolution and remain part of the hashed raw JSON,
so an older verifier cannot silently discard them while checking the chain.
Durable row changes follow a separate ordered migration registry with literal
previous-version fixtures; old witness records are never rewritten.

Each record has an increasing `seq`, its predecessor's `prevHash`, and its own
`hash`. To compute the hash, tally serializes the complete canonical JSON with
the `hash` value cleared, then hashes those bytes with SHA-256. Verification
checks compact encoding, schema and field invariants, every record hash,
predecessor links, sequence order, duplicates, and gaps.

This is tamper evidence, not a signature system. The coordinator's
single-writer ledger is authoritative because tally's deployment and
acknowledgement boundary make it so, not because SHA-256 authenticates the
machine.

## Canonical facts and advisory attestations

Adapter scrape output—session reference, model, final message, usage, and
similar captures—is advisory. It is appended to a separate attestation chain
and may enrich queries after the terminal acknowledgement, but it cannot
declare a canonical verdict, artifact, charge, or evidence result.

When execution attestations are enabled, each executing host can also append an
independent `exec-attestations.jsonl` chain. The task UUID, attempt, and lease
epoch give coordinator and worker a common execution identity.
`tally witness compare` verifies the input chains and compares exit status,
artifact hashes, store paths, and payload identity. It can expose agreement,
missing attestations, or self-consistent divergence. Those chains remain
explicitly `unauthenticated-by-construction`.

## Views can be rebuilt

TaskChampion and journal-backed query fields are projections, not the ledger.
With the daemon stopped, `tally view rebuild --yes` reconstructs TaskChampion
from acknowledged durable events and the verified current witness chain,
moving an existing projection aside first. Losing or changing that view does
not change canonical verdict history.

The record builder, validator, append lock/fsync, clean-cut refusal, and all
chain verification live in `crates/tally-core/src/witness.rs`. Query authority
joins live in `crates/tally-core/src/query_v2.rs`, and view reconstruction lives
in `crates/tally-core/src/view.rs`. Tests
`ledger_fixture_is_green_and_tamper_is_red`,
`unknown_additive_fields_verify_and_round_trip_in_raw_order`,
`old_format_is_red_and_open_returns_an_actionable_archive_error`, and
`wrapper_and_compare_distinguish_chain_tamper_from_self_consistent_divergence`
cover the important failure modes. The exact field contract is in
[Witness format and verification](../reference/witness-format.md).

Verify the canonical ledger and the advisory chains available on this host:

```console
$ tally witness verify --format json | jq '{ok, chains, execAttestations}'
```
