# Final conformance bar

This directory is the executable companion to
`doc/final-conformance-bar.md`.  It tests a tally.nix working tree supplied by
the caller; the suite does not copy expectations from that tree and does not
write into it.

Run the whole bar with:

```console
test/final-bar/run /absolute/path/to/tally.nix
```

Use `--list` to print the stable case IDs and issue mapping.  `--case` may be
repeated for focused diagnosis.  A complete run writes `report.json` beneath
the directory named by `--artifacts`; without that option it uses a temporary
directory and prints the report before removing it.

Exit status is deliberately tri-state:

- `0`: every selected conformance assertion passed;
- `1`: the harness completed, but one or more desired-state assertions failed;
- `2`: at least one probe could not run or the harness/fixture was broken.

The long `parallel-population` case is part of the default bar.  It prebuilds
the exact `tally_core` test binary from the target, runs the focused race
regressions, and then invokes `test/flake-probe.sh <binary> 480 3`.

## Layout

- `run` / `run.py`: parameterized entry point and result reporter.
- `support.py`: process, artifact, daemon, and assertion primitives.
- `cases/`: black-box conformance cases grouped by public boundary.
- `fixtures/manifest/`: arm/driver campaign grammar corpus.
- `fixtures/adapters/`: evaluated-preset/rendered-argv corpus.
- `fixtures/usage/`: cumulative and declared-surface evidence corpus.
- `fixtures/registry/`: literal rollback records and N-1 provenance.
- `fixtures/pipeline/`: hermetic forge, agent, and flow helpers.

All expected values come from the normative specification or recorded tool
captures.  A fixture is never regenerated from the target's current output.
