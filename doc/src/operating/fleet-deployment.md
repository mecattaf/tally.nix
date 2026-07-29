# Fleet deployment

A fleet has one coordinator and zero or more daemonless SSH workers. The
coordinator owns admission, leases, event history, and witnesses. A worker
accepts only the fixed `tally __remote-executor` protocol over SSH, launches
transient user units, and retains its local launch markers, exit records, and
captures.

The shipped multi-host check uses a Home Manager coordinator embedded in a
NixOS host. That distinction matters: the NixOS module renders the system
daemon, witness emitter, drain timer, and retention timer. It rejects producers,
meters, and flow calendar declarations; those workload-scheduling units exist
only in the Home Manager module.

## Choose the coordinator shape

Use the NixOS module when the host needs a machine-wide daemon:

```nix
{
  imports = [ inputs.tally.nixosModules.tally ];

  services.tally = {
    enable = true;
    pools.local-build = {
      resource = "build-slot";
      capacity = 2;
    };
  };
}
```

This creates the dedicated `tally` service account, installs
`tally-daemon.service` plus its drain and retention timers, listens on
`/run/tally/tally.sock`, and stores durable state under
`/var/lib/tally/{data,state}`. Set `retention.enable = false` when host-wide Nix
store collection is not part of this machine's policy.

Use Home Manager when tally itself should materialise scheduled work:

```nix
{
  imports = [ inputs.tally.homeManagerModules.tally ];

  services.tally = {
    enable = true;

    pools = {
      coordinator-slot = {
        resource = "build-slot";
        capacity = 1;
      };
      worker-slot = {
        resource = "build-slot";
        capacity = 1;
      };
    };

    flows.nightly = {
      script = ./flows/nightly.js;
      onCalendar = "daily";
      args = { };
      maxNodes = 20;
      runtimeMaxSec = 7200;
    };
  };
}
```

The user daemon listens on
`$XDG_RUNTIME_DIR/tally/tally.sock`; its default durable directories are
`$XDG_DATA_HOME/tally` and `$XDG_STATE_HOME/tally`, with the usual
`~/.local/share` and `~/.local/state` fallbacks.

In a NixOS fleet, integrating this Home Manager configuration through
`home-manager.nixosModules.home-manager` gives the user a reproducible system
generation. Enable lingering for the coordinator account so its user manager
and timers survive logout:

```nix
{ inputs, ... }:
{
  imports = [ inputs.home-manager.nixosModules.home-manager ];

  users.users.tally = {
    isNormalUser = true;
    createHome = true;
    home = "/var/lib/tally-coordinator";
    linger = true;
  };

  home-manager = {
    useGlobalPkgs = true;
    useUserPackages = true;
    users.tally = {
      imports = [ inputs.tally.homeManagerModules.tally ];
      home = {
        username = "tally";
        homeDirectory = "/var/lib/tally-coordinator";
        stateVersion = "26.11";
      };
      services.tally = {
        enable = true;
        retention.enable = false;
        pools = {
          coordinator-slot = {
            resource = "build-slot";
            capacity = 1;
          };
          worker-slot = {
            resource = "build-slot";
            capacity = 1;
          };
        };
      };
    };
  };
}
```

This is a complete single-host coordinator before adding the executor block
below. Retention is disabled here because its default Home Manager timer calls
host-wide Nix GC; choose the horizon deliberately before enabling it.

Do not enable both a system daemon and a Home Manager daemon as though they
formed one coordinator. They have different sockets and durable directories.

## Provision a worker

A worker runs no tally daemon. It needs:

- the exact `tally` executable named by the coordinator's executor record;
- a dedicated SSH account with public-key authentication;
- a persistent user systemd manager, normally through lingering;
- a private `stateDir` writable by that account; and
- every executable, credential, repository, and artifact path used by remote
  job argv.

A minimal NixOS worker looks like this:

```nix
{ inputs, pkgs, ... }:
{
  environment.systemPackages = [ inputs.tally.packages.${pkgs.system}.tally ];

  users.users.tally-worker = {
    isNormalUser = true;
    createHome = true;
    home = "/var/lib/tally-worker";
    linger = true;
    openssh.authorizedKeys.keys = [
      "ssh-ed25519 AAAA... coordinator-for-tally"
    ];
  };

  services.openssh = {
    enable = true;
    settings = {
      PasswordAuthentication = false;
      KbdInteractiveAuthentication = false;
      PermitRootLogin = "no";
      AllowUsers = [ "tally-worker" ];
    };
  };

  systemd.tmpfiles.rules = [
    "d /var/lib/tally-remote 0700 tally-worker users -"
  ];
}
```

