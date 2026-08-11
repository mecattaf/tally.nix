# The campaign drivers and the worktree manager they share, in one store
# directory.
#
# `spec_build_driver.py` and `agency_nightly_driver.py` import
# `campaign_worktrees` as a sibling module, so they cannot each be packaged as
# a lone store path: a driver's own directory has to contain the manager. This
# derivation is that directory, and it is the only place either driver is
# resolved from.
{ pkgs }:

pkgs.runCommand "tally-campaign-drivers" { } ''
  mkdir -p "$out"
  cp ${../../drivers/campaign_worktrees.py} "$out/campaign_worktrees.py"
  cp ${../../drivers/spec_build_driver.py} "$out/spec_build_driver.py"
  cp ${../../drivers/agency_nightly_driver.py} "$out/agency_nightly_driver.py"
''
