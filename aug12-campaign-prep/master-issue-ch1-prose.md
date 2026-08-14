campaign: silent-factory-ch1 — squash prerequisites + contract fixes

First realizing campaign of the silent-factory pass (SILENT-FACTORY-PLAN.md,
decision register D38–D41). Chapter text from the plan, verbatim:

## Chapter 1 — Squash prerequisites + contract fixes (campaign, forge:"local", maxParallel 2)

| id | goal | targets | deps | domains |
|---|---|---|---|---|
| 1.1 `corpus-divergence-vectors` | Add conflictDomains/forbidPaths rejection vectors; align Python casefold-dedup to Rust exact; merge third forbidPaths copy (`normalize_forbid_paths_gate:6909`) into the canonical one (`:1411`) | driver `:926-946`, `:6909-6935`; `campaign_contract_corpus.rs`; `contract-corpus.json` | — | driver-py, contract-rs, fixtures |
| 1.2 `squash-legacy-checkpoint-tag` | Delete legacy checkpoint tag namespace (0 tags on origin) | driver `:2992,:3561-3574,:9030-9047`; campaign.rs `:3339,:3563-3600`; checkpoint-refs.json legacyTag | — | driver-py, campaign-rs, fixtures |
| 1.3 `worklist-task-revision` | Give file-worklist tasks a `revision` (computed as `task_completion_revision`) | driver `normalize_task:976`, source `:1155` | — | driver-py |
| 1.4 `marker-single-arm` | Collapse `pull_request_marker`/`_revisions`/`campaign_marker_prefixes` to one arm | driver `:2906-2940`; campaign.rs `:3266` | 1.3 | driver-py |
| 1.5 `drop-polluted-v2-migration` | Delete `migrate_polluted_v2` + dispatch; relax registry read to shared lock | campaign_registry.rs `:198,:259,:705-748`, tests `:1446,:1503` | — | registry-rs |

A sixth checkpoint task (`chapter-gate`, `bash test/fleet-gate.sh`) closes the
chapter with a full workspace check after all implementation tasks merge.

Line numbers in the goals were taken at plan time against `84786f4` and may
have drifted; the tree is authoritative. Read the cited symbols, not the line
numbers, when they disagree.

## Self-hosting notice

The workload is tally itself, so three versions are in play: the **installed
pin** that executes this campaign, **`main`** that lanes branch from, and each
**lane**. Workers must never run `nixos-rebuild`, `home-manager switch`, or
restart any `tally-*` unit — the daemon they would restart is the one grading
them. Merged work reaches the running system at a later deliberate deploy,
never during this run. The journal filter that usually isolates campaign
signal is unsound here: `cargo test --workspace` spawns `tally-job-fixture-*`
units carrying `TALLY_POOL=campaign-agent`. Corroborate against
`tally query run` and forge state only.

## Not in scope

No documentation work (D53), no changes outside the cited surfaces. Chapter 2
(local canon + git-ai removal) arms only after this campaign's terminal pass
closes this issue with receipts.