Create a dedicated keypair in a private provisioning directory outside the
repository:

```console
$ ssh-keygen -t ed25519 \
    -f /secure/provisioning/tally-worker-key \
    -C coordinator-for-tally
```

Put only `tally-worker-key.pub` in the worker's
`openssh.authorizedKeys.keys`. Deliver the private key through the
coordinator's encrypted-secret mechanism as
`/run/credentials/tally-worker-key`, owned by `tally` and mode `0400`. Never
import the private key as a Nix path or commit it.

The remote helper uses `systemd-run --user`. Verify the user manager before
admitting work:

```console
$ sudo -u tally-worker \
    XDG_RUNTIME_DIR=/run/user/$(id -u tally-worker) \
    systemctl --user is-active default.target
active
```

## Pin the SSH transport

Register the worker on the coordinator:

```nix
services.tally.executors.worker = {
  host = "worker.example.net";
  user = "tally-worker";
  port = 22;
  identityFile = "/run/credentials/tally-worker-key";
  knownHostsFile = "/etc/tally/worker-known-hosts";
  program = "/run/current-system/sw/bin/tally";
  stateDir = "/var/lib/tally-remote";
  connectTimeoutSec = 10;
  serverAliveIntervalSec = 15;
  serverAliveCountMax = 3;
  retryIntervalMs = 1000;
};
```

`program` and `stateDir` are paths on the worker. `identityFile` and
`knownHostsFile` are paths on the coordinator.

The private key described above must be readable by the coordinator account,
but no broader. The host key is public and should be pinned in the declared
known-hosts file:

```console
worker.example.net ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA...
```

Obtain the candidate during provisioning, compare its fingerprint with the
worker console or another trusted channel, then place the verified public line
at `/etc/tally/worker-known-hosts`:

```console
$ ssh-keyscan -t ed25519 worker.example.net > worker-known-hosts.candidate
$ ssh-keygen -lf worker-known-hosts.candidate
```

The file can safely be declared because it contains only a public host key:

```nix
environment.etc."tally/worker-known-hosts".source =
  ./worker-known-hosts;
```

tally does not read ambient SSH configuration or an agent. Its transport uses
an empty config, batch mode, the one identity, strict host-key checking, no
proxy command, and no forwarding. Test the same constraints before rollout:

```console
$ sudo -u tally ssh -T -F /dev/null \
    -o BatchMode=yes \
    -o IdentitiesOnly=yes \
    -o IdentityAgent=none \
    -o StrictHostKeyChecking=yes \
    -o UserKnownHostsFile=/etc/tally/worker-known-hosts \
    -o GlobalKnownHostsFile=/dev/null \
    -i /run/credentials/tally-worker-key \
    tally-worker@worker.example.net true
```

The fixed helper is a private framed-JSON protocol, not an operator shell
interface. Use `true` for a reachability probe; do not invoke
`__remote-executor` by hand.

## Place work deliberately

Pools are coordinator-side logical gates. `executor = "worker"` chooses the
transport independently:

```javascript
const built = await sh(["/run/current-system/sw/bin/build-report"], {
  executor: "worker",
  pools: ["worker-slot"],
  key: "build-report",
  evidence: ["exit:0"],
  runtimeMaxSec: 1800
});
```

The worker is not inferred from the pool name, and configuring an executor
does not move a pool to that host. The stock implementation exposes only
`enforce = "cooperative"`; it has no dmem, dmemcg-booster, serving-slice, or
kernel-backed resource isolation. The conformance scenario that required
those surfaces is recorded as `BLOCKED`, not as a software-enforced capacity
proof.

Flow runner jobs always request the generated `flow` pool and may co-lease one
typed capacity-1 `workloadMutex` for the runner process lifetime. Runner death
releases that mutex; replay waits behind the next holder while durable children
may complete. A direct manual flow run has no lease, so a mutex-declaring flow
must be enqueued as an admitted parent holding both pools.
The former `budgetPool` option has been removed because it never added a pool
to runner admission. Separately, flow nodes deliberately have no
`consumptionEstimate` field and configured flow checking excludes
`windowed-consumption` pools by design. Use priorities for contention between
flow workloads. Manual and producer enqueue retain
`--consumption-estimate` where that pool type is appropriate.

