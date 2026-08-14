campaign: flow-render-467 — static mermaid extraction from a checked flow (#467)

## What this is

The last forge-native campaign before the silent-factory shift. After this, campaigns
run `forge:"local"` against a committed worklist file and GitHub sees one
release-shaped act per campaign (SILENT-FACTORY-PLAN.md, D2–D8). This run exists to put
campaign-hours on the newly deployed pin and, if the weather turns, to answer the
standing #455 machine-steering question on the substrate it was implemented for.

Subject: **#467** — `tally flow render <script>`, conservative static extraction of a
checked flow's node graph as mermaid, no execution. Chosen because no plan chapter owns
it: #519/#520/#521 are owned by the plan's worklists, every open docs issue is out of
scope under D53, #523 is a standing queue item under D55, and the producers surface is
Chapter P's to delete. #467 is self-contained, has acceptance criteria of its own, and
is the least entangled with the line-anchored targets the realizing worklists cite.

## What it must prove

- The campaign path reconciles, dispatches, gates, publishes, and merges on pin
  `fxn0jyc…-tally-0.1.0` (system generation 120), which has never executed a campaign.
- The gate ladder runs green in a lane worktree of the repository the daemon serves from.
- The codex adapter commits under `sandboxPolicy: danger-full-access` in this
  repository's development shell.

## Pre-arm validation already performed (2026-08-11 evening)

| Check | Result |
|---|---|
| `tally adapter smoke codex --pool campaign-agent --assert-commit --sandbox danger-full-access --approval-policy never` | PASS — verdict `pass`, exit 0, commitProbe `verified` (1 commit, clean worktree), witnessSeq 1869 |
| Estate load before arming | all ten pools `GO`, 0 held / 0 queued |
| Daemon liveness on the new generation | `tally-daemon.service` active, status "tally daemon ready" |
| Manifest field check against the current contract | no unknown top-level or task fields (contract is `deny_unknown_fields`); all six gate ids unique |
| Gate ladder assets present | `test/cargo-deny.sh`, `test/fleet-gate.sh` both present and executable |

## Self-hosting notice

The workload is tally itself, so three versions are in play: the **installed pin** that
executes this campaign, **`main`** that lanes branch from, and each **lane**. Workers
must never run `nixos-rebuild`, `home-manager switch`, or restart any `tally-*` unit —
the daemon they would restart is the one grading them. Merged work reaches the running
system at a later deliberate deploy, never during this run. The journal filter that
usually isolates campaign signal is unsound here: `cargo test --workspace` spawns
`tally-job-fixture-*` units carrying `TALLY_POOL=campaign-agent`. Corroborate against
`tally query run` and forge state only.

## Not in scope

No graph surgery, no parallelism (`maxParallel: 1`), no documentation work (D53). If
this campaign needs an operator intervention of any kind, that fact is the finding.
