# The product profile: everything a single machine needs to run tally as its
# own thing, and nothing a fleet needs.
#
# This is the fleet-free half of the product split. The other half is the CLI:
# `nix profile install github:mecattaf/tally.nix#tally` (equivalently the
# flake's default package) puts a self-contained prefix on PATH — the binary
# plus `share/tally/flows/spec-build.js` and `libexec/tally/spec-build-driver`,
# which the campaign verbs resolve relative to their own executable. Neither
# half reads a coordinator's deployed pin, so upgrading tally on one machine is
# a profile upgrade of one flake input, not a fleet rebuild, and running a
# campaign on an ordinary repository needs no deploy at all.
#
# Usage — import it beside the tally module, not instead of it:
#
#   home-manager.users.<you>.imports = [
#     tally.homeManagerModules.tally
#     tally.homeManagerModules.product
#   ];
#
# Every value below is an ordinary definition rather than a `lib.mkDefault`,
# because the module layer already supplies the defaults this profile is
# choosing against; override one with `lib.mkForce`.
self:
{ ... }:

{
  services.tally = {
    enable = true;

    # The two pools the campaign flow's nodes name — grep
    # `examples/flows/spec-build.js` for `pools:` and these are the whole list.
    # Their `resource` kinds come from the module layer's campaign runtime
    # defaults (`nix/modules/common.nix`, `mkCampaignRuntimeConfig`); a profile
    # that restated them would be a second place for them to drift. What the
    # profile does state is width, because width is the operator's choice and
    # a single machine's answer is not a fleet's.
    pools = {
      # given: operator pool capacity — control lanes are the flow's own
      # bookkeeping (worklist reads, gate dispatch, publication), cheap enough
      # that they should never be what a pass waits on.
      campaign-control.capacity = 4;
      # given: operator pool capacity — one agent lane at a time is the
      # product default. Serialized dispatch is what the eta run log records as
      # having survived a budget wall (specs/eta/evidence/run-log.md, the rate
      # reconciliation entry): one lane at a time spends the window on recorded
      # work instead of on four simultaneous attempts. A worklist's own
      # `maxParallel` sits under this ceiling, so raising it here is the only
      # way to widen a machine.
      campaign-agent.capacity = 1;
    };

    # The profile's one catalog adapter: the name a worklist's `agent.adapter`
    # resolves against. Stated explicitly rather than inherited from the
    # preset defaults, so the config a machine deploys names the rail it
    # actually runs on; swapping rails is this one attribute (`pi`, `codex`,
    # and `shell` are the other presets in `nix/lib/adapters.nix`).
    adapters.claude-code = self.lib.adapters.presets.claude-code;
  };
}
