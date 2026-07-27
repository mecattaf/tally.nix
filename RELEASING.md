# Releasing tally.nix

> **Release hold:** there are no releases yet. This file documents the procedure, but no worker or
> automation may create or push a version tag. Tom must explicitly authorize a tag on a later day,
> after personal day-to-day use. Green gates, completed milestones, or this runbook alone are not
> authorization.

Every release names one immutable commit from `origin/main`. Prepare and merge the release commit
before starting the live gate, and keep the worktree clean for every command below.

## Prepare the release commit

1. Choose `X.Y.Z` and update all three version declarations:
   - `[workspace.package].version` in `Cargo.toml`;
   - the `tally` package version in `flake.nix`; and
   - the `tally-doc` package version in `flake.nix`.
2. Run `cargo check --workspace`, inspect the local workspace-package entries changed in
   `Cargo.lock`, and then run `cargo check --workspace --locked`. A release version change must not
   become an unrelated dependency update.
3. Refresh only the pinned RustSec database and inspect the lock diff:

   ```console
   $ nix flake update advisory-db
   $ git diff -- flake.lock
   $ nix develop --command test/cargo-deny.sh
   ```

4. Move the accumulated `[Unreleased]` entries in `CHANGELOG.md` into
   `[X.Y.Z] - YYYY-MM-DD`, restore an empty `[Unreleased]` section, and update its comparison links.
   The release notes are derived from that exact section.
5. Add this compatibility declaration to the release notes, with evidence instead of assumptions:

   ```text
   State compatibility
   - Upgrade: vX.Y.Z can read state last written by vPREVIOUS: yes/no — reason.
   - Rollback: vPREVIOUS can read state written by vX.Y.Z: yes/no — reason.
   - Required backup, migration, or restore procedure: none/details.
   ```

   Durable task rows use the ordered taskdb migration registry. That does not by itself guarantee
   that an older binary can read state after the new binary has migrated or written it; both
   directions must be tested and stated for every release.
6. Open a release-preparation pull request, run the gates below, and merge it normally. Record the
   resulting `origin/main` SHA as `release_sha`; do not tag a pull-request head.

## Standard gate

Run the ordinary ladder on the release commit:

```console
$ nix develop --command cargo fmt --all --check
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command cargo test --workspace
$ nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings
$ nix develop --command test/cargo-deny.sh
$ nix flake check -L
$ grep -rn 'todo!\|unimplemented!\|TODO' crates/
```

The final grep passes only when it prints nothing and exits 1. The fleet runner additionally proves
that no GitHub workflow exists, asserts the `flow-multi-host` VM inventory, enforces the changelog
rule, publishes its transcript, and posts the required status for the exact SHA:

```console
$ test/fleet-gate.sh "$release_sha"
```

Do not continue until `fleet/gate-ladder` is green for `release_sha`, `origin/main` still resolves to
that SHA, and the linked transcript is complete.

## Live gate on the designated host

The live tests do not SSH to `TALLY_TEST_REMOTE_HOST`; the variable is an explicit opt-in marker.
Log into the named NixOS host, check out `release_sha` there, enter the repository development shell,
and run the command on that host itself:

```console
$ TALLY_TEST_REMOTE_HOST="$(hostname -f)" nix develop --command \
    cargo test --workspace -- --ignored --nocapture --test-threads=1
```

The transcript must show all six tests passing, by name:

- `systemd_user_manager_liveness_smoke`
- `real_type_notify_daemon_survives_watchdog_periods`
- `real_user_manager_adapter_capture_scrape`
- `real_user_manager_daemon_contention_restart_soak`
- `real_user_manager_executor_smoke`
- `real_user_manager_journal_paths`

Run the two release scenarios from the same checkout:

```console
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command \
    test/scenarios/run fleet-conformance
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command \
    test/scenarios/run slow-sqlite
```

Repeat the six-test live gate before every authorized release and after any change to
`crates/tally-core/src/executor.rs`, `crates/tally-core/src/lease.rs`, or
`crates/tally-core/src/journal.rs`. The `pool-vanished/return` scenario reboots its target and is
confined to a disposable microVM; never run it against the designated live host.

Capture stdout and stderr with pipe-failure preservation. Retain the full fleet and live transcripts,
including the commit SHA, host name, pinned advisory database revision, six live-test names, and both
scenario results. Attach that evidence to the GitHub release rather than pasting only a summary.

## Upgrade and rollback

Before switching a host, quiesce new admission and take a recoverable backup of its configured
`stateDir` and `dataDir`. Preserve witness, taskdb, lease, launch-marker, and worker state together;
never delete durable state to force a deployment or rollback to start.

For a NixOS deployment, select the previous system generation with:

```console
$ sudo nixos-rebuild --rollback switch
```

For a standalone Home Manager deployment, identify and activate the previous generation:

```console
$ home-manager generations
$ /nix/store/<previous>-home-manager-generation/activate
```

If the release's rollback compatibility statement says the previous binary cannot read state written
by the new binary, stop tally and restore the matching pre-upgrade `stateDir` and `dataDir` backup
before activating the old generation. Do not mix directories from different snapshots. After either
rollback, verify the daemon, query the pools and jobs, verify the witness chain, and run one bounded
canary before reopening admission.

## Sign, tag, and publish

This section is procedure only. Execute it only after Tom gives explicit tag authorization and every
gate above is green for the unchanged `release_sha`. Set `release_sha` to that full commit ID,
`transcript` to the retained gate transcript, and `release_notes` to the notes cut from the changelog
before running the commands.

```console
$ version=X.Y.Z
$ tag="v$version"
$ git fetch origin --tags
$ test -z "$(git status --porcelain)"
$ test "$(git rev-parse origin/main)" = "$release_sha"
$ git tag -s -a "$tag" "$release_sha" -m "tally $tag"
$ git verify-tag "$tag"
$ test "$(git rev-list -n 1 "$tag")" = "$release_sha"
$ git push origin "refs/tags/$tag"
$ gh release create "$tag" "$transcript#release gate transcript" \
    --verify-tag --title "tally $tag" --notes-file "$release_notes"
```

The tag is signed and annotated; never move or replace a published tag. If a released build needs a
correction, preserve its evidence and publish a new version through the same procedure.
