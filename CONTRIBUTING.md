# Contributing to tally.nix

Thank you for helping improve tally. Keep changes inside its defining boundary: tally arbitrates
contention and emits proof. A driver decides what work should run and interprets domain output.

## Development environment

Use the flake development shell; it provides Rust, Cargo, Clippy, rustfmt, jq, SQLite,
Taskwarrior, and the other tools used by the checks.

```console
$ nix develop
$ cargo build --workspace
```

Do not update `Cargo.lock` for source-only, test-only, Nix-only, or documentation-only changes.
Update it only when the dependency graph genuinely changes.

## Required gates

Run the ordinary suite with no remote host selected:

```console
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command cargo test --workspace
$ nix develop --command cargo clippy --workspace --all-targets -- -D warnings
$ nix develop --command cargo fmt --all --check
$ nix flake check -L
```

Verify the good ledger and prove that the tampered ledger is rejected:

```console
$ nix develop --command cargo run --quiet -p tally -- \
    witness verify test/fixtures/ledger/valid.jsonl
$ nix develop --command cargo run --quiet -p tally -- \
    witness verify test/fixtures/ledger/tampered.jsonl
```

The first command must exit 0. The second must exit 1; its nonzero status is the expected result,
not a gate failure.

Run the stock-host VM test explicitly when systemd or either module changes. It proves that the
event drain and producer timers fire autonomously from a fresh boot:

```console
$ nix build -L .#checks.x86_64-linux.stock-host-activation --no-link
```

Run all four scenario entry points. The first three are local. The fourth must skip with exit 0
when no host is selected:

```console
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command \
    test/scenarios/run fleet-conformance
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command \
    test/scenarios/run fanout-guardrail
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command \
    test/scenarios/run slow-sqlite
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command \
    test/scenarios/run pool-vanished/return
```

Finally, keep the Rust tree free of placeholders:

```console
$ grep -rn 'todo!\|unimplemented!\|TODO' crates/
```

The expected result is no output and grep exit 1.

## Merge verification

This repository has no GitHub Actions workflows or GitHub-hosted checks. GitHub is used for
issues, pull requests, and merges, never for verification. Before every merge, the implementing
worker runs the authoritative gate ladder on this KVM-capable fleet host:

```console
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command cargo test --workspace
$ nix develop --command cargo clippy --workspace --all-targets -- -D warnings
$ nix flake check -L
$ grep -rn 'todo!\|unimplemented!\|TODO' crates/
```

The `nix flake check -L` run must include the `flow-multi-host` coordinator-and-worker NixOS VM
check. The no-stubs grep must produce no output and exit 1. The worker pastes the gate transcript
into the pull request, and that transcript alone is the merge evidence. Nothing waits on, blocks
on, or schedules a GitHub-hosted check because none exists.

## Live-system tests

Ignored Rust tests exercise a real NixOS user manager and journal. They require an explicit
`TALLY_TEST_REMOTE_HOST` opt-in and should be run on the named host. With the variable unset they
print `SKIP` and return before touching systemd.

The `pool-vanished/return` scenario is destructive to the selected test host: it requires SSH,
copies a Nix store package, and invokes `sudo -n systemctl reboot`. Use a disposable machine and
read the scenario before opting in.

## Scope boundary

Changes belong in tally when they improve one of these mechanisms:

- admission and local resource leases;
- safe transient-unit execution and cooperative yield;
- durable enqueue, evidence, witness, recovery, or query behavior;
- the five existing producer kinds;
- the structured adapter envelope; or
- the Home Manager and NixOS packaging of those mechanisms.

Put workflow policy in a driver instead. Examples include choosing a task, interpreting an
artifact, deciding how to review or retry domain work, managing interactive sessions, or routing
work between machines. Do not add speculative option names, enum slots, or placeholder branches
for features that are not implemented.

New producer behavior must not silently expand the closed kind set. New agent integrations should
normally be data-only adapters built with `lib.adapters.mkAdapter`.

## Change hygiene

- Add focused tests for changed behavior and preserve the full gate suite.
- Keep credentials by reference; never log or fixture secret values from a real environment.
- Preserve direct argv arrays. Do not introduce shell-string execution as an adapter mode.
- Record a gate that could not run as **NOT RUN**, never as passed.
- Keep documentation aligned with executable code and generated module output.
