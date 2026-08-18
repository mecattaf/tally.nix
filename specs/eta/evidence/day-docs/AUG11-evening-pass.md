# AUG 11 — evening pass: the silent factory is ruled, planned, and staged

One session, morning to evening: Fossil read at source; the wayfinder suite and SSSF
re-examined; the estate's own history mined (the 2025 Git Utility Belt draft, the
July 11 no-ceremony ruling, the Aug 1 two-surface doctrine); nine research
deliberations and three audits run; and the whole thing distilled into rulings, a
merged plan, and armable worklists. This file is the session record. The plan is
`SILENT-FACTORY-PLAN.md` (57-ruling decision register, final state, five-chapter
action plan, merge addendum); the worklists are `silent-factory-worklists/ch*.json`,
all validated against origin/main's `normalize_task` with conflict domains required.

## The shape of the ruling, in one paragraph

Tally campaigns go silent: local canonical state end to end (worklist file as the
only decomposition home, local steering verb under embargo+cursor, attempt receipts
as append-only JSONL, completion = marker commit reachable from a local integration
branch), and GitHub is demoted to last-mile — one sparse release-announcement issue,
one PR stack rendered as a projection of the locally-integrated branch, fast-forward
push under the merge-control ruling, one receipts summary, close. The commit becomes
the projection format: Conventional Commits subject (six closed types, scope = closed
subsystem enum, 72-char header) over a kernel trailer block (`Tally-Task:`,
`Tally-Revision:`, `Tally-Receipt:` — public commits verifiable against the witness
ledger — and `Assisted-by:`). No versions (the flake pin is the contract), no releases
by default, `git log --oneline` is the changelog. The Git Utility Belt: dead as a
toolbelt, canonized as a grammar — tally is the belt worn on the inside, only the
buckle showing on GitHub.

## Rulings made today that supersede standing positions

- **git-ai is removed from tally entirely; only the ledger-rendered `Assisted-by:`
  trailer survives.** (Field finding en route: the checkpoint feed on this host is
  dead — 48 empty notes minted overnight onto unreachable lane commits. Resolution:
  descope, not repair.)
- **Python is terminal.** "There isn't a principled long-term case for python here."
  Rust replaces it right after the deletions: a `spec-build-driver` workspace crate,
  own binary, same argv seam, separately pinned, importing `campaign_contract`
  directly — the contract-corpus machinery deletes as the victory lap. Supersedes the
  Aug-10 keep-Python ruling. Pre-port gate: reseat the tests at the argv seam.
- **The GitHub-inbound trigger surface is DEAD** (ruled this evening): the producers
  GH stack (~6k LOC: gh_intake, gh_decision, orphan, the ghProducerType nix tree,
  gh-login) goes in full. Contingent worklist `chR` is activated as Chapter P.
- **Stack-as-projection**, not stack-as-merge-path (avoids the preview-API cliff, the
  O(n²/2)×~416 s re-gate bill, and trailer destruction; resolves squash-vs-bisectable
  permanently — both live in the never-rewritten local integration branch).
- **Schema squash rides this pass** (estate carries zero N-1 bytes; three items are
  shift prerequisites; witness float normalization and its neighbors untouchable).
- **No SQL database, ever, for the read model.** Option 1 ships instead: typed
  `nodeRole`/`subjectTaskId` capsule keys, `journalScope`, declared canonical/derived
  split, `durable_run_view` promoted and named `tally rebuild`.
- **Documentation is out of scope for this pass** — the docs redo round is queued
  separately; this pass lands first.

## The triple compilation and its merge

Three independent Fable compilers mined the full session transcript plus all
artifacts and produced the same four-part deliverable; the registers were diffed by
the session lead holding the complete conversation. **Zero contradictions.** Unique
catches were merged as addendum rulings D58–D61 (the backlog.md placement law; the
counter-recovery/capture-horizon demand; the CONTRIBUTING paste-clause deletion; the
reconciling doctrine sentence). Compiler A's plan is the base; its six worklists are
canonical.

## The compilation doubled as the first local-mode machinery test

Emitting the plan as real worklists surfaced seven impedance findings (plan §5.2),
the material three: cross-chapter dependencies are inexpressible in a worklist file
(operator arming discipline carries chapter order); conflict domains are enforced as
case-folded path prefixes against actual changed paths (symbolic labels decode fine,
then fail every lane — all emitted worklists use real path prefixes); and worklist
authority is a committed blob on the **remote** base branch even in fully local mode
— this plan and its worklists must be committed and pushed before anything arms.
Also expected and priced: the realizing campaigns themselves write the v1 marker
until their own `worklist-task-revision` task lands. Do not diagnose it mid-campaign.

## Sequencing (as staged tonight)

1. Fast-forward the local checkout onto origin/main (17 commits behind all session).
2. One typed hand commit: plan + worklists + this file (sanctioned exception under
   the merge-control ruling — the plan is the campaign's project document and
   worklist authority requires committed bytes).
3. Deploy the validated pin per the standing runbook (bump, build, switch, adapter
   smoke under manifest policies, one-task probe). Rollback is one generation.
4. One more forge-native campaign on the deployed pin — the last of its kind — to
   add campaign-hours and, if weather permits, answer the #455 machine-steering
   question on the substrate it was implemented for.
5. Chapters 1→2→P→3→4 as module-declared `forge:"local"` campaigns, `gh` off the
   driver's PATH; then the argv-seam reseat → Rust port chapter; read-model chapter
   rides anywhere post-deploy.

## Open items — ALL RULED, evening of Aug 11 (zero ambiguity remains)

- **#523** — cleared by the operator.
- **Off-host steering** — ruled: steering stays at the coordinator's keyboard;
  SSH into the coordinator is the off-host path. CH2 builds no new read surface;
  the red-team demand is satisfied by declared SSH access.
- **Scope enum vocabulary** — blessed; drafted from the conflict-domain
  vocabulary at the renderer chapter without further sign-off.
- **`agency_nightly_driver.py`** — rides the Rust port, sequenced: port begins
  once the overnight work is 100% committed and pushed; the final Python file is
  removed only after Rust is confirmed at feature parity with no loss of
  functionality (a successful port is the deletion gate).
- **Pin deploy tonight** — confirmed and authorized in the overnight handoff.

Session artifacts: two printed documents (The Silent Factory, 19pp; Appendix C,
8pp), nine deliberation reports, two shed-audit ledgers, three plan compilations and
their registers — preserved in the session record. Prepared 2026-08-11, evening.
