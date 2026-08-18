# Install tally

This walkthrough uses the Home Manager module because that is the complete
deployed surface: it creates the user daemon, producer units, usage meters, and
declarative flows. tally also exports a NixOS module for a system daemon, but
that module deliberately does not generate producers, meters, or flows.

If what you want is tally on one machine, for campaigns on your own
repositories, read [Install tally as a product](product.md) instead. That path
installs the CLI with `nix profile install` and the daemon with the product
profile beside this module, and it needs no coordinator, no fleet flake, and no
deployed pin belonging to anyone else. The two are the same flake; nothing on
this page is a prerequisite for that one.

## NixOS module upgrade note

The NixOS module now creates a dedicated unprivileged service account through
`services.tally.user` and `services.tally.group` (both default to `tally`). Its
activation migrates existing root-owned tally data and state files to that
account, and its lingering user manager owns every transient job.

Two previously inert settings now have visible upgrade effects:

- `retention.enable = true`, the default, now installs and starts the system
  retention timer. Set it to `false` before upgrading if host-wide Nix store
  collection is not yet part of the machine's operating policy.
- `producers`, `flows`, and pool `usageMeter` settings now fail NixOS
  evaluation instead of being silently ignored. Move those settings to the
  Home Manager module, `tally.homeManagerModules.tally`.

The NixOS module also installs the event-drain timer. It remains the right
surface for a machine-wide coordinator daemon; Home Manager remains the
complete surface for producer, flow, and usage-meter units.

You need Linux with systemd, Nix with flakes enabled, and Home Manager. The
examples use `jq` only to make JSON output readable.

## Add the flake and one pool

In an existing flake, add the input and import
`tally.homeManagerModules.tally`. This is the smallest useful configuration:

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
        {
          home = {
            username = "alice";
            homeDirectory = "/home/alice";
            stateVersion = "25.11";
          };

          services.tally = {
            enable = true;
            pools.local = {
              resource = "build-slot";
              capacity = 1;
              enforce = "cooperative";
            };
          };
        }
      ];
    };
  };
}
```

Replace the username, home directory, system, and Home Manager state version
with yours. Keep an existing `home.stateVersion`; do not raise it merely for
tally.

The generated [options reference](../configuration/options.md) owns the option
types and defaults. Prose chapters use explicit values when a walkthrough
depends on them.

Apply the generation with your normal Home Manager command, or from a machine
without the `home-manager` command installed:

```console
$ nix run github:nix-community/home-manager -- switch --flake .#alice
```

Home Manager builds the JSON configuration with tally's production parser,
installs the package, and starts `tally-daemon.service`. Confirm both the daemon
and the configured pool:

```console
$ systemctl --user is-active tally-daemon.service
active
$ tally query pools | jq '.pools[] | {pool, capacity, held, queued, signal}'
{
  "pool": "local",
  "capacity": 1,
  "held": 0,
  "queued": 0,
  "signal": "GO"
}
```

The remaining commands in Getting started assume the Home Manager socket. With
the NixOS module, pass `--socket /run/tally/tally.sock` to the same CLI
commands.

## What the module just installed

The Home Manager implementation in `nix/modules/home-manager.nix` writes a
checked config, runs one coordinator daemon, owns its private data and state
directories, and creates the event-drain timer. `nix/modules/common.nix`
defines the shared typed graph and validates cross-references before
activation. The flake checks named `module-layer` and
`stock-host-activation` prove the rendered contract and a booted user service.
