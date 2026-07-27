# Operational credentials

## Fleet merge-gate token

The fleet-side merge gate uses a dedicated fine-grained GitHub personal access token. Limit it to
the `mecattaf/tally.nix` repository with commit-status write and pull-request read access (plus the
metadata access GitHub grants automatically). Do not reuse a developer, release, or broad `gh`
login token.

Store the token only on the fleet coordinator at
`~/.config/tally-fleet-gate/github-token`, owned by the service user with mode `0600`. The poller
rejects a token file with any other mode. Never put the token in this repository, a unit file, a
command-line argument, a transcript, or a journal environment dump. Rotate it at least quarterly
and immediately after suspected disclosure; revoke the old token after a green status has been
posted with the replacement.

Publishing transcripts to the `gate-evidence` branch uses the coordinator's separately configured
Git credential. The status token deliberately does not gain repository-contents write access for
that purpose.
