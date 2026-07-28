# Cross-host handoff

**Tally moves no bytes between hosts.** An SSH executor moves a bounded job
description to a worker and returns status, captures, evidence metadata, and a
terminal result. It does not copy a checkout, an output file, or a Nix store
object. Cross-host bytes need an explicit data plane that both steps can name.

The shipped choices are ordinary, visible infrastructure:

- commits and branches in a Git repository for source, workspace state, and
  small receipts;
- an artifact store such as Attic for Nix store objects, with an explicit push
  by the producer and substitution by the consumer; or
- an explicit object-store upload/download, such as R2 for public artifacts.

Those transfers are workload nodes, not executor hooks. Evidence can prove an
artifact on the host where a job ran, but a witness only records the path and
proof; it never contains the artifact bytes. Flow scripts must not treat the same
absolute pathname on two hosts as shared storage.

## What `executor` changes

A node chooses a configured target by name:

```javascript
const built = await sh([args.workerProgram], {
  executor: "worker",
  pools: ["worker-slot"],
  key: "worker-artifact",
  evidence: ["exit:0"]
});
```

The `executor` field is an optional non-empty string in a `job()` specification;
omitting it selects local execution. Its result is the same `NodeResult` as a
local node. `executor: "worker"` appears in the durable payload and witness, so
changing it under an existing flow run is a payload divergence rather than a
transparent relocation.

The named executor must exist in the coordinator's configuration. An unknown
name is rejected by admission. The worker command and any absolute argv, cwd,
credential, workspace, and evidence paths must exist in the worker's namespace.
A missing program or checkout becomes an ordinary failed terminal node; it is not
recovered by copying the coordinator's path.

Pools remain logical admission gates on the coordinator. A node can acquire a
pool named `worker-slot` and execute on the worker, but Tally does not inspect the
worker to prove that the pool describes its hardware. The operator owns that
mapping.

## Configure a daemonless SSH worker

The central daemon owns admission and the witness ledger. A remote worker runs no
second Tally daemon; it needs SSH, the configured Tally binary, a usable user
systemd manager, and durable private state for remote unit records:

```nix
services.tally = {
  enable = true;

  pools = {
    coordinator-build = {
      resource = "build-slot";
      capacity = 1;
    };
    worker-deploy = {
      resource = "build-slot";
      capacity = 1;
    };
  };

  executors.worker = {
    host = "worker.example.net";
    user = "tally-worker";
    identityFile = "/etc/tally/worker-key";
    knownHostsFile = "/etc/tally/worker-known-hosts";
    program = "/run/current-system/sw/bin/tally";
    stateDir = "/var/lib/tally-remote";
    connectTimeoutSec = 10;
    serverAliveIntervalSec = 15;
    serverAliveCountMax = 3;
    retryIntervalMs = 1000;
  };

  flows.fleet-deploy = {
    script = ./flows/fleet-deploy.js;
    args = {
      remote = "origin";
      revision = "refs/heads/release";
      coordinatorCheckout = "/srv/deploy/coordinator";
      workerCheckout = "/srv/deploy/worker";
    };
    maxNodes = 5;
  };
};
```

This registration stanza is provided by the Home Manager module. With the
default `onCalendar = null` it is checked but not scheduled. The NixOS module
rejects flow declarations because it does not render their calendar producers
or automatic pools.

The coordinator always invokes a fixed `tally __remote-executor` command through
OpenSSH with an empty SSH config, pinned known-hosts file, explicit key, and no
agent. The workload request travels as JSON on stdin; its argv is not joined into
the SSH command or evaluated by a remote shell. Worker credentials in a job are
worker-side paths provisioned to its transient systemd unit.

Keep `stateDir` on storage that survives worker restarts. Before launch, the
worker fsyncs a generation marker. If it later finds that marker with neither the
exact unit nor a durable exit record, it refuses ambiguous replay rather than
starting a possible duplicate.

## The fleet-deploy handoff, honestly

