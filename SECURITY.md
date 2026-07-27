# Security policy

## Supported versions

There are no releases yet; pin a reviewed commit. Until a versioned release exists, there is no
supported SemVer line or promise that an arbitrary commit receives backported security fixes.

## Report a vulnerability privately

Do not open a public issue for a suspected vulnerability. Use either of these private routes:

- [GitHub private vulnerability reporting](https://github.com/mecattaf/tally.nix/security/advisories/new)
- email [thomas@leger.run](mailto:thomas@leger.run)

Include the reviewed commit, deployment mode, reproduction steps, and expected impact. Do not send
real credentials, private witness records, or other sensitive operator data unless a secure transfer
method has been agreed first.

An initial acknowledgement is due within seven calendar days. The reporter and maintainer will
coordinate remediation, release timing, and attribution. The target for coordinated public
disclosure is within 90 days of the first private report; change that date only by explicit mutual
agreement or when earlier disclosure is necessary to protect users.

## Supported trust model

tally runs on one machine, under one trusted Unix user, running local AI plus authenticated
claude-code/codex subscriptions whose utilization it maximizes. Keep the trust model that simple:
one tally instance is not a multi-tenant service and does not isolate mutually hostile same-UID
processes.

The NixOS and Home Manager services place the Unix socket inside a runtime directory with mode
`0700`. That filesystem boundary excludes other Unix users. A process already running as tally's
service user is in the same trust domain and is trusted as an operator; it can inspect same-UID
process state and reach the daemon socket. The local, filesystem-protected RPC boundary is described
in [RPC protocol contract](doc/src/reference/rpc-protocol.md#socket-and-framing).

[Job-origin admission](doc/src/concepts/jobs-and-admission.md#admission-is-a-boundary-not-a-queue-append)
uses identity to enforce ancestry, depth, fan-out, and `noEnqueue`. Environment and request fields
such as `TALLY_JOB_ID` and `callerJobId` are context, not authorization on their own. The ratified
job-identity contract binds a daemon-minted per-job token to the running job so a job cannot
impersonate a sibling or evade those guardrails. On a commit without token enforcement, treat the
job guardrails as cooperative and run only trusted workloads. The token makes guardrails real; it
does not turn hostile same-user code into a supported tenancy model.

[Hardening presets](doc/src/configuration/hardening.md) reduce ambient filesystem, device, kernel,
and credential access when an operator opts into them. They are defense in depth, not a hostile-code
sandbox. Presets do not make pool accounting a hard isolation boundary and must not be assumed to
remove network or daemon-socket access unless the rendered unit properties say so.

The canonical [witness chain](doc/src/concepts/witness-ledger.md#what-a-record-binds) is hash-linked
and detects changed, missing, duplicated, or reordered records. It is unsigned. A valid chain does
not authenticate the machine or prove which human or process wrote it; its authority comes from the
single-writer deployment and acknowledgement boundary.

## Operational credentials

### Fleet merge-gate token

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
