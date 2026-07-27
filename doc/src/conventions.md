# Conventions

These conventions keep configuration, queries, and witnesses readable across
a fleet. Statements marked “must” are enforced by the shipped surface;
recommendations are house style.

## Ownership boundary

Keep mechanisms in tally.nix and instances in the consuming configuration.

| Owner | Examples |
|---|---|
| tally.nix | Pool and producer schemas, adapter protocol, flow dialect, RPC framing, systemd unit rendering, recovery, evidence evaluation, witness format |
| Fleet configuration | Named pools and executors, capacities, schedules, flow registrations and args, credential paths, retention horizon |
| Flow or workload | Which jobs should exist, node keys, dependency structure, artifact handoff, domain-specific acceptance and repair branches |
| External operator/system | Original intent, secret material, source repositories, approval, Attic policy, artifact archive policy |

The package should not acquire a producer instance, hostname, capacity, token
path, repository, or schedule merely because one deployment uses it. The
consumer should not reproduce tally's unit templates or mutate its rendered
JSON to get a new mechanism.

Nix assertions and `tally --mode check-config` validate the boundary at build
time. Change the Nix source and rebuild; do not patch
`~/.config/tally/config.json` or `/etc/tally/config.json` in place.

## Declared names

Pool, executor, producer, and flow attribute names must be safe registry or
unit components: they begin with an ASCII letter, digit, or underscore; the
remaining characters are letters, digits, underscores, dots, or hyphens; and
`.` and `..` are forbidden.

Prefer lowercase kebab-case:

```nix
services.tally = {
  pools.worker-gpu = { /* … */ };
  executors.build-worker = { /* … */ };
  producers.nightly-review = { /* … */ };
  flows.monthly-review = { /* … */ };
};
```

Name the role, not a mutable implementation detail. `review-gpu` ages better
than `gpu-24gb-1`; capacity and host placement already have explicit fields.
When host location is operationally important, use a stable role prefix such
as `worker-build`, not an IP address.

Adapter names are technically less restrictive—non-empty and no NUL—but use
the same lowercase style. It avoids quoting surprises in queries and keeps
custom adapters visually distinct from their invocation arguments.

Generated Home Manager unit names include producer names:

```text
tally-producer-<name>.service
tally-producer-<name>.timer
tally-meter-<pool>.service
```

Do not create hand-written units with those prefixes.

## Stable identities

Use the task UUID as the durable anchor. A transient execution unit is named
`tally-job-<unit-uuid>.service`, and attempt/epoch lanes can change during
recovery. Query output resolves those live identities back to the task.

Names serve different purposes:

| Name | Meaning | Identity role |
|---|---|---|
| `description` or node `label` | Human-readable observation | Not canonical payload identity; still recorded provenance |
| producer `dedupKey` | UTC strftime-expanded existence key | Stable intake key; not a payload hash |
| flow `key` | Flow-local explicit key, namespaced by `flowRunId` | Stable within one run |
| flow `dedupKey` | Raw author-supplied global key | Use only for intentional cross-run identity |
| task UUID | Durable admitted task anchor | Immutable |
| `flowRunId` | Runner task UUID and flow provenance key | Immutable for that run |

Prefer `key` over raw `dedupKey` inside flows:

```javascript
await sh(["review", args.repository], {
  key: `review:${args.repository}`,
  label: "review repository",
  pools: ["review-gpu"]
});
```

tally renders that key as `flow:<flowRunId>:k:<key>`. Without an explicit key,
it uses the deterministic ordinal
`flow:<flowRunId>:<nodeOrdinal>`. A raw `dedupKey` opts out of the run
namespace and should describe an intentionally global, immutable payload.

Producer dedup keys are UTC strftime templates. Include the intended cadence
and domain, for example `nightly-review:%Y-%m-%d`, rather than a timestamp more
precise than the producer's schedule.

## Repository layout

A consuming flake is easiest to audit with declarations near their immutable
inputs:

```text
flake.nix
flake.lock
nix/
  tally.nix
flows/
  monthly-review.js
  monthly-review.catalog.json
prompts/
  reviewer.md
```

Keep a flow script and its optional selector catalog in source control. Pass a
path through the typed Nix option so it becomes an immutable store input. Read
prompt and skill bundle content during Nix evaluation when the adapter
configuration expects content; do not have replay discover a mutable file by
ambient path.

Runtime artifacts do not belong beside configuration by default. Give the
workload an absolute, declared output directory and a retention policy. For
cross-host work, make Git or Attic handoff an explicit node instead of relying
on matching path names.

## Surface spelling

The public surfaces use their native conventions:

- Nix options and JSON fields use camel case such as `runtimeMaxSec` and
  `flowRunId`.
- CLI long options use kebab case such as `--runtime-max-sec` and
  `--flow-run`.
- Environment capabilities are uppercase `TALLY_*`.
- Verdicts and status values use lowercase kebab case such as
  `clean-exit-no-artifact` and `cursor-expired`.
- Content identities spell the algorithm, normally `sha256:<hex>`.
- Times in query and witness records use RFC 3339 UTC; producer strftime
  expansion also uses UTC.

Do not translate these at integration boundaries. Copy field names from the
versioned schema and honour each surface's unknown-field policy.

## Evidence and artifacts

Declare the narrowest evidence that proves the intended transition:

- `exit:0` proves only process success;
- `artifact:/absolute/path` proves observed bytes at that path;
- `store:/nix/store/...` ties the result to Nix store evidence; and
- `drv()` declares derivation outputs and their Nix evidence automatically.

Name artifacts by domain result, not task UUID, when a human or downstream
system must find them. Keep the task UUID in metadata so the bytes can be
joined back to the witness.

A gate manifest records semantic culmination and acceptance facts. It does not
replace evidence that the claimed artifact exists. A provider trace or final
message is advisory and should not be named as canonical proof.

## Files on disk

Treat `dataDir` and `stateDir` as private durable storage:

- `dataDir` owns witness, attestation, lifecycle, and watch ledgers plus tally
  GC roots;
- `stateDir` owns enqueue ingress, captures, execution records, meter events,
  and other recovery inputs; and
- a remote executor's `stateDir` lives on that worker and must be backed up and
  inspected separately.

Use query and verification commands rather than parsing or editing files.
When an operator must archive storage, quiesce the coordinator, preserve file
order and permissions, and test recovery before removing the source. The
supported pruning boundary is documented in
[Retention and growth](operating/retention.md).
