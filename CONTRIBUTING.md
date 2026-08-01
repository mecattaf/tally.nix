# Contributing to tally.nix

Thank you for helping improve tally. Keep changes inside its defining boundary: tally arbitrates
contention and emits proof. A driver decides what work should run and interprets domain output.

Project policy lives in the [security policy](SECURITY.md), [release runbook](RELEASING.md),
[changelog](CHANGELOG.md), and [dependency policy](deny.toml). Read the security policy before
handling vulnerability reports or credentials. Documenting the runbook's tag commands is not
authorization to execute them.

## Development environment

Use the flake development shell; it provides Rust, Cargo, Clippy, rustfmt, jq, and the other
tools used by the checks.

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
$ nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings
$ nix develop --command cargo fmt --all --check
$ nix develop --command test/cargo-deny.sh
$ nix flake check -L
```

The live flow suite waits on fixed wall-clock budgets sized for an idle machine. When reproducing a
suspected timeout on a loaded host, set `TALLY_TEST_TIMEOUT_SCALE` to a positive multiplier to widen
every one of those budgets without editing the tests:

```console
$ TALLY_TEST_TIMEOUT_SCALE=3 cargo test -p tally --test flow_live
```

Unset means `1` and is byte-identical to the unscaled budgets. A value that is not a positive finite
number panics rather than silently running unscaled. The variable is read from the test process
environment, so it reaches a direct `cargo test` run only; the tests run by `nix flake check` execute
inside `buildRustPackage`'s pure sandbox and never see it. Diagnosing a red gate happens on the
direct path anyway. The knob widens waits; it never changes what a test asserts.

`test/cargo-deny.sh` checks advisories, licenses, sources, and duplicate versions with Cargo in
offline and locked mode. The development shell supplies the RustSec database revision pinned by
the `advisory-db` flake input; refresh that input deliberately instead of fetching during the gate.

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

Run all three scenario entry points. The first two are local. The third must skip with exit 0
when no host is selected:

```console
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command \
    test/scenarios/run fleet-conformance
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command \
    test/scenarios/run fanout-guardrail
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command \
    test/scenarios/run pool-vanished/return
```

Finally, keep the Rust tree free of placeholders:

```console
$ grep -rn 'todo!\|unimplemented!\|TODO' crates/
```

The expected result is no output and grep exit 1.

## Merge verification

This repository has no GitHub Actions workflows or GitHub-hosted checks. The implementing worker
runs the one canonical local ladder in its worktree for the exact pull-request head:

```console
$ test/fleet-gate.sh "$(git rev-parse HEAD)"
```

The runner uses the operator's existing `gh` authentication to read pull-request metadata, then
starts from a pristine clone and disposable detached worktree. In order it runs
`cargo fmt --all --check`, `env -u TALLY_TEST_REMOTE_HOST cargo test --workspace`, Clippy for all
workspace targets and features with warnings denied, the pinned offline dependency-policy stage,
`nix flake check -L`, an evaluated-check assertion for the `flow-multi-host` VM, the no-stubs grep,
the no-workflows assertion, and the changelog stage. A pull request must touch `CHANGELOG.md` or
carry the `no-changelog` label. A main-branch audit records that this rule was enforced on the pull
request head rather than inventing a second diff.

The runner writes a local transcript below
`${XDG_STATE_HOME:-$HOME/.local/state}/tally-fleet-gate/transcripts/` and prints its path. Paste the
transcript tail into the pull request. That worker-run transcript is the merge evidence; the runner
does not publish evidence or write any merge-control state to GitHub.

For the single-operator phase, #128 item 5's “independently enforced merge control” is
**consciously rejected**. Trusted agents act under the operator's existing `gh`, Claude Code, and
Codex authentication; a GitHub-side control those agents could bypass would add ceremony and
credential cost without creating an independent boundary. Revisit this decision only at the first
outside contributor or the first release tag, and only through a new ruling from Tom.

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

## Changelog discipline

Every behavior-affecting pull request must add a concise entry under the appropriate heading in
`CHANGELOG.md`'s `[Unreleased]` section. Behavior includes user-visible CLI/RPC changes, execution or
admission semantics, configuration defaults and options, durable formats, security posture, and
operator-facing deployment behavior.

A pull request with no release-note-worthy effect may carry the `no-changelog` label instead. State
the reason in its description; the label is an explicit reviewable decision, not a way to skip an
entry for behavior that changed. The local runner enforces the mechanical rule against the exact
pull-request base and head.

## Change hygiene

- Add focused tests for changed behavior and preserve the full gate suite.
- Keep credentials by reference; never log or fixture secret values from a real environment.
- Preserve direct argv arrays. Do not introduce shell-string execution as an adapter mode.
- Record a gate that could not run as **NOT RUN**, never as passed.
- Keep documentation aligned with executable code and generated module output.
