# test/nix/eval-nixos.nix — a standalone evaluator for nixosModules.tally (nix/nixos-module.nix),
# used by test/nix/hm-module.test.ts. The nixos module is the ruled UNBUILT thin stub (PS#17): it
# declares only `services.tally.enable` and asserts-on-enable, so this evaluator needs no NixOS
# option stubs beyond `assertions`. It surfaces whether the module's assertions pass for a given
# `services.tally` args set.
#
# No vendor code (clean-room, CLI-SURFACE §4).

{ pkgs ? import <nixpkgs> { }
, module ? ../../nix/nixos-module.nix
, args ? { }
}:

let
  lib = pkgs.lib;

  stub = { lib, ... }: {
    options.assertions = lib.mkOption {
      type = lib.types.listOf (lib.types.submodule {
        options = {
          assertion = lib.mkOption { type = lib.types.bool; };
          message = lib.mkOption { type = lib.types.str; };
        };
      });
      default = [ ];
    };
  };

  overrideModule = { ... }: {
    config.services.tally = args;
  };

  evaluated = lib.evalModules {
    modules = [ stub module overrideModule ];
  };

  cfg = evaluated.config;
  assertions = cfg.assertions or [ ];
  failed = builtins.filter (a: !a.assertion) assertions;
in
{
  assertionsPassed = failed == [ ];
  failedMessages = map (a: a.message) failed;
}