[`fleet-deploy.js`](https://github.com/mecattaf/tally.nix/blob/main/examples/flows/fleet-deploy.js)
uses five shell nodes and no language model:

1. The coordinator pushes the requested revision to
   `refs/heads/tally-deploy` under `coordinator-build`.
2. The `worker` executor fetches that ref into its separate checkout under
   `worker-deploy`.
3. The worker checks out `FETCH_HEAD` detached.
4. The worker pushes its resulting `HEAD` to
   `refs/heads/tally-deployed` as a receipt.
5. The coordinator fetches that receipt ref.

The host boundary is just two fields on the worker nodes:

```javascript
await sh(
  ["git", "-C", args.workerCheckout, "fetch", args.remote,
   "refs/heads/tally-deploy"],
  {
    pools: ["worker-deploy"],
    executor: "worker",
    key: "worker-fetch",
    evidence: ["exit:0"],
    label: "worker-fetch"
  }
);
```

This example is a handoff skeleton, not a production deployment engine. The step
named `worker-deploy` only runs `git checkout --detach`; it does not switch a
NixOS generation, restart a service, or run a health check. Its canonical
evidence is only `exit:0`. The final coordinator fetch proves that the named ref
is reachable, but the example does not hash a deployment receipt or validate its
contents.

The branch names are also fixed. Concurrent flow runs can overwrite the same Git
refs even though their Tally keys are flow-local. Use this exact example only
where some external operating rule guarantees a single deployment at a time, or
adapt the existing arguments and script to use unambiguous per-deployment refs.
There is no ruled flow API today for holding a mutex or lease for the whole run;
that question remains open in
[#107](https://github.com/mecattaf/tally.nix/issues/107).

Both checkouts must already exist, both hosts must be able to reach the configured
Git remote, and `git` must be on the execution PATH. Tally creates none of those
prerequisites. A safer production version would also make its receipt a small,
versioned document and validate it in the coordinator step.

## A store-native handoff

Large or immutable Nix results belong in a binary cache, not a Git commit. The
shipped `flow-multi-host` VM test exercises both channels in one two-node flow.
Its worker node runs this sequence on the remote host:

1. clone a Git repository;
2. write a small `artifact.txt`;
3. add a second file to the Nix store with `nix store add`;
4. explicitly run `attic push` for that store path;
5. write the path into `attic-store-path.txt`; and
6. commit both small handoff files and push an `artifact` branch.

The following coordinator-local node then:

1. clones the `artifact` branch;
2. validates the plain Git artifact;
3. reads and shape-checks the reported `/nix/store/...` path;
4. proves that path is initially absent locally;
5. realizes it through the configured Attic substituter; and
6. validates the substituted content.

The Git document carries identity and a small receipt. Attic carries the store
bytes. Tally carries neither. In a real flow, make the push and the downstream
substitution or validation separate witnessed nodes so replay can identify which
side of the handoff completed.

There is no executor post-run cache push. Hiding publication there would make a
passing worker result ambiguous: the work could be terminal while its data was
still unavailable. An explicit push lets `exit:0`, `store:<path>`, artifact
evidence, and a later consumer say exactly what was durable at each boundary.

## Restart and transport behavior

An SSH interruption is not permission to launch another process. Remote unit
identity includes the durable task UUID, attempt, lease epoch, and systemd
invocation ID. The coordinator retains logical leases while it repeats `Ensure`,
`Probe`, or `Adopt` until the worker reports an authoritative state. Missing or
contradictory live state fails closed.

The multi-host VM deliberately makes the worker sleep after preparing its Git and
Attic data, records the remote unit's PID and invocation ID, and kills the
coordinator daemon. After the daemon restarts:

- the lease epoch advances;
- the existing worker unit is re-adopted without changing its PID or invocation;
- a replaying flow runner submits the same child and receives `attached`;
- exactly one remote unit finishes and pushes the handoff; and
- the coordinator consumes both the Git artifact and the substituted store
  object, then verifies the witness ledger.

That is the tested recovery claim. The test does not claim that an arbitrary
script can recover a half-finished external upload, or that Git/Attic themselves
are transactional together. Each handoff command must be idempotent or able to
recognize its own already-published result.

## Failure and replay predictions

| Event | Result |
|---|---|
| Worker is unreachable before or after launch | Transport ambiguity retains the row and leases while the coordinator probes/retries; it does not relocate the node locally. |
| Worker program exits nonzero | The node receives a failed terminal verdict; the next dependent `await` is not executed unless the script explicitly settles and handles it. |
| Coordinator daemon restarts during a known live unit | Recovery probes and re-adopts that exact invocation; replay attaches to the same attempt. |
| Worker has a launch marker but no unit or durable exit | Recovery refuses the ambiguous prior launch instead of replaying argv. |
| Git push passed but the runner died | Replay reuses the passing push witness, then continues at the fetch/consume frontier. |
| A separately witnessed Attic-push node passed but its later receipt node did not | Replay reuses the push only if its declared evidence still validates; the later Git node must still publish the path. The two-node VM fixture combines these operations and cannot make that finer claim. |
| Consumer receives a malformed or unavailable store path | Its explicit shape, realization, or content check fails; Tally does not fabricate or copy the object. |
| A script passes a producer-host path to a consumer on another host | The consumer sees its own namespace and normally fails with a missing path. |

The durable orchestration record answers *which host ran which command and what
that command proved*. Git, Attic, or the chosen object store answers *where the
bytes are*. Keeping those roles separate is the whole cross-host contract.
