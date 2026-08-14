# The grandfathered agency-nightly driver and its worktree manager, in one
# store directory.
#
# `agency_nightly_driver.py` imports `campaign_worktrees` as a sibling module,
# so its store directory has to contain the manager. This derivation is that
# directory and the only place the grandfathered driver is resolved from.
{ pkgs }:

pkgs.runCommand "tally-campaign-drivers" { } ''
  mkdir -p "$out"
  cp ${../../drivers/campaign_worktrees.py} "$out/campaign_worktrees.py"
  cp ${../../drivers/agency_nightly_driver.py} "$out/agency_nightly_driver.py"
''