## Move artifacts explicitly

Remote execution transports requests, status, and bounded captures. It does
not copy workload artifacts. Cross-host handoff must be visible flow work:

- use an explicit Git push/fetch for repository content;
- use an explicit `attic push` followed by realisation from the configured
  substituter for Nix store objects; or
- call a workload-specific transfer tool in an `sh()` node and witness the
  receiving-side artifact.

There is no executor post-run push hook. A successful worker command proves
only its declared evidence on that execution host. If a later coordinator
node consumes a result, give that node an explicit transfer dependency and
its own evidence.

## Bump tally with the fleet configuration

Pin tally as a flake input and make the Home Manager coordinator part of the
host configuration:

```nix
{
  inputs.tally = {
    url = "github:mecattaf/tally.nix";
    inputs.nixpkgs.follows = "nixpkgs";
  };
}
```

Update only that input, inspect both source and transitive lock changes, and
build every affected host before switching one:

```console
$ nix flake update tally
$ git diff -- flake.lock
$ nix build .#nixosConfigurations.worker.config.system.build.toplevel
$ nix build .#nixosConfigurations.coordinator.config.system.build.toplevel
```

Commit the lock-file update with the fleet configuration that expects it.
When Home Manager is integrated into the coordinator's NixOS configuration,
the flow script is copied to the Nix store and referenced by that system
generation. `sudo nixos-rebuild --rollback switch` therefore restores the
older Home Manager configuration and its older flow-script store path along
with the rest of the host generation.

## Roll out without stranding work

For a forward deployment:

1. build both host configurations and run their evaluation checks;
2. deploy workers first—`sudo nixos-rebuild switch --flake .#worker`—including
   the exact remote `program` and all argv dependencies;
3. verify the worker user manager, pinned SSH probe, and writable `stateDir`;
4. provision the coordinator's private key, then deploy it with
   `sudo nixos-rebuild switch --flake .#coordinator`;
5. inspect `tally query pools`, start one bounded canary, and follow it with
   `tally watch`; and
6. verify its witness and receiving-side artifact before expanding admission.

Deploying the worker first prevents a new coordinator from selecting a helper
or executable absent on the worker. Existing remote leases remain owned by the
coordinator during a transport outage; tally retries rather than silently
running the work locally.

The first remote canary can be deliberately boring:

```console
$ sudo -iu tally
$ export XDG_RUNTIME_DIR=/run/user/$(id -u)
$ result="$(tally enqueue --pool worker-slot --executor worker \
    --evidence exit:0 --wait -- /run/current-system/sw/bin/true)"
$ tally query proof --task "$(printf '%s\n' "$result" | jq -r .task_uuid)"
```

That proves pinned SSH, the worker user manager, remote unit execution,
completion return, and coordinator witnessing before real argv are admitted.

For rollback, first stop the relevant producer timers or otherwise quiesce new
admission. Roll the coordinator back to a generation whose flow scripts and
configuration are known to the ledger, then roll workers back only after
running or ambiguous remote generations have settled. A flow witness records
the SHA-256 hash of the exact JavaScript bytes. Reverting to identical bytes
therefore restores the same script identity; a NixOS generation number is not
itself the identity.

Never erase a worker `stateDir` to make a rollout look clean. Its durable
launch marker is what lets the coordinator distinguish an absent job from a
possibly launched job. Losing it turns a diagnosable transport incident into
an audit gap.

## Fleet smoke test

After every host change, check the boundaries from both sides:

```console
# coordinator
$ systemctl --user is-active tally-daemon.service
$ tally query pools
$ tally query jobs --executor worker
$ journalctl --user -u tally-daemon.service --since today

# worker
$ systemctl --user list-units 'tally-job-*.service' --all
$ du -sh /var/lib/tally-remote
```

For a system daemon, use `systemctl`, `journalctl -u tally-daemon.service`, and
pass `--socket /run/tally/tally.sock`. The exact two-host recovery and
Git/Attic handoff exercised by the release is in the
[`flow-multi-host`](https://github.com/mecattaf/tally.nix/blob/284f641bd9b00036d7bd29f094f4b353872c30d0/flake.nix#L2015-L2016)
VM check.
