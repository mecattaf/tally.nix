# Changelog

All notable changes to tally.nix are recorded here. The format is based on
[Keep a Changelog], and the project intends to follow [Semantic Versioning] once version tags are
authorized.

## [Unreleased]

### Added

- Added operator chapters for configured mechanisms and declarative flows, including exact module
  contracts and deployment asymmetries.
- Added a hermetic, offline dependency-policy gate with a flake-pinned RustSec advisory database,
  a tree-derived license allowlist, crates.io-only sources, and duplicate-version warnings.
- Added private vulnerability-reporting policy, a supported threat model, and an end-to-end release
  and rollback runbook.
- Added this changelog and the contribution rule that keeps `[Unreleased]` current.

### Changed

- Protected pull-request merges with an exact-head `fleet/gate-ladder` status produced from a
  pristine fleet checkout, with durable transcripts and explicit no-workflow/no-stub checks.
- Required each behavior-affecting pull request to update `[Unreleased]` unless it carries the
  `no-changelog` label.

### Security

- Documented the single-trusted-Unix-user boundary, cooperative versus token-bound job identity,
  unsigned witness-chain limits, and why hardening presets are not a hostile-code sandbox.
- Acknowledged `RUSTSEC-2024-0436` as the sole advisory suppression: unmaintained `paste` arrives
  through Boa 0.21.1 and must be revisited by 2026-12-31 or when a later Boa removes it.

## [Pre-release history]

Development through baseline commit [`c6c304e`] was pre-release; see `git log c6c304e` for that
earlier history. No version tag is implied by this retroactive section.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html
[Unreleased]: https://github.com/mecattaf/tally.nix/compare/c6c304e...HEAD
[Pre-release history]: https://github.com/mecattaf/tally.nix/commits/c6c304e
[`c6c304e`]: https://github.com/mecattaf/tally.nix/commit/c6c304e
