# nix/nixos-module.nix — nixosModules.tally: the ruled UNBUILT THIN WRAPPER STUB
# (IMPLEMENTATION-PLAN §1 item 8 / M3.3; SPEC "Flake outputs"; PS#17). This file OVERWRITES the
# layer-0 scaffold placeholder (the "scaffold creates, nix-module overwrites" handoff).
#
# "No system-level need is named yet; do not pre-build it (use-case precedes surface)" (SPEC). A
# stub here is COMPLIANCE, not a shortcut — everything tally owns is user-lifecycle, so
# `homeManagerModules.tally` is the primary, load-bearing module (SPEC "Flake outputs"). This
# nixos module deliberately renders NO system units and configures nothing: it exists so a future
# system-level need has a named surface to grow into, and it points the operator at the HM module.
#
# When a concrete system need lands (e.g. a machine-wide pls broker as a system service rather than
# a user service), THIS is where it grows — additively, behind new options. Until then it is a
# typed no-op that asserts loudly if enabled, so no one mistakes it for the working surface.
#
# No vendor code (clean-room, CLI-SURFACE §4).

{ config, lib, ... }:

let
  cfg = config.services.tally;
in
{
  options.services.tally = {
    enable = lib.mkEnableOption ''
      tally at the SYSTEM (NixOS) level.

      This is the ruled UNBUILT thin-wrapper stub (PS#17): tally is user-lifecycle, so use
      `homeManagerModules.tally` — the primary, load-bearing module. No system-level surface is
      built yet. Enabling this asserts, by design, so a future need grows here deliberately.
    '';
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = false;
        message = ''
          services.tally (nixosModules.tally) is the ruled UNBUILT thin-wrapper stub — no
          system-level surface is built yet (SPEC "Flake outputs": use-case precedes surface, PS#17).
          Use `homeManagerModules.tally` instead: everything tally owns is user-lifecycle (systemd
          USER units, per-user socket, per-user config). If you have a genuine system-level need,
          that is the deliberate moment to grow this module — additively.
        '';
      }
    ];
  };
}
