# Fleet merge gate units

These are new, standalone user units for the external `fleet/gate-ladder` commit status. They do
not modify tally's daemon, producer, drain, or witness units.

Install the runner and units from a trusted `main` checkout:

```console
$ contrib/systemd/install-fleet-gate.sh
```

Create `~/.config/tally-fleet-gate/github-token` with the fine-grained token described in
[`SECURITY.md`](../../SECURITY.md), set its mode to `0600`, and then enable the timer:

```console
$ systemctl --user enable --now tally-fleet-gate.timer
$ systemctl --user start tally-fleet-gate.service
$ journalctl --user -u tally-fleet-gate.service
```

Every poll considers all open pull-request heads and the current `main` head. A SHA with no
`fleet/gate-ladder` status is tested; a pending status older than two hours is retried after an
interrupted run. Finished success, failure, and error statuses are not repeated until the SHA
changes. Override the two-hour recovery window with `TALLY_GATE_PENDING_MAX_AGE_SEC` in
`~/.config/tally-fleet-gate/environment`.

The runner keeps its pristine bare clone under `~/.cache/tally-fleet-gate`, creates a disposable
detached worktree per SHA, and writes local transcripts under
`~/.local/state/tally-fleet-gate/transcripts`. It also commits each transcript as `<sha>.log` on
the repository's `gate-evidence` branch and uses that stable blob URL as the status target.
