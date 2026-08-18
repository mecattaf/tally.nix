# Install tally as a product

[Install tally](install.md) deploys tally the way a fleet deploys it: one Home
Manager generation that owns pools, producers, meters, and declarative flows,
usually pinned by a coordinator's configuration. This page is the other half of
the same flake — what a single machine does to run campaigns on its own
repositories, with no coordinator, no fleet pin, and nobody else's generation
in the way.

The product path is two independent halves:

- **The CLI is a flake package.** `nix profile install` puts a self-contained
  prefix on `PATH`: the binary plus the packaged campaign flow and driver it
  resolves relative to itself.
- **The daemon is a Home Manager profile.**
  `tally.homeManagerModules.product`, imported beside
  `tally.homeManagerModules.tally`, renders the campaign runtime a single
  machine needs and nothing a fleet needs.

Neither half reads a coordinator's deployed pin. Upgrading tally on one machine
is a profile upgrade of one flake input, not a fleet rebuild, and running a
campaign on an ordinary repository is no deploy at all.

## Install the CLI

```console
$ nix profile install github:mecattaf/tally.nix
$ tally --version
tally 0.1.0 (rev 64576e92…)
```

`--version` names both halves of the provenance question: the crate version the
workspace declares, and the source revision the build came from. The revision
is stamped into the build environment from the flake ref, so a profile install
reports the exact tree it was built from; a tarball or a plain `cargo build`
that never sees the flake reports the literal `dev`. Ask the program which tree
it is rather than reading store paths back to a pin.

The installed prefix carries more than the binary:

```text
bin/tally
bin/tallyd                       (symlink to tally)
share/tally/flows/spec-build.js
libexec/tally/spec-build-driver
```

The campaign verbs resolve those two assets relative to the directory the
running executable lives in — not relative to a checkout, and not through a
store path an operator had to know. On a profile install, `tally campaign arm`
therefore needs neither `--flow` nor `--driver`. A binary invoked from a build
tree, where that prefix does not exist, says exactly which path it probed:

```console
$ ./target/debug/tally campaign arm acme/notes silent-factory-worklists/night-notes.json
tally: packaged campaign flow is missing; probed …/target/debug/../share/tally/flows/spec-build.js
```

Upgrade and remove the entry like any other:

```console
$ nix profile upgrade tally
$ nix profile remove tally
```

## Start the daemon, fleet-free

The CLI admits work; it does not execute it. Anything that runs a job —
`adapter smoke`, `campaign arm`, `campaign poll` — reaches a coordinator daemon
over the Unix socket, and that daemon owns the pools, the leases, and the
witness ledger. The product profile is the smallest configuration that runs
one:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    tally.url = "github:mecattaf/tally.nix";
  };

  outputs = { home-manager, nixpkgs, tally, ... }: {
    homeConfigurations.alice = home-manager.lib.homeManagerConfiguration {
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      modules = [
        tally.homeManagerModules.tally
        tally.homeManagerModules.product
        {
          home = {
            username = "alice";
            homeDirectory = "/home/alice";
            stateVersion = "25.11";
          };
        }
      ];
    };
  };
}
```

There is no `services.tally` block, because the profile is one. That is not a
documentation shortcut: the flake check named `product-profile` evaluates the
two modules plus exactly the three attributes Home Manager demands of any
configuration, builds that activation package, and feeds the config it installs
to the daemon's own parser. Anything this page left out that a machine outside
the fleet would still need is something that check fails on.

Apply the generation and confirm the daemon and its pools:

```console
$ nix run github:nix-community/home-manager -- switch --flake .#alice
$ systemctl --user is-active tally-daemon.service
active
$ tally query pools | jq -r '.pools[].pool'
campaign-agent
campaign-control
flow
```

Those three are the whole runtime. `campaign-agent` is where agent lanes run
and the profile sets its capacity to `1`, because serialized dispatch is the
product default: one lane at a time spends a metered window on recorded work
instead of on four simultaneous attempts. A worklist's own `maxParallel` sits
under that ceiling, so raising this capacity is the only way to widen a
machine. `campaign-control` (capacity `4`) carries the flow's own bookkeeping —
worklist reads, gate dispatch, publication — which should never be what a pass
waits on. The per-campaign mutex is reserved, not declared: a pass holds the
capacity-1 `campaign/OWNER/REPO` pool for the identity it is reconciling.

The profile also names the rail it runs on. `services.tally.adapters.claude-code`
is stated explicitly rather than inherited silently, so the config a machine
deploys says which agent it dispatches; `pi`, `codex`, and `shell` are the
other presets in `nix/lib/adapters.nix`, and swapping rails is that one
attribute. The module layer supplies the rest: the packaged
`spec-build-driver` adapter, and `tally-campaign-poll.timer` on
[`services.tally.campaignPoll.interval`](../configuration/home-manager-options.md#servicestallycampaignpollinterval)
(60s by default), which is what turns a pushed worklist back into a running
pass.

What the profile deliberately renders empty is the fleet: no executors, no
declarative flows, no producers, no meters. Add them the day a second machine
exists.

## First proof of life

Before a campaign spends agent time, run one minimal job through the daemon's
real admission, lease, transient-unit, execution, capture, and witness path:

```console
$ tally adapter smoke claude-code --pool campaign-agent
```

`--pool` is explicit here for a reason worth knowing. Smoke infers a pool only
from a conventional lane — `claude-window`, then `claude-code` — and the
product profile declares neither. Naming `campaign-agent` is also the better
test: it is the lane campaign work will actually contend for.

Its verdict is three-valued on `verdictState`: `PASS`, `FAIL`, or
`VERDICT-UNAVAILABLE` when the result read exceeded its RPC deadline — the
third is never a statement about the adapter. See
[Adapter smoke](../operating/cli.md#adapter-smoke) for the full contract.

The stronger probe answers what a fixture cannot: whether this adapter, under
these policies, can do what a campaign implementation node must.

```console
$ tally adapter smoke claude-code --pool campaign-agent --assert-commit
```

It seeds a throwaway git repository, runs the adapter in it with a
write-stage-commit workload, and then requires exactly what publication
requires — a clean worktree and at least one commit descended from the seeded
base. A verified probe deletes itself; a failed one is retained as the
evidence, and the error names its path.

## What this path does not need

The product split exists so that none of the following is a precondition for
running tally on your own repository:

- a coordinator, a fleet flake, or a pinned deployed generation of tally
  belonging to someone else;
- a `services.tally.campaigns` declaration — that surface is for recurring
  estate-configured campaigns, described in [Campaigns](../flows/campaigns.md);
- a spec corpus, a frozen spec plane, or citation apparatus in the worklist;
- a per-repository flow script, dispatch wrapper, producer block, or
  serialization service.

With the daemon up and the smoke green, [A small worklist, end to
end](../flows/small-worklist.md) takes one ordinary repository from a scaffolded
worklist to a release.
