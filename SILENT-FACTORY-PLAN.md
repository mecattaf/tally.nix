# tally.nix — The Silent Factory Pass: Final Plan (merged)

Compiled 2026-08-11 from the session transcript (primary authority), the session artifacts
(silent-factory.md, silent-factory-appendix-c.md, shed-twin-A/B.md, wf-0..wf-8), and the
repository at `origin/main` = `23509ab`. The local checkout is 17 commits behind origin/main;
every file:line below is an origin/main reference. This document is intended to become the
in-repo project document of the realizing campaigns (Decision D47).

---

# PART 1 — DECISION REGISTER

Every ruling is stated once, with what it supersedes and its source. "Operator" = Tom's own
words in the transcript; "ratified" = proposed by an agent/deliberation and accepted by the
operator's subsequent direction without objection. Rulings marked **PENDING** are not made.

## A. Substrate and architecture

- **D1 — No Fossil migration.** Fossil is a remix source only; no SCM switch, no autosync/cathedral model, no PR-gate rebuild. Settles the opening question of the session. *(Transcript: assistant verdict, operator ratified by redirecting to "what to steal".)*
- **D2 — The campaign interior goes private.** "The push history doesn't have to exist in GitHub. The public doesn't need to see my per-issue breakdown in a tally run that is ultimately wrapped into a 'master issue.'" GitHub issues are conceptually upgraded: **not** the home of a long tally-flow decomposition. *(Operator, transcript msgs 222 and 384.)*
- **D3 — The silent factory operating model.** Tally does the work silently, locally; then one full high-quality GH issue → PR merge, following simple conventional commits, squashing many items. GitHub becomes a **release target, not a coordination medium**; compounding happens in-process, GitHub is last-mile. *(Operator, msgs 269, 384, 560.)*
- **D4 — The issue is a release announcement.** Sparse (title; operator intent prose rendered verbatim from the project document; link to the merged stack; closing receipts summary), conventional, never self-referential. Falsifiable test: *could this issue exist unchanged in a repository that never heard of tally?* *(wf-7 §5, ratified.)*
- **D5 — No public chatter; the mention doorbell dies for silent campaigns.** No progress comments, no at-mention assignment ("feels clunky, like this was done impromptu"), no phone-following. `tally campaign arm` is the doorbell, rung locally. Public-native remains a *mode*, not the architecture. *(Operator, msg 384; silent-factory §5.2.)*
- **D6 — Promote, don't build.** `repositoryConfig.forge = "local"` already runs campaigns end-to-end with zero `gh` (it is the 6,263-line driver test suite's default harness). The shift is promotion of that mode to production default — armable, identified, steerable without gh — not a new backend. *(Appendix C.3, ratified.)*
- **D7 — Merge-control unchanged.** `main` changes only through a campaign merge; the campaign's merge is the local integration the gates witnessed. The `git push` to origin per chapter merge is unavoidable and correct. *(wf-7 §6, C.7.)*

## B. GitHub's remaining role

- **D8 — Exactly three structurally-obliged forge contacts** for the realizing work: (1) `git push` per chapter merge; (2) one release act per chapter (issue + stack + merges + summary + close); (3) scratch-repo lifecycle for renderer probes. Forbidden for the duration: gh issue tracking, sub-issues, per-task PRs, progress comments, checkbox masters, steering via GitHub. A needed GitHub *read* mid-campaign is a design finding, not a call to make. *(wf-7 §6, ratified.)*
- **D9 — Transient GitHub repos for testing are structurally required.** Two patterns: probe campaigns in throwaway `tally-probe-<date>-<short>` repos; a resettable stack-renderer probe repo. Six lifecycle rules: one name prefix so teardown is a glob; created/deleted by the same verb; renderer mechanically refuses non-armed-repo targets unless `probe` flag + `tally-probe-*` match; 7-day TTL sweep; private and never a fork of the real repo; no scratch state read back except a pass/fail line. *(Operator "entire transient gh repos for testing purposes", msg 384; wf-7 §4.)*
- **D10 — Local working trees favored.** Lanes, integration branches, worklists, receipts, steering: all local; the operator explicitly favors "local working trees" over public process. *(Operator, msg 384.)*

## C. Steering, receipts, completion

- **D11 — Steering relocates to a local append-only JSONL + `tally campaign steer` verb**, consumed at attempt-prep under an **embargo + high-water cursor** contract (Fossil hook-embargo/hook-last-rcvid shape). The existing fold (prepared∪rechecked, edit detection, late-ID witnessing, `authorized_steering_comments` record shape) moves byte-for-byte; only transport changes. The steering-race class collapses structurally. *(silent-factory §6.2; wf-7 C1; red team blocker 1, ratified.)*
- **D12 — Steering must be campaign-proven with an off-host read path before any real mission runs dark.** Human steering is the only steering with evidence behind it; a steer verb usable only at the coordinator's keyboard is a regression against a phone. Escalation acquires a non-forge delivery channel (push notification, email, or the morning `tally query` ritual). *(Red team blocker 1; silent-factory §5.2 escalation; ratified via C.7/C.8.)*
- **D13 — Attempt receipts split by reachability.** Checkpoint and merge receipts stay as hidden refs (must be reachable from the integration branch). Attempt *counters* (diagnosis/retry/escalation/pardon) move to an append-only campaign-scoped JSONL with flock-and-fsync discipline; `contiguous_receipts` and pardon-boundary arithmetic become a fold over an ordered log. The ref-namespace walk is deleted **in the same task** or the added store is unpaid ceremony. *(wf-7 C2, ratified.)*
- **D14 — Completion oracle is local:** marker commit reachable from the local integration branch (`merged_local_tasks`). The public completion fact is re-established once, at release. *(silent-factory §4.3; wf-7 C4.)*
- **D15 — Integration branches replace the shared remote base** as lane merge target; `merge_local` drops the temp-clone dance but **keeps the `actual_base`/`actual_head` re-verification race guard**; publication URLs become `local://repo/branch`; `stable_publish_branch` survives with its `issue` argument re-keyed to a campaign id. *(wf-7 C4.)*

## D. Commit grammar, markers, trailers

- **D16 — Conventional Commits subject + kernel trailer block.** The "CC vs kernel style" binary is false — CC footers *are* git trailers. Subject = the cheap one-token-classified index; trailer block = the machine channel. *(Convention scout, ratified msg 606.)*
- **D17 — Six closed types:** `feat fix refactor docs build chore`; decision procedure "does a consumer need to care?" (`feat`/`fix` yes, rest no). Amendment required to extend. *(Convention scout, ratified.)*
- **D18 — Scope is a closed per-repo enum of 5–15 durable subsystems — never the task name.** Tally's current `taskname: subject` puts a never-repeating identifier in the scope slot (scope cardinality = commit count, indexes nothing). Task IDs move to the `Tally-Task:` trailer. Scopes come pre-disjoint: conflict domains are the scope vocabulary, enforced by the scheduler, not by a linter. *(Convention scout + belt ruling, msgs 606/673.)*
- **D19 — The trailer block:** `Tally-Task:` (ledger task id), `Tally-Revision:` (stable id surviving rewrite), `Tally-Receipt:` (witness hash — the keystone: every public commit verifiable against the local ledger), `Assisted-by:` (model id; never `Co-authored-by:`, which pollutes contributor graphs), plus `Fixes:`/`Closes:` when applicable and `BREAKING-CHANGE:` (hyphenated — the spaced form is not a valid trailer token) only with `!`. Trailer block must be one contiguous block at the very end; validate it. *(Convention scout, ratified.)*
- **D20 — Header rules:** 72-char hard cap; imperative, lowercase, no period; `!` retargeted to mean "advancing your flake pin past this requires action". *(Convention scout, ratified.)*
- **D21 — Completion markers become git trailers.** The `<!-- tally:spec-build:v2 -->` HTML-comment grammar dies; `Tally-Task:`/`Tally-Revision:` trailers are the oracle key. One schema for the public record instead of a human grammar plus a hidden machine grammar. The respell is **atomic at the shift moment, ordered last in its chapter, never split across passes** (the marker is the oracle's key). *(Msg 554 wrap-back, operator-prompted; wf-7 C5.)*
- **D22 — Conventional commits are policy estate-wide** (every project's GitHub-facing surface); GitHub Releases are **not** enforced for any project. *(Operator, msg 572.)*
- **D23 — GitHub squash-merge is banned on projection repos** — it composes a new message from PR title + concatenated bodies and destroys the trailer block. Merge/rebase/ff preserve it. Another independent vote for stack-as-projection with fast-forward push. *(Convention scout, ratified msg 606.)*

## E. Versioning, releases, changelogs

- **D24 — No versions declared; the flake pin is the contract.** `flake.lock`'s rev+narHash is the honest form; version strings, where needed, are projections: `0.0.0+<lastModifiedDate>.<shortRev>` from flake `self`. Tags are bookmarks. ZeroVer (0.0.x) kept. SemVer-as-mechanism rejected. *(Convention scout, ratified; belt ruling msg 673.)*
- **D25 — No releases by default; `git log --oneline` is the changelog.** No `CHANGELOG.md` in-tree, ever. The rare publishing project gets a human-triggered GitHub Release body rendered per Common Changelog style (feat/fix/! only), with **git-cliff as conformance oracle only** — never the changelog layer (ledger → commit text → parse → changelog is a lossy round trip through the narrowest pipe). *(Convention scout, ratified.)*
- **D26 — The validator is ~150 lines of Rust in the renderer** (eight commitlint-shaped rules: type-enum, scope-enum, subject-case, subject-full-stop, header-max-length, body-leading-blank, footer-leading-blank, trailer-block-wellformed), exposed as `tally lint-history <range>` for on-demand audit. No hooks, no CI, no commitlint/commitizen/cocogitto dependency. *(Convention scout, ratified.)*
- **D27 — Git Utility Belt: dead as a toolbelt, canonized as a grammar.** The 2025 thesis (commit message as machine-readable interface) is ratified; every install (Semantic PR app, release-please, Actions CI/deploy/cleanup, templates/labels/milestones, enforcement) is rejected forever. The dataflow inverts: tally *renders* commits from the ledger — the conventional commit is the belt's **output format, not its input**. release-please dies as machinery, its release-PR pattern is absorbed into the renderer. "Tally is the belt, worn on the inside, with only the buckle showing on GitHub." *(Operator question msg 670; ruling msg 673, closing the arc the operator demanded in msg 552.)*

## F. Stacked PRs and the release surface

- **D28 — Stack-as-projection, not stack-as-merge-path.** Integrate locally, fast-forward push under the merge-control ruling, render the stack for layer-by-layer review; "merged bottom-up" is presentation. Avoids the preview-API cliff (stacked PRs are public preview), the O(n²/2)×~416 s cascading re-gate bill (~10 h for a 14-task chapter), and (formerly) git-ai unbinding; keeps split campaigns viable. *(Red team blocker 3, recommendation ratified msgs 533/606.)*
- **D29 — The squash-vs-bisectable-train conflict is resolved:** bisectability and authorship live in the **local integration branch, never rewritten**; the public stack is a projection and its merge method a rendering choice. *(Appendix C.5 arbitration, simplified by D31.)*
- **D30 — Release renderer acceptance test is G1-shaped (Igalia):** with tally absent, the released repository must be **indistinguishable from a disciplined solo developer's repo on a good week**. Public repo = upstream; campaign interior = downstream carry (P6 modifying-delta logic; P9 isolated layer; Pink Elephant absence-over-prohibition). *(wf-0/Appendix C.2, ratified.)*

## G. git-ai

- **D31 — git-ai is removed from tally entirely; only the ledger-rendered `Assisted-by:` trailer survives.** Gates A and B, the per-campaign private daemon, `reconstruct_squash`, `verify-authorship` verb + `authorship.rs`, `GitAiConfig`, all six nix leaf options + the await-coupling assertion, the d12 flake scenario, squash-fidelity doc+script, final-bar case, fixtures, and ~1,500–2,000 LOC of tests. Supersedes Appendix C.5's "fix the checkpoint feed" and `gitAiBinding` design, and the assistant's "keep for personal tooling" tail ("we're rescoping tally right now"). Rationale is structural: one lane = one agent = one worktree = one witnessed commit — authorship is guaranteed by process, not recovered by observation; gate B's problem (forge squashes destroying notes) is unconstructable after the shift. *(Operator, msgs 552/560: "git-ai one-liner remains (Assisted-by) but the rest is removed.")*
- **D32 — The `Assisted-by:` trailer builder must be relocated before producers are touched** — the Rust builder lives at `producers/gh_intake.rs:101-154` (`assisted_by_from_evidence`, `AssistedBy::trailer`) + `producers/engine.rs:986,1049`; the driver's own builder (`spec_build_driver.py:145-165, 696-735`) stays in place. *(Shed twins, merged ledger, ratified msg 656.)*
- **D33 — Witness `authorship` field *acceptance* is removed last, deliberately, under a ledger-compat plan.** Live chain-hashed records carry `authorship`/`authorshipSessions`; emission and required-validation go now, read-side acceptance survives or historical chain verification breaks. Same hazard class as the float normalization. *(Shed twin A G7 / twin B A11, ratified.)*

## H. Language, driver, tests

- **D34 — Python is retired: "there isn't a principled long-term case for python here. we'll move away from it right after the deletions."** Supersedes the Aug-10 keep-Python ruling and the python-verdict's "keep until named triggers." Python survives only through the deletion chapters, on sequencing (don't rewrite code that's about to die), not on merit. *(Operator, msg 662.)*
- **D35 — The replacement language is Rust — "and it's not close."** JS is forbidden twice over (Boa's effect-free doctrine; Node's runtime/supply chain vs stdlib-only discipline). Shape: a small `spec-build-driver` crate in the existing workspace, compiled to its own binary, behind the **same argv seam** (`TALLY_BRIEF` in, one `TALLY_FINAL_MESSAGE=` JSON line out), **separately pinned** as its own store path (the registry models flow and driver as two store paths — never folded into the tally CLI), importing `campaign_contract` directly. `campaign_worktrees.py` ports **with** the driver, never first (shared with the nightly driver, zero contract mirroring, worst test coupling). `agency_nightly_driver.py` rides the same port or is explicitly grandfathered — flagged, never silent. End state: **Rust for everything that does, Boa JS for the effect-free flow scripts that decide, zero Python.** *(Operator asks msg 662; ruling msg 664.)*
- **D36 — Port sequence:** deletions land → tests reseat at the argv seam (`run_driver(action, brief)` helper; the 49 action-only tests migrate; `main()`/`load_brief()`/`emit()`/DriverError-exit currently have zero coverage) → port driver + worktrees → **delete the contract-corpus machinery as the victory lap** (generator, byte-compare tests, the regex that re-parses Rust source). The port deletes the Python contract mirror entirely; the JS argsSchema is single-sourced from Rust in the same chapter (#520 direction), taking four contract copies toward one. *(Msgs 660/664, operator-ratified.)*
- **D37 — Test harness carries over untouched:** real `git init` repos in tmpdirs and PATH shims (FakeGitHub/FakeTally/GIT_AI_SHIM patterns) are language-agnostic; the Rust host idioms already exist (`flow_live.rs:4862` shells the driver; `TALLY_GH_PROGRAM` fake-gh in `campaign.rs:4040`). *(python-verdict, ratified.)*
- **D38 — Two live contract divergences are fixed pre-shift, cheaply, under the corpus:** `normalize_conflict_domains` dedupes casefolded in Python vs exact `BTreeSet` in Rust (and the corpus has zero `conflictDomains` rejection vectors); `normalize_forbid_paths_gate` is a third forbidPaths copy missing the trailing-slash check. ~30 lines of corpus vectors + merging the third copy into the first. *(python-verdict, ratified as pre-Chapter-1 items, msg 628.)*

## I. Schemas and hygiene

- **D39 — Schema squash now.** The estate carries **zero N-1 bytes anywhere** (1,772 witness records all schema 2; 1,775 durable rows all rowVersion 5; 0 legacy checkpoint tags; 0 v1-marker PRs). Tier A deletes outright: legacy checkpoint-tag fallback; `migrate_polluted_v2` (then relax registry reads to shared locking); GhOrigin/GhContextSnapshot v0/v1 acceptance; `capture_migration.rs` + `unit_exit_migration.rs` + the `tally migrate` verb (after one `--plan` run confirms `isClean`); pre-split `.err` fallbacks. Tier B: rowVersion ladder squashes to a floor-refusal, **keeping the migration frame** (CONTRIBUTING policy). *(Operator instinct msg 384(2); schema census, ratified.)*
- **D40 — Three squash items are shift prerequisites and land first:** (1) legacy checkpoint tag namespace (before the local completion oracle); (2) file-worklist tasks gain a `revision` **before** the v1 marker arm is collapsed — the v1 spelling is what file-worklist mode *writes today* (`normalize_task` sets no revision → `pull_request_marker:2911` emits v1), and file-worklist is the mode being promoted to default — a live-fire trap; (3) `migrate_polluted_v2` deletion before the authority bump (never bump into a ladder carrying a repair arm). *(wf-7 §2, ratified.)*
- **D41 — Authority schema v3:** drop `issue_url`, `issue_number`, `sub_issue_walk`, `last_forge_observation`, `allow_test_local_forge`; add worklist pattern + code repository + local actor. N/N-1 tests move with it; the pre-v3 refusal names the remedy ("disarm and re-arm"). *(wf-7 2.1, ratified.)*
- **D42 — Tier D untouchables:** `normalize_witness_numbers`/`stable_canonical_value` (1,771/1,772 chain hashes depend on the legacy f64 spelling — the comment *is* the mechanism); witness/events `OldFormat` boot refusals (interlocks); flow-membership forward tolerance (rollback survival); `SubmissionOptions` Legacy-vs-Full (frozen until full-mode-universal is ruled); `taskdb/migrations.rs` frame. Ledger-touching cuts go **last, inside the shift's own cut, priced once** — after the ledger becomes sole authority, a clean-cut refusal is permanent capability loss. *(Census + red team repricing, ratified.)*

## J. Read model and SQL

- **D43 — The SQL question is closed: no database.** No SQLite read model (it would create the one operator rule that today cannot exist — "if the view disagrees with the ledger, run rebuild" — and cannot hold git reachability or unit liveness; the estate's own default gate forbids `*.db`/`*.sqlite*`); no `tally export sqlite` yet (deletes zero rules); ad-hoc forensics = `duckdb`/`jq` over the JSONL as-is. Confirms the operator's PS: "I don't want to reconstruct an sql database just because fossil has it." *(Operator msg 384 PS; wf-6, Appendix C.6.)*
- **D44 — Read-model option 1 ships:** (a) declare canonical vs derived in one tested place; (b) **the load-bearing piece** — type the orchestration capsule's semantic keys (`nodeRole`: agent|merge|checkpoint-record|reconcile|prep|cleanup|sweep|continue, plus `subjectTaskId`), additive alongside the existing label, killing #482 and the `label == format!("merge-{task_id}")` seams at the source; (c) a `journalScope` field carrying flowRunId (makes #483/#519 expressible); (d) promote `durable_run_view` to primary, give it unit-liveness facts, name it **`tally rebuild`**. Explicitly do **not** grow `query_v2`'s 15-endpoint surface (tally's analogue of Fossil's rotted JSON API); the `query_v2` rename is dropped entirely (pure churn). This chapter rides anywhere after the deploy; orthogonal to the substrate. *(wf-6/C.6, ratified.)*

## K. Models and intent

- **D45 — No model enters the closing procedure.** Every release step is a pure function already in tree (stack order from dependency topology, branch names from `stable_publish_branch`, receipts from `campaign_digest`, idempotency from markers). The one prose slot — per-PR narration — keeps its existing cage (`narrate`, ≤2 attempts, closed type enum, deterministic `template_narration` fallback, model architecturally optional). If a small local model is ever wanted it enters as an adapter-table argv swap at the existing steward seam: zero new mechanism. Resolves the operator's "distill the gh last mile to a small local model" to: the last mile is deterministic code that already exists. *(Appendix C.3, ratified.)*
- **D46 — The campaign issue's intent prose is the operator's project document rendered verbatim.** A model writing the "one high-quality issue" would originate the campaign's rationale — the doctrine violation. Tally never originates intent. *(wf-7 §3, ratified.)*

## L. Sequencing, pre-flight, and the realizing campaign

- **D47 — The cut plan must exist in-repo before dispatch.** 582 audited doc claims currently describe the *opposite* system as live doctrine; the plan on paper becomes the campaign's project document or the doc-as-oracle census becomes authoritatively wrong. This document (and its worklists) is that artifact; the triple compilation is itself the first test of local (no-GH-mention) tally operations. *(python-verdict caveat, given "top-of-queue status" msg 628; operator msg 675.)*
- **D48 — Pre-flight order:** (1) deploy the validated pin — first, alone; (2) one more forge-native stormy campaign on the deployed pin to answer the standing #455 machine-steering question on the only substrate with campaign-hours — never move the substrate and refactor the driver in the same campaign; (3) rebase the local checkout onto origin/main (17 commits; `drivers/` relocation) or every worklist is written against phantom paths. *(Red team blocker 2 + C.8 + wf-7 Chapter 0, ratified. C.8's "fix the git-ai feed" step is superseded by D31.)*
- **D49 — The realizing campaigns run as module-declared, `forge:"local"` campaigns under `--allow-test-local-forge`, with `gh` deleted from the driver's PATH** so any stray forge dependency fails loudly. The flag's own deletion (2.7) is ordered last in Chapter 2, after authority v3 makes it meaningless. Chapter 3 self-hosts: the release renderer's first real use is releasing itself. *(wf-7 §6, ratified; operator: "all these tally updates would be realized in a tally run — one that would deliberately not use gh (unless we're now structurally obliged).")*
- **D50 — Rule-of-residue accounting binds the pass:** eight operator rules deleted (checkbox-drift checks, comment-thread archaeology, sub-issue capability probes, marker-PR labeling, dual receipt namespaces, polluted armed records, dual capture names, GitHub-only resume) against one store and one verb added. The shift halves coordination residue and does nothing for execution residue — claimed honestly. *(C.7, ratified.)*
- **D51 — Both operator skills are rewritten from scratch at the shift** (they script the forge lifecycle; every rule is a confession about the substrate). The skills shrinking is the acceptance test, measured in the diff. *(silent-factory §2/§9; twins S4/B10.)*
- **D52 — Filed, not built:** the backoffice lease (leased daemon-less control with pid-liveness reclamation) and unversioned-content semantics — designs on record, built only when re-arm concurrency / cross-host capture replication exist. `tally ui` remains an ambition that stands on D44's substrate, not part of this pass. *(silent-factory §6.7.)*

## M. Explicitly out of scope for this pass

- **D53 — ALL documentation work.** The documentation redo round is already lined up as future GH issues; "docs that the twins would have seen are moot anyway"; and this pass "will happen before ANY documentation pass is written at all." Consequence: the campaigns.md rewrite, the doc/audit/campaigns-*.md shedding, root note archiving, and mutation-ladder fold-in are all deferred to the docs round. Only whole-file deletions of docs whose *subject code is deleted in the same chapter* (git-ai-authorship.md, squash-fidelity doc) ride this pass. *(Operator, msgs 658 and 675.)*
- **D54 — No new coordination surfaces:** no SQLite, no `tally export`, no generic query filter language, no closing-model, no backlog.md-shaped artifacts. *(D43/D44/D45/D52.)*
- **D55 — `#523` (lineage prose / `tally note` technotes) stays a standing queue item** — the root `AUGUST-*.md` question defers to it; the tree stays code either way; not part of this pass. *(Twins adjudication, ratified msg 656.)*

## N. Pending

- **D56 — PENDING (the one-word ruling): the producers GH stack.** `producers/gh_intake.rs` (2,145), `gh_decision.rs` (739), `orphan.rs` (339), the `ghProducerType` nix tree (~370 LOC, 9+ leaf options), `gh-login.nix` — ~5,700 LOC Rust + ~400 nix, ~16–25 leaf options. Twin A refused to cut on its own authority (general gh-trigger surface, not campaign narration); Twin B condemned with evidence (zero non-campaign gh producers configured in-repo; only consumer is the campaign mention doorbell the operator already called clunky). The question awaiting the operator's one word: **does any GitHub-inbound trigger surface survive at all?** Yes/no settles ~6k LOC. Prerequisite either way: D32's trailer relocation, plus deciding the home of the thin outbound gh client the release act needs. *(Merged shed verdict, msg 656 — operator has not answered.)*
- **D57 — PENDING (one grep, not a ruling): `build-effect` / `pool-reachability` producer kinds** (~1,100 LOC) — dead in-repo; check the deployment flakes for `kind = "build-effect"` / `"pool-reachability"` before cutting. Also: `test/eval_manifest_check_test.py` — wire into a check or delete (Twin A: reached by nothing; the script it tests is live via final-bar). *(Twins, ratified as pending verifications.)*

---

# PART 2 — FINAL STATE

## What tally.nix is after the pass

A **silent factory**: the campaign machinery runs entirely against local canonical state, and
GitHub sees exactly one release-shaped act per campaign. One toolchain — **Rust for everything
that does, Boa JS for the effect-free flow scripts that decide, zero Python**. The codebase is
~26–34k LOC smaller (subtraction-dominated pass, per the operator's stated preference).

## A campaign run, end to end

**Arm.** The operator writes a worklist JSON (`{schemaVersion:1, tasks:[...]}`) into the
repository and merges it to the base branch. The campaign is module-declared in nix
(`forge:"local"`, worklist glob, maxTasks/maxParallel). `tally campaign arm` is the doorbell —
no issue, no mention, no projection. The armed authority record (v3: worklist pattern, code
repository, local actor — no issue_url/issue_number) pins the worklist at the remote base
revision, hashed into the witness as `{path, sha256, revision}`.

**Work.** Reconcile passes re-derive the whole board from local durable facts: completed =
marker-trailer commits reachable from the **local integration branch** + checkpoint refs;
blocked = diagnosis + retry receipts in the **attempt-receipts JSONL**; frontier = first
maxParallel ready tasks with disjoint conflict domains. Lanes are cut as local worktrees; each
lane squashes locally into **one conventional-commit layer commit** carrying the full trailer
block (`Tally-Task`, `Tally-Revision`, `Tally-Receipt`, `Assisted-by`). Merges land on the
integration branch under witnessed command gates — never a model's opinion. Steering is
`tally campaign steer` appending to a local ordered log; attempt-prep folds it into the
immutable brief under embargo + high-water cursor; an off-host read/notify path covers the
phone case. Escalations notify via a non-forge channel. Nothing — nothing — reaches GitHub
while work is in flight.

**Release.** One public act, rendered from the ledger by deterministic code: create one sparse
issue (title; operator's project-document prose verbatim; nothing else), fast-forward push the
integration result under the merge-control ruling, render the PR stack as **projection**
(layer-by-layer review surface; "merged bottom-up" is presentation), post one receipts summary
rendered from `campaign_digest`, close the issue. The commit-grammar validator (~150 lines
Rust) holds authority at the render; `tally lint-history` audits on demand. Acceptance test,
G1-shaped: the repository is indistinguishable from a disciplined solo developer's.

## What GitHub sees / never sees

| GitHub sees | GitHub never sees |
|---|---|
| One sparse issue per campaign (release announcement) | Manifests or worklists in issue bodies |
| One stack of conventional-commit PRs, ff-merged | Sub-issues, checkbox projections, progress bars |
| Trailer blocks on every commit (ledger-verifiable) | Steering, diagnosis, retry, escalation, pardon comments |
| One closing receipts summary comment | Marker PRs, capability probes, attempt counters |
| `tally-probe-*` scratch repos (TTL'd, private) | Per-lane branches, restamps, narration, HTML-comment markers |

## The commit grammar, in full

```
<type>(<scope>)[!]: <imperative subject, lowercase, no period>   # ≤72 chars

<body: rendered from receipts, wrapped 72, why-not-what>

Tally-Task: <ledger task id>
Tally-Revision: <stable id, survives rewrite>
Tally-Receipt: <witness hash>
Assisted-by: <model id>
Fixes: <abbrev-sha> ("<subject>")          # when applicable
Closes: #<n>                               # only on the commit closing the sparse issue
BREAKING-CHANGE: <migration note>          # only with !
```

Types: `feat fix refactor docs build chore` (closed). Scopes: closed per-repo enum of 5–15
durable subsystems (= conflict-domain vocabulary). No releases by default; flake pin
(rev+narHash) is the contract; derived version strings `0.0.0+<date>.<shortRev>`; tags are
bookmarks; `git log --oneline` is the changelog.

## Language map

| Layer | Language | Notes |
|---|---|---|
| Core, daemon, CLI, contract | Rust | `campaign_contract` is the single contract source |
| Campaign driver | Rust (`spec-build-driver` crate, own binary, separately pinned) | same argv seam: `TALLY_BRIEF` in, `TALLY_FINAL_MESSAGE=` out |
| Worktrees | Rust (ports with the driver) | never ported first |
| Flow scripts | Boa JS, effect-free | keeps failure-class pricing + witnessed merge criterion |
| Nix modules | Nix | campaign declaration = estate configuration |
| Python | **none** | corpus machinery deleted with the port |

## Canonical vs derived

Canonical (append-only, flock+fsync, JSONL under XDG): witness ledger (hash-chained, sole
proof surface), campaign registry + approved-graph snapshots, attempt-receipts log, steering
log, lifecycle/membership/attestations/events, captures. In-git canon: integration branches,
checkpoint/merge receipt refs, trailer-marked commits. Derived (regenerable, never
load-bearing): everything `query`/`query_v2` serves, `tally rebuild` = promoted
`durable_run_view` + unit facts, the public rendering itself. Typed `nodeRole`/`subjectTaskId`
replace label-grammar seams; `journalScope` makes campaign-scoped journal reads sound. No
SQLite anywhere; duckdb/jq for ad-hoc forensics.

## Operator skills

Both skills shrink to small documents: arm (write worklist, merge it, `tally campaign arm`),
observe (`tally query` / `tally rebuild` — the corroboration ritual is deleted), steer
(`tally campaign steer`), release (one verb, idempotent, probe-able). Every rule that was a
confession about the forge substrate is gone; the diff is the acceptance test.

---

# PART 3 — ACTION PLAN

Conflict domains: `driver-py` · `campaign-rs` · `registry-rs` · `cli-args-rs` · `taskdb-rs` ·
`executor-rs` · `core-rs` (git_ai/authorship/witness/query) · `contract-rs` · `nix-modules` ·
`flake-checks` · `fixtures` · `tests-py` · `flow-js` · `skills` · `producers-rs`.

## Chapter 0 — Pre-flight (not a campaign)

| id | goal | notes |
|---|---|---|
| P0.1 | Deploy the validated pin | first, alone (D48) |
| P0.2 | One forge-native stormy campaign on the deployed pin | answers #455 machine-steering on the only substrate with campaign-hours; last forge-native run (D48) |
| P0.3 | Rebase local checkout onto origin/main (17 commits) | all paths/lines else phantom (D48) |
| P0.4 | Commit this plan document + worklist files to the repo; merge to main | D47; bootstrap: the worklist must be on the remote base branch to be armable |
| P0.5 | One grep of deployment flakes for `build-effect`/`pool-reachability`; one grep to settle eval_manifest_check_test.py | D57 |
| P0.6 | **Obtain the producers ruling (one word)** | D56 — gates Chapter P |

## Chapter 1 — Squash prerequisites + contract fixes (campaign, forge:"local", maxParallel 2)

| id | goal | targets | deps | domains |
|---|---|---|---|---|
| 1.1 `corpus-divergence-vectors` | Add conflictDomains/forbidPaths rejection vectors; align Python casefold-dedup to Rust exact; merge third forbidPaths copy (`normalize_forbid_paths_gate:6909`) into the canonical one (`:1411`) | driver `:926-946`, `:6909-6935`; `campaign_contract_corpus.rs`; `contract-corpus.json` | — | driver-py, contract-rs, fixtures |
| 1.2 `squash-legacy-checkpoint-tag` | Delete legacy checkpoint tag namespace (0 tags on origin) | driver `:2992,:3561-3574,:9030-9047`; campaign.rs `:3339,:3563-3600`; checkpoint-refs.json legacyTag | — | driver-py, campaign-rs, fixtures |
| 1.3 `worklist-task-revision` | Give file-worklist tasks a `revision` (computed as `task_completion_revision`) | driver `normalize_task:976`, source `:1155` | — | driver-py |
| 1.4 `marker-single-arm` | Collapse `pull_request_marker`/`_revisions`/`campaign_marker_prefixes` to one arm | driver `:2906-2940`; campaign.rs `:3266` | 1.3 | driver-py |
| 1.5 `drop-polluted-v2-migration` | Delete `migrate_polluted_v2` + dispatch; relax registry read to shared lock | campaign_registry.rs `:198,:259,:705-748`, tests `:1446,:1503` | — | registry-rs |

## Chapter 2 — Local canon + git-ai removal (campaign; the shift proper)

Local-canon lane:

| id | goal | targets | deps | domains |
|---|---|---|---|---|
| 2.1 `authority-schema-v3` | v3 registration: drop issue/sub-issue/observation/flag fields; add worklist pattern, code repo, local actor; N/N-1 tests move; refusal names "disarm and re-arm" | campaign_registry.rs `:30-56` | 1.5 | registry-rs, campaign-rs |
| 2.2 `local-steering-source` | Steering JSONL + `tally campaign steer`; repoint `action_steering_recheck:4693`; embargo + high-water cursor; local actor validator replaces `github_login:4554`; off-host read path designed in | driver `:4499-4780`; new CLI verb | 2.1 | driver-py, campaign-rs, cli-args-rs |
| 2.3 `attempt-receipts-jsonl` | Append-only receipts log; port `contiguous_receipts`/pardon arithmetic (`:2496,:2797-2895`) to a fold; **delete ref-blob writers for diagnosis/retry/escalation in the same task** | driver `:2451-2895,:4968,:5399,:5588` | 2.1 | driver-py |
| 2.4 `integration-branch-merge` | Integration branch as merge target; `merge_local:8593` drops temp clone, keeps actual_base/head guard; publish → `local://`; oracle = `merged_local_tasks:3468`; re-key `stable_publish_branch:3460` issue→campaign id | driver `:7746-7838,:8593` | 1.4 | driver-py |
| 2.5 `port-local-semantics` | Re-home worklist validation out of `campaign project`; port escalation/pardon semantics (`active_escalated_tasks`, `amendment_pardon_plan`) to local state | campaign.rs `:2781-3697` (extract), `:844-931` | 2.1 | campaign-rs |
| 2.6 `delete-forge-io-python` | Delete driver [A] block (~1,700–4,600 ln): subissue_walk, issue_graph_worklist, merged_github_tasks, checkboxes+repair, GH comments, steering threads, merge_github, narration posting | driver S3 inventory (twin ledgers) | 2.2, 2.3, 2.4 | driver-py, tests-py |
| 2.7 `delete-forge-io-rust` | Delete arm-time forge read, forge_observation, projection renderer, capability probe, marker/escalation comment code | campaign.rs `:250-1360,:2417-2540,:3093-3963` | 2.1, 2.5 | campaign-rs |
| 2.8 `marker-respell-trailers` | **Atomic:** HTML-comment marker → `Tally-Task:`/`Tally-Revision:` trailers; oracle keys on trailers; last before 2.9 | driver marker fns; campaign.rs counterparts; fixtures | 2.6, 2.7 | driver-py, campaign-rs, fixtures |
| 2.9 `drop-local-forge-escape-hatches` | Delete `--allow-test-local-forge` (`campaign.rs:1932`, args.rs:234) and resume's GitHub-only refusal (`:2541`) | cli | 2.8 | campaign-rs, cli-args-rs |

git-ai lane (one stack; may interleave with local-canon lane on disjoint domains):

| id | goal | targets | deps | domains |
|---|---|---|---|---|
| 2.G1 `relocate-assisted-by` | Move trailer builder out of `producers/gh_intake.rs:101-154` (+engine.rs uses) to a neutral home | producers, core | — | producers-rs, core-rs |
| 2.G2 `remove-gate-a` | Executor/daemon wiring, then `git_ai.rs` (2,255), config surface, 6 nix options + coupling assertion, d12 flake scenario, fixtures | twin ledgers G4-G6, G12-G13/A4-A7, A12 | 2.G1 | executor-rs, core-rs, nix-modules, flake-checks, fixtures |
| 2.G3 `remove-gate-b-and-contract` | Driver gate B (~515 ln `:8146-8592`), contract fields `gitAiBinding`/`gitAiAwaitSec` (+corpus vectors, JS argsSchema) — **contract-field removal lands at the digest-rotation moment (with 2.1/2.8)** | driver, campaign_contract.rs `:172-333,:750`, spec-build.js | 2.G2 | driver-py, contract-rs, flow-js, fixtures |
| 2.G4 `remove-verify-authorship` | `authorship.rs` (855) + `witness verify-authorship` verb + AuthorshipProjection in query | args.rs `:956-1010`, cli/mod.rs, query_v2.rs | 2.G2 | core-rs, cli-args-rs |
| 2.G5 `witness-authorship-emission` | Remove emission + required-validation; **acceptance stays** (D33) — read-side handled last under ledger-compat plan | witness.rs `:119-165` etc. | 2.G4 | core-rs |
| 2.G6 `git-ai-test-doc-sweep` | Delete squash-fidelity .sh/.md, git-ai-authorship.md, final-bar case, test bodies (~1,500–2,000 ln) | twin ledgers G14-G15/A13-A14 | 2.G2–2.G5 | tests-py, fixtures, flake-checks |

Also in Chapter 2: rewrite both skills from scratch (D51; domain `skills`), delete the
CONTRIBUTING paste-transcript clause (superseded in-file).

## Chapter 3 — Release renderer (campaign; self-hosting)

| id | goal | deps | domains |
|---|---|---|---|
| 3.1 `release-issue-create` | One-shot sparse issue create; intent prose = project document verbatim; no manifest/worklist sections | ch2 | campaign-rs |
| 3.2 `release-stack-projection` | Stack renderer over `stable_publish_branch` + dependency levels; **stack-as-projection**: local integration, ff-push, stacked PRs as review surface; squash-merge refused | 3.1 | driver-py, campaign-rs |
| 3.3 `commit-validator-lint-history` | ~150-line Rust validator (8 rules) at the render; `tally lint-history <range>` verb; git-cliff usable as external conformance oracle (not a dependency) | 3.2 | core-rs, cli-args-rs |
| 3.4 `release-receipts-close` | Receipts summary from `campaign_digest`; close; idempotent terminal pass (re-run creates nothing) | 3.2 | driver-py |
| 3.5 `probe-repo-lifecycle` | `tally-probe-*` create/delete verb, TTL sweep, mechanical target refusal, probe assertions | 3.4 | campaign-rs |

## Chapter 4 — Squash the rest + dead cuts (campaign; concurrent with Ch3 except `cli-args-rs`)

| id | goal | deps | domains |
|---|---|---|---|
| 4.1 `squash-gh-origin-schema` | GhOrigin/GhContextSnapshot v0/v1 acceptance arms | — (subsumed by Ch P if producers die) | taskdb-rs, fixtures |
| 4.2 `squash-migration-modules` | capture_migration.rs + unit_exit_migration.rs + `tally migrate` verb + migrate_cli.rs; relocate recovery.rs fixture helpers first; rewrite strict-read error strings naming `tally migrate`; gate: one `--plan` run isClean | gate | taskdb-rs, cli-args-rs, executor-rs |
| 4.3 `squash-err-fallbacks` | pre-split `.err` dual-reads (`captures.rs:140-172,:331-352`) | — | executor-rs |
| 4.4 `squash-rowversion-ladder` | 4 ladder entries → floor refusal; keep frame; delete 7 legacy fixtures + N_MINUS_1 usage fixture | 4.1 | taskdb-rs, fixtures |
| 4.5 `dead-cuts` | `campaign poll --continuation-token` (parsed, discarded); pre-#312 `legacy_state_markers` reclaim; `--mode daemon` spelling; duplicate top-level `enqueue` | — | cli-args-rs, driver-py |

`cli-args-rs` is shared by 2.9/2.G4/3.3/4.2/4.5 — serialize on it; nothing else overlaps.

## Chapter 5 — The Rust port (campaign; right after the deletions, per D34)

| id | goal | deps | domains |
|---|---|---|---|
| 5.1 `argv-seam-reseat` | `run_driver(action, brief)` helper (writes TALLY_BRIEF, execs, parses TALLY_FINAL_MESSAGE=); migrate the 49 action-only tests; cover main/load_brief/emit/DriverError | ch2 complete | tests-py |
| 5.2 `spec-build-driver-crate` | New workspace crate + binary, same argv seam, separately pinned store path; imports `campaign_contract` directly | 5.1 | core-rs (new crate) |
| 5.3 `port-worktrees` | `campaign_worktrees.py` ports with the driver (flock lane locks, identity round-trip, change_set_fingerprint) | 5.2 | core-rs |
| 5.4 `port-actions` | Port surviving ~6,700 lines of actions (pure fold half + git-choreography half); registry asset-manifest keeps two store paths | 5.2, 5.3 | core-rs, driver-py |
| 5.5 `single-source-js-argsschema` | Generate/verify `spec-build.js` argsSchema from Rust (#520 direction) | 5.4 | flow-js, contract-rs |
| 5.6 `delete-python-and-corpus` | Delete drivers/*.py, the corpus generator, byte-compare tests, Rust-source-regex re-parser; flag `agency_nightly_driver.py` (port or explicit grandfather) | 5.4, 5.5 | driver-py, tests-py, contract-rs, fixtures |

## Chapter R — Read model (rides anywhere after P0.1; orthogonal)

| id | goal | domains |
|---|---|---|
| R.1 `declare-canonical-derived` | One tested declaration of canonical stores vs derived views | core-rs |
| R.2 `node-role-typing` | `nodeRole` + `subjectTaskId` in the orchestration capsule, additive; delete `query_v2:1534/1550/1385-1388` string seams + NodeLabelIndex | core-rs, flow-js |
| R.3 `journal-scope` | `journalScope` (flowRunId) on emitted records; JournalFilter gains the field | core-rs |
| R.4 `tally-rebuild-verb` | Promote `durable_run_view` to primary; add unit facts; name the verb | core-rs, cli-args-rs |

## Chapter P — Producers (CONDITIONAL on D56's one word)

If the ruling is "nothing survives": delete `gh_intake.rs`/`gh_decision.rs`/`orphan.rs`, gh
arms of engine/validate, `producer test/orphaned` verbs, `ghProducerType` nix tree +
gh-login.nix + grammar check, `CliSource::Gh`, GhOrigin write paths + projection, campaign
forge nix options (~16–25 leaf options), campaignPoll units, `mkCampaignProducer`,
campaignForge NixOS surface — after 2.G1 relocation and after the outbound thin gh client for
the release act has a home. If "survives": only the campaign doorbell wiring goes. E1/E2
(`build-effect`/`pool-reachability`) cut after P0.5's grep comes back clean.

## Explicitly excluded from the pass

Documentation rewrites of any kind (D53); SQLite/export/query-language surfaces (D54); #523
technotes (D55); backoffice lease + unversioned semantics (D52); `tally ui`; the `query_v2`
rename; any change to the witness float normalization, OldFormat refusals, flow-membership
tolerance, SubmissionOptions legacy arm, migrations frame (D42).

---

# PART 4 — THE WORKLIST FILES

## Schema, derived from code (origin/main `drivers/spec_build_driver.py`)

`action_worklist` (`:1056`): the worklist is **one JSON file in the repository**, located by a
relative glob (no `..`, not absolute) that must match **exactly one** regular blob
(mode 100644/100755) in `git ls-tree -r --full-tree <remote/baseBranch rev>`. Top level is
exactly `{"schemaVersion": 1, "tasks": [...]}` (`object_exact` — unknown keys refused).
`tasks` non-empty, ≤ maxTasks (≤128); task array must be a valid topological order
(dependencies may only name **earlier** tasks); ids unique.

`normalize_task` (`:976`) — two kinds, exact field sets:

- `kind:"implementation"`: `{id, kind, title, goal, deliveredBehaviors, readFirst,
  acceptanceCriteria, dependencies, conflictDomains?}` — `id` matches
  `^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$`, ≤80; `title` ≤300; `goal` ≤12000;
  `deliveredBehaviors` non-empty string list; `readFirst` exactly
  `{specSections: [≥1], styleReferences: []}`; `acceptanceCriteria` non-empty list of
  `{id (safe component ≤80), description ≤4000, argv (non-empty string list)}`;
  `conflictDomains` **required when maxParallel > 1** — normalized relative paths, no
  duplicates under casefold.
- `kind:"checkpoint"`: `{id, kind, title, argv, runtimeMaxSec, dependencies}`.

`maxTasks`/`maxParallel` live in the campaign declaration (nix module), not the file. The
witnessed source is `{path, sha256, revision}` at the pinned remote base revision —
**uncommitted files and local HEAD are not worklist authority** (campaigns.md:1550).

## Impedances found (findings about local mode)

1. **Worklist authority is the remote base branch.** Even a `forge:"local"` campaign runs
   `git fetch <remote>` and reads the worklist at `<remote>/<baseBranch>` — so these files
   must be *merged to main and pushed* before arming (hence P0.4). A fully offline campaign
   requires a path-URL remote; for tally.nix itself the bootstrap commit is one hand-merged
   push, consistent with D8's git-push allowance but worth naming: the "local" mode still has
   a remote-read in its arm path.
2. **No cross-file dependency edges.** Dependencies are intra-worklist only; chapter ordering
   (Ch1 → Ch2 → …) is operator sequencing of separate campaigns, not schema-expressible. The
   forge-native manifest had the same property (one issue = one campaign), so this is parity,
   not regression — but the plan's cross-chapter gates (e.g. "2.G3 lands at the digest-rotation
   moment with 2.1") must be enforced by conflict domains + within-chapter ordering instead.
3. **The v1-marker live-fire trap applies to these very campaigns.** File-worklist tasks carry
   no `revision` today, so Chapter 1 itself runs in the mode that writes v1 markers — task 1.3
   fixes the trap the campaign is standing in. Ordering 1.3 → 1.4 inside one campaign is safe
   because markers are per-task; the respell (2.8) is a different, later campaign.
4. **conflictDomains are path-shaped strings** (relative-path normalization + casefold dedup)
   though semantically arbitrary labels; single-segment names like `driver-py` pass. A
   forge-native manifest would have carried issue numbers, sub-issue URLs and labels — the
   file worklist has **no identity fields at all** beyond task id; identity is task id +
   campaign name, and the arm record still demands `issue_url`/`issue_number` until authority
   v3, which is why every campaign before 2.1 needs `--allow-test-local-forge`.
5. **acceptanceCriteria demand executable argvs** — every criterion is a witnessed command, so
   plan prose must compress to ≤4000-char descriptions plus mechanical checks; several
   deletion tasks below use `bash -lc` negative greps as their mechanical form, which is the
   honest local equivalent of "reviewer confirms the code is gone."

## Chapter 1 worklist (full, valid against the schema)

```json
{
  "schemaVersion": 1,
  "tasks": [
    {
      "id": "corpus-divergence-vectors",
      "kind": "implementation",
      "title": "Close the two live Rust/Python contract divergences under the corpus",
      "goal": "Align drivers/spec_build_driver.py normalize_conflict_domains (lines 926-946) with Rust campaign_contract.rs dedup semantics (exact BTreeSet, not casefold), merge the third forbidPaths copy normalize_forbid_paths_gate (lines 6909-6935) into the canonical normalize path (line 1411) so the trailing-slash check is enforced everywhere, and add rejection vectors for conflictDomains and forbidPaths to crates/tally-core/tests/campaign_contract_corpus.rs so test/fixtures/spec-build/contract-corpus.json covers both fields. The corpus currently has zero conflictDomains rejection vectors; both divergences ship today.",
      "deliveredBehaviors": [
        "Python and Rust reject and accept byte-identical conflictDomains inputs",
        "A single forbidPaths normalizer serves both the contract decode and the constraint gate",
        "contract-corpus.json carries rejection vectors for conflictDomains and forbidPaths"
      ],
      "readFirst": {
        "specSections": ["final-plan-A.md Part 1 D38", "final-plan-A.md Part 3 Chapter 1"],
        "styleReferences": ["crates/tally-core/tests/campaign_contract_corpus.rs"]
      },
      "acceptanceCriteria": [
        {
          "id": "corpus-regenerated",
          "description": "The corpus regenerates from Rust and the Python decoder test passes against it, including the new conflictDomains and forbidPaths rejection vectors.",
          "argv": ["python3", "test/spec_build_contract_corpus_test.py"]
        },
        {
          "id": "single-forbidpaths-copy",
          "description": "normalize_forbid_paths_gate no longer carries an independent grammar; the constraint-gate path calls the canonical normalizer.",
          "argv": ["bash", "-lc", "test \"$(grep -c 'def normalize_forbid_paths_gate' drivers/spec_build_driver.py)\" -eq 0 || ! grep -q 'endswith' <(sed -n '/def normalize_forbid_paths_gate/,/^def /p' drivers/spec_build_driver.py)"]
        }
      ],
      "dependencies": [],
      "conflictDomains": ["drivers", "crates/tally-core", "test/fixtures/spec-build"]
    },
    {
      "id": "squash-legacy-checkpoint-tag",
      "kind": "implementation",
      "title": "Delete the legacy checkpoint tag namespace",
      "goal": "Remove the pre-#307 refs/tags/tally checkpoint fallback: drivers/spec_build_driver.py legacy_checkpoint_tag (line 2992) and its dual-read call sites (3561-3574, 9030-9047), crates/tally/src/cli/campaign.rs legacy_checkpoint_tag_prefix (3339, 3563-3600), the legacyTag/legacyTagPrefix vectors in test/fixtures/spec-build/checkpoint-refs.json and test/spec_build_checkpoint_receipts_test.py (678, 758). Precondition already verified: git ls-remote origin 'refs/tags/tally/*' returns zero entries.",
      "deliveredBehaviors": [
        "Checkpoint receipts read from exactly one namespace",
        "The operator rule 'checkpoint receipts may live in two namespaces' is deleted"
      ],
      "readFirst": {
        "specSections": ["final-plan-A.md Part 1 D39-D40", "final-plan-A.md Part 3 Chapter 1"],
        "styleReferences": ["doc/audit/campaigns-worklist-checkpoints.md"]
      },
      "acceptanceCriteria": [
        {
          "id": "no-legacy-tag-symbol",
          "description": "No source file references the legacy checkpoint tag helpers.",
          "argv": ["bash", "-lc", "! grep -rn 'legacy_checkpoint_tag' drivers crates test"]
        },
        {
          "id": "checkpoint-suite-green",
          "description": "The checkpoint receipts suite passes with the single namespace.",
          "argv": ["python3", "test/spec_build_checkpoint_receipts_test.py"]
        }
      ],
      "dependencies": [],
      "conflictDomains": ["drivers", "crates/tally", "test/fixtures/spec-build"]
    },
    {
      "id": "worklist-task-revision",
      "kind": "implementation",
      "title": "Give file-worklist tasks a completion revision",
      "goal": "normalize_task (drivers/spec_build_driver.py line 976) admits file-worklist tasks with no revision, so pull_request_marker (line 2911) emits the tally:spec-build:v1 spelling for exactly the mode the silent factory promotes to default. Compute a revision for file-worklist tasks the way task_completion_revision does for issue tasks, thread it through source construction (line 1155), and cover it with a driver test asserting a file-worklist task now yields a v2-revision marker.",
      "deliveredBehaviors": [
        "File-worklist tasks carry a completion revision",
        "pull_request_marker never emits the v1 spelling for file-worklist tasks"
      ],
      "readFirst": {
        "specSections": ["final-plan-A.md Part 1 D40", "final-plan-A.md Part 4 impedance 3"],
        "styleReferences": ["test/spec_build_driver_test.py"]
      },
      "acceptanceCriteria": [
        {
          "id": "driver-suite-green",
          "description": "Driver suite passes including the new file-worklist revision test.",
          "argv": ["python3", "test/spec_build_driver_test.py"]
        }
      ],
      "dependencies": [],
      "conflictDomains": ["drivers"]
    },
    {
      "id": "marker-single-arm",
      "kind": "implementation",
      "title": "Collapse the PR marker readers and writers to one arm",
      "goal": "With every task now carrying a revision, delete the revision-is-None v1 builder branch in pull_request_marker (drivers/spec_build_driver.py 2906-2921), the legacy probe in pull_request_marker_revisions (2922-2940), and the second prefix in campaign_marker_prefixes plus the crates/tally/src/cli/campaign.rs acceptance counterpart (around line 3266). Estate fact: zero v1-marker PRs exist; 25 v2 PRs are merged history no future campaign re-parses.",
      "deliveredBehaviors": [
        "One marker grammar with one reader and one writer",
        "The completion oracle keys on a single marker spelling"
      ],
      "readFirst": {
        "specSections": ["final-plan-A.md Part 1 D40", "final-plan-A.md Part 3 Chapter 1"],
        "styleReferences": ["test/spec_build_driver_test.py"]
      },
      "acceptanceCriteria": [
        {
          "id": "no-v1-spelling",
          "description": "The v1 marker spelling appears nowhere in source (fixtures for historical parsing excepted only if a test names why).",
          "argv": ["bash", "-lc", "! grep -rn 'tally:spec-build:v1' drivers crates"]
        },
        {
          "id": "driver-suite-green",
          "description": "Driver suite passes with the single-arm marker.",
          "argv": ["python3", "test/spec_build_driver_test.py"]
        }
      ],
      "dependencies": ["worklist-task-revision"],
      "conflictDomains": ["drivers", "crates/tally"]
    },
    {
      "id": "drop-polluted-v2-migration",
      "kind": "implementation",
      "title": "Delete migrate_polluted_v2 and relax registry read locking",
      "goal": "Delete the v1-polluted-to-v2 repair arm in crates/tally-core/src/campaign_registry.rs (dispatch at line 259, body at 705-748) and its pin tests (1446, 1503), then relax the exclusive read lock to shared (line 198). Estate fact: all local stores are at current versions; the repair guards data that does not exist. This must land before the authority v3 bump (Chapter 2) so the bump never enters a ladder carrying a repair arm.",
      "deliveredBehaviors": [
        "Registry reads take shared locks",
        "The armed-record repair arm and the operator rule 'an armed record may be polluted' are deleted"
      ],
      "readFirst": {
        "specSections": ["final-plan-A.md Part 1 D39-D41", "final-plan-A.md Part 3 Chapter 1"],
        "styleReferences": ["crates/tally-core/src/campaign_registry.rs"]
      },
      "acceptanceCriteria": [
        {
          "id": "no-polluted-symbol",
          "description": "migrate_polluted_v2 is gone from the crate.",
          "argv": ["bash", "-lc", "! grep -rn 'migrate_polluted_v2' crates"]
        },
        {
          "id": "core-tests-green",
          "description": "tally-core test suite passes.",
          "argv": ["cargo", "test", "-p", "tally-core"]
        }
      ],
      "dependencies": [],
      "conflictDomains": ["crates/tally-core"]
    },
    {
      "id": "chapter-gate",
      "kind": "checkpoint",
      "title": "Chapter 1 gate: full workspace check",
      "argv": ["bash", "test/fleet-gate.sh"],
      "runtimeMaxSec": 7200,
      "dependencies": [
        "corpus-divergence-vectors",
        "squash-legacy-checkpoint-tag",
        "marker-single-arm",
        "drop-polluted-v2-migration"
      ]
    }
  ]
}
```

All chapters are emitted as standalone files beside this document —
`final-worklist-A-ch1.json` … `final-worklist-A-ch5.json` and `final-worklist-A-chR.json` —
one campaign per chapter, same schema. Chapter 2 encodes the two lanes (local-canon, git-ai)
as one topological order with conflict domains keeping them parallel-safe;
`marker-respell-trailers` and `drop-local-forge-escape-hatches` are the last implementation
nodes, per D21/D49. Chapter P (producers) is not drafted — it gates on the PENDING one-word
ruling D56.

**Validation:** all 46 tasks in the six files were validated by importing
`origin/main:drivers/spec_build_driver.py` and calling its `normalize_task` on every node with
`require_conflict_domains=True` (the maxParallel>1 posture); zero rejections. The generator +
validator lives at `wlval/gen_worklists.py` in the session scratchpad.


---

# PART 5 — MERGE ADDENDUM (three-way compilation, adjudicated)

This plan was compiled three times independently (compilers A, B, C) from the full session transcript and artifacts, then merged by the session lead holding the complete conversation in context. **Verdict: zero contradictions between the three registers and zero contradictions with the session record.** Compiler A's compilation (this document) is the base; the following items are the union patch — rulings a sibling stated more explicitly, adopted into the register by this addendum.

## 5.1 Register additions (from compilers B and C)

- **D58 (C's R10) — The backlog.md placement law.** Every roll-in lives mechanism-side (daemon, CLI, registry) and must delete operator rules; anything that asks the operator to tend a new artifact is forbidden. This is the acceptance filter the whole pass was designed under; it binds future additions too.
- **D59 (C's R16) — Local canonical state must survive the coordinator.** Before the first real dark mission: prove attempt-counter recovery across a daemon/coordinator restart, and exempt campaign-terminal evidence from the 30-day capture horizon (or bind diagnosis prose into the unpruned ledger). The forge's comments outlived its captures; the local factory must not retain less than the surface it replaces.
- **D60 (C's R43) — The CONTRIBUTING paste-transcript clause deletes now.** It is already superseded in-file by the merge-control section; `test/fleet-gate.sh` itself stays (it is the gate ladder's runner).
- **D61 (doctrine, from the OSS-notes synthesis, endorsed in session) — The reconciling sentence:** *ceremony aimed at an imagined public audience is out; machinery that makes agent output mergeable is in.* Resolves every apparent contradiction between "no ceremony ever" and tally's instrumentation; belongs wherever doctrine lands in the docs round.

## 5.2 Local-mode impedance findings (the machinery test this compilation doubled as)

All three compilers emitted worklists against origin/main's real `normalize_task(require_conflict_domains=True)` — 46 tasks (A), 14 (B), 31 (C), all decoding clean. The union of impedances, each a finding about `forge:"local"` mode, several worth issues of their own:

1. **Cross-chapter dependencies are inexpressible.** `dependencies` may only name earlier tasks in the same worklist file; CH1→CH2→CH3 ordering is operator arming discipline, where a forge manifest carried cross-issue blocked-by. (Candidate mechanism: a `requiresWorklist` pin or multi-file campaign container — file as an issue, do not improvise.)
2. **Conflict domains are enforced as case-folded path prefixes against actual changed paths** — symbolic labels (`driver-py`) decode fine and then fail every lane at ownership. All emitted worklists use real path prefixes. Granularity adjudication: **directory-level prefixes (A/C style) are the default**; exact-file domains (B style) only where two tasks genuinely partition one file's neighborhood — a worker touching an unlisted sibling test file fails ownership, and that failure mode is priced in attempts.
3. **Worklist authority is a committed blob on the *remote* base branch** — even a fully local campaign runs `git fetch <remote>` and reads at `<remote>/<baseBranch>`; uncommitted bytes and local HEAD are explicitly not authority. Consequence: this plan document and its worklists must be committed and pushed before anything arms. One typed hand commit per chapter for plan+worklist is the sanctioned exception under the merge-control ruling, or the plan lands via its own first release act.
4. **The v1-marker trap fires on the realizing campaigns themselves** — file-worklist mode writes the `tally:spec-build:v1` marker today, so Chapter 1 emits the very spelling this pass deletes until its own `worklist-task-revision` task lands. Harmless (local, private), but expected — do not diagnose it as a defect mid-campaign.
5. **Every acceptance criterion must be an executable argv.** Deletion tasks carry `bash -lc` negative-grep gates; plan-shaped work (rulings, sequencing) cannot be tasks and lives in this document instead.
6. **Identity residue until authority v3:** `repository` must be `owner/name`-shaped even locally; the arm record demands `issue_url`/`issue_number` until task `authority-schema-v3`; every pre-v3 campaign runs under `--allow-test-local-forge`; `stable_publish_branch` still expects an `issue` key (re-keyed to campaign id in CH2).
7. **The plan document is machine-consumed spec, not documentation.** With no issue bodies, each task's `goal` plus `readFirst` pointers into this committed document are the worker's entire context — which is why this document is in scope while the docs round is not.

## 5.3 Worklist selection

The canonical worklists for arming are **compiler A's six files** (`ch1`–`ch5`, `chR`), adopted with this addendum's granularity rule. B's and C's sets are retained in the session record as cross-checks; where B/C split or merged tasks differently, A's decomposition was verified against the shed ledgers' file:line evidence and stands.

## 5.4 The producers ruling — RESOLVED 2026-08-11 evening

**D56/R45 is ruled: the GitHub-inbound trigger surface is DEAD.** Operator, verbatim: *"github-inbound trigger is DEAD. i no longer want this. 6k lines of code less."* The producers GH stack goes in full: `producers/gh_intake.rs` (2,145), `gh_decision.rs` (739), `orphan.rs` (339), the gh arms of `engine.rs`/`validate.rs` (~600), ~1,500 test LOC, the `ghProducerType` nix tree + projection options, `nix/lib/gh-login.nix` + fixtures + flake check, `CliSource::Gh`, the GhOrigin/GhContextSnapshot write paths, `producer orphaned` and the gh arms of `producer test`. The contingent worklist `chR.json` is hereby **activated as Chapter P**, ordered after Chapter 2, with two standing preconditions: (1) the `Assisted-by:` builder relocation out of `gh_intake.rs:101-154`/`engine.rs:986,1049` (already a CH2 dependency); (2) the one-time deployment-flake grep for `build-effect`/`pool-reachability` producer kinds (D57), whose result folds `~1,100` further LOC into Chapter P if clean.

---

# PART 6 — AUG-12 AMENDMENT (field findings F1–F11 rolled in as sound fixes)

The first two nights of execution (campaign #527/#467, the ch1 arming as #529, and
sodimo/os running as the estate's first second tenant) produced eleven recorded
findings plus one morning discovery. This amendment rules each into the pass with
its **sound** fix — never a workaround — adds **Chapter 0**, amends **Chapter 2**,
and re-sequences the ladder. Authored 2026-08-12; supersedes Part 3's ordering.

## 6.1 New rulings

- **D62 — Repo-scoped campaign mutexes, minted on demand.** The host-wide
  `campaign` pool was a relic of the single-tenant era; the capacity-1 invariant
  protects a *repository's* integration branch, and two campaigns on different
  repositories share nothing but compute — which `campaign-agent` (slots) and the
  gpu pools already govern host-wide. The doctrine line: **mutexes scope to the
  thing they serialize; resource pools scope to the host.** Mechanism: the
  `campaign/` pool namespace is reserved; a lease request naming
  `campaign/<owner>/<repo>` mints a capacity-1 co-residency mutex deterministically
  from the name alone, in live admission and in durable rebuild — no config entry,
  no daemon restart, no new persistence (the pool *is* its name). Arm defaults
  `manifest.pool` to the repo's namespace pool. Cross-repo campaigns run
  concurrently; same-repo campaigns serialize exactly as before.
- **D63 — Substrate freeze is mechanism.** `tally campaign quiescent` (exit 0 iff
  no registration is armed) becomes the `ExecCondition` of every deploy unit. The
  hand-authored epoch drop-in from the Aug-12 night (F5) is retired; per D58 the
  roll-in deletes an operator rule.
- **D64 — `project` synthesizes checkpoint briefs.** The committed worklist file is
  `project`'s input **verbatim**; for `kind=checkpoint` tasks the brief is rendered
  from the task's own argv/runtime/dependencies (F8/F11). Side copies of worklists
  with hand-added bodies are abolished.
- **D65 — The arm-time gate-argv hazard lint is tier-aware** (F9). Silent on a
  host with no hardening preset; verbatim warnings under a real tier. A warning
  that fires on every input trains operators to ignore warnings.
- **D66 — The gitAi config surface deletes with the gates** (F6). With the key
  gone from the schema, a stale host config fails loud at boot — residue cannot be
  carried silently. Host-side key removal is a documented step of the deploy that
  ships Chapter 2.
- **D67 — The module renders `forge` as a declared option** (F10).
  `renderCampaignRepositories` stops hardcoding `github`; a module-declared
  campaign can state `forge = "local"` once authority v3 lands.
- **D69 — One live view per campaign** (sodimo F-01/F-08/F-04/F-07). `tally
  campaign status <master-url>` resolves registration → latest observation →
  per-task state → live flow run, with a campaign-level usage rollup; a finished
  arm-time run prints a superseded pointer; poll events carry per-registration
  attribution; completion short-circuits clean (no schema-mismatch noise, no
  rearm flap from tally's own forge writes). The arm-time run is not the
  campaign — the machinery must say so itself.
- **D70 — The preflight contract** (sodimo F-02). Preflights assert environment
  only, never gate-produced state; a failing preflight's error carries its argv
  verbatim (empty-stderr deaths are abolished); the pre-arm freeze rehearsal runs
  preflight argvs in a pristine worktree. Code half in ch0
  (`preflight-error-argv`); doctrine half in the ch2 `skills-rewrite` (which also
  picks up F-06: all supervision `gh` calls pass `-R <owner>/<repo>` — bare `gh`
  resolves to `upstream` in fork checkouts).
- **D71 — The adversarial lane is a standing campaign pattern** (sodimo positive
  finding 4). Two dedicated adversarial-suite lanes found and fixed five real
  security holes that implementation lanes' own tests missed, inside the same
  witnessed-gate discipline. Every future campaign of consequence ships one
  end-of-chapter adversarial task with a fix-in-place mandate and structural
  (not hand-listed) surface enumeration. For this pass: candidate for ch3 (the
  release surface is the outward-facing one); not retrofitted into ch0-ch2's
  deletion work.
- **D68 — The worker-context law.** Every `readFirst` pointer must name a file
  that exists on the authority revision. (This amendment repaired 48 phantom
  `final-plan-A.md` pointers across all six worklists to `SILENT-FACTORY-PLAN.md`
  — every worker's primary spec pointer named a file absent from the repo.)
  Corollary, per the operator's ruling on ceremony friction: the arming ceremony
  is fixed by **provisioning context** — the skills carry the exact projection
  contract and this document carries the worker context — not by relaxing
  mechanism. And F4's fix is never "fake a URL": arm's authority input becomes
  the campaign identity (repository + committed worklist), per the sharpened
  `authority-schema-v3` task.

## 6.2 Findings ledger — disposition of all eleven plus the morning discovery

| Finding | Disposition |
|---|---|
| F1 stale untracked file blocked ff | Resolved in the night; no mechanism. |
| F2 dotfiles lock carried 5-input update | **Operator's**: land the pending nixpkgs+friends update deliberately. |
| F3 docs-only commits never move the pin (src filter) | Knowledge; recorded here. Worklist changes need no redeploy. |
| F4 arm demands GitHub URL even local | Sound fix in **ch2 `authority-schema-v3`** (sharpened) + `port-local-semantics` + `delete-forge-io-rust`. |
| F5 fleet-deploy vs live campaign | **ch0 `campaign-quiescent-verb`** (D63). |
| F6 gitAi config residue | **ch2 `gitai-config-purge`** (D66). |
| F7 readFirst resolves from checkout | Knowledge; superseded in practice by D68 (pointers verified against authority). |
| F8 project rejects reference manifest | **ch0 `checkpoint-brief-render`** (D64) + skills carry the projection contract. |
| F9 static arm-lint noise | **ch0 `tier-aware-arm-lint`** (D65). |
| F10 module hardcodes forge github | **ch2 `module-forge-option`** (D67). |
| F11 checkpoint tasks need bodies | Same fix as F8 (D64) — render-side synthesis, not schema relaxation. |
| Morning: host-wide campaign mutex | **ch0 `campaign-pool-namespace` + `mutex-restart-recovery`** (D62, discharging D59). |

Sodimo program findings (`sodimo-aug11-learnings.md`, 33/33 tasks, zero
interventions), folded 2026-08-12 afternoon:

| Sodimo finding | Disposition |
|---|---|
| F-01 stale arm-uuid query, no live status verb | **ch0 `campaign-status-verb`** (D69). |
| F-02 preflight fails with empty stderr; pristine-sweep landmine | **ch0 `preflight-error-argv`** (D70); doctrine in ch2 `skills-rewrite`. |
| F-03 #484 linter false-positives (self-created /tmp, non-evaluating nix) | Folded into **ch0 `tier-aware-arm-lint`** (D65). |
| F-04 poll counters fleet-wide, not per-campaign | **ch0 `poll-event-quality`** (D69) — urgent once D62 makes concurrency the norm. |
| F-05 fleet-deploy exec-condition guard exists | Confirms D63's direction; `campaign-quiescent-verb` supersedes both drop-ins. |
| F-06 bare `gh` resolves to upstream in forks | One line in ch2 `skills-rewrite` (D70). |
| F-07 post-completion reconcile noise, rearm flap | **ch0 `poll-event-quality`** (D69). |
| F-08 usage fragmented across poller flow runs | Rollup rides **ch0 `campaign-status-verb`** (D69). |
| Positive 4: adversarial lanes found 5 real holes | **D71** — standing pattern; ch3 candidate task. |

## 6.3 Revised ladder

**ch0 → pin deploy → ch1 → ch2 (18 tasks, amended) → chP → ch3 → ch4 → ch5**, with
chR/read-model riding anywhere post-deploy. Rationale:

1. **Chapter 0 first** (9 tasks: D62–D65 and D69–D70, plus the D59 recovery
   proof). It is the bleeding wound (scheduling), every ceremony fix the later
   chapters themselves will trip over, and the supervision surface (status
   verb, truthful poll events, argv-bearing preflight errors) that makes the
   rest of the ladder cheap to watch. It runs forge-native on the current pin,
   exactly like #527 did.
2. **One pin deploy after ch0 merges**, at campaign quiescence, before anything
   else arms — the only substrate move in the ladder until the operator chooses
   another. From that pin onward: repo-scoped mutexes are live (**sodimo may
   resume alongside tally with zero contention**), deploys self-defer under armed
   campaigns, `project` takes committed worklists verbatim, and arming is silent
   on this host.
3. Chapters 1–5 as previously specified. The #529–#535 projection is superseded
   (worklist bytes changed → new authority hash): disarm #529's registration,
   close the sub-issues as superseded, and re-project ch1 fresh from the amended
   file when its turn comes.

57 tasks total across ch0–ch5 + chR, all validated on the working tree against
`normalize_task(require_conflict_domains=True)` at the time of this amendment
(re-run after the sodimo fold).

# PART 7 — CHAPTER 3-EPSILON (2026-08-13 amendment)

## 7.0 Standing

This part supersedes Part 3 Chapters 3, 4, 5 and P, and item 3 of §6.3, at the
moment chapter 2 closed (receipt 2026-08-13T12:08:20Z, worklist
sha256:fde8ad81…, base `52eff4db`). The committed `ch3.json`, `ch4.json`,
`ch5.json` and `chR.json` files remain in the tree as historical inputs and are
never armed; their `readFirst` pointers into Part 3 are void. The remaining
work is realized as **three campaign stages sharing one identity** (§7.2),
designed from the chapter 0–2 findings F13–F26 and a three-agent design panel
adjudicated on 2026-08-13.

Bookkeeping corrections carried here rather than by rewriting old parts (D53):

- **S17**: Part 5's identification of `chR.json` as "the Chapter P worklist" is
  false. `chR.json` is the read-model chapter (its substance rides ε2's
  R-lane). Chapter P never had a worklist file; its enumeration was done fresh
  against the tree for ε1.
- **D68 resolve check**: every `readFirst.specSections` entry in an armed
  epsilon worklist points into this Part 7. Anything still pointing at Part 3
  chapter prose is a defect in the worklist, not in the plan.

## 7.1 What chapter 2 falsified

Part 3's chapter 3–5 prose was written against a tree that chapter 2 rewrote:

- `campaign project`, the projection renderer and the master-issue lifecycle
  are deleted. Campaign authority is v3: `arm` takes a repository plus a
  committed worklist path and synthesizes a `local://` identity. There is no
  master issue, no sub-issue projection, and the driver performs zero gh I/O.
- The release-renderer lineage ch3.json told workers to extend
  (`render_project_task_body`, campaign.rs digest functions) no longer exists;
  the surviving digest/summary/branch-naming folds live in the Python driver
  that ε2's C-lane deletes. Building the renderer in Python first would be
  writing code that is about to die (D34's own logic) — so the release surface
  is Rust-native from the start (§7.5).
- The module-declared campaign was discovered by
  `local_campaign_declaration_from_document` scanning the rendered config for
  an **enabled `kind:"gh"` producer named `campaign-<name>`** whose
  `enqueue.brief` carries the worklist — the single most load-bearing fact in
  the tree, found independently by both design proposers. The operator
  rejected per-campaign host configuration outright, so **D77** (landed
  out-of-band before ε0, see §7.7) deleted that mechanism: arm is
  self-contained, campaign policy lives in the worklist's `campaign` section,
  and ε1's `P1` shrinks to deleting the now-vestigial nix rendering.
- The `campaignPoll` units (nix/modules/nixos.nix:665–719,
  home-manager.nix:591–647) are the local heartbeat and **survive**; Part 3's
  "delete campaignPoll" prose is wrong.
- The final conformance bar (`test/final-bar/`) is broken on main: chapter 2's
  `drop-local-forge-escape-hatches` deleted `--allow-test-local-forge` from
  `crates/` while four call sites still pass it (cases/manifest.py:286,
  cases/pipeline.py:149 and :253, cases/registry.py:59) — and nothing noticed,
  because the bar's only flake attribute (`final-conformance-bar-harness`) runs
  `--list` without executing a case and fleet-gate has no final-bar step. The
  bar had **no gate coverage at all** (recorded as fact; repaired in ε0).

## 7.2 The epsilon structure

Three stages, two deploys. One campaign identity: repository
`mecattaf/tally.nix`, worklist `silent-factory-worklists/epsilon.json`. No
host configuration names the campaign (D77): the worklist's own `campaign`
section carries the policy, and arm is
`tally campaign arm mecattaf/tally.nix silent-factory-worklists/epsilon.json`
run from the checkout. The committed content of `epsilon.json` is replaced
between stages, only at registry quiescence; each stage's exact bytes are
pinned by its completion receipt hash and recoverable from git history (D73).

1. **deploy-1** (done 2026-08-13): pin `52eff4db`, gitAi host key removed,
   quiescence guards on deploy admission and activation. A follow-up pin
   bump carrying D77 precedes ε0 arming.
2. **ε0 — shakedown** (3 tasks + gate, §7.3): the first-ever local-mode
   campaign. Three real, small, disjoint repairs that double as the checklist
   for local arm/steer/correction/completion semantics.
3. **ε1 — deletion wave** (~13 tasks + gate, §7.4): the producers stack, gh
   origin, gh nix surface, squashes and dead cuts, plus the three hardening
   tasks (brief-carries-conflictDomains, ownership preflight warn, poll
   liveness arm).
4. **deploy-2** (operator act, at ε1 quiescence): hardening and deletions go
   live for the build wave. The sanctioned moment to adjust the module gate
   set if needed (D74).
5. **ε2 — build wave** (~17 tasks + gate, §7.5): release surface, read model,
   Rust driver port — **authored only after ε1 has merged**, against the
   observed tree, converting predicted consumer sets into observed ones (the
   decisive lesson: nine of fourteen chapter 0–2 authority corrections were
   mis-predicted consumer sets).
6. **Probe run + self-release**: a real `tally-probe-*` run as a named
   pre-release operator step with a recorded receipt, then ε2 releases itself
   with the `tally campaign release` verb it built (D49 self-hosting).

## 7.3 Stage ε0 — shakedown

Three implementation tasks, mutually disjoint, `maxParallel 3` — deliberately
parallel so the first local campaign exercises multi-lane conflict-domain
scheduling on day one.

- **`final-bar-local-forge-repair`** — remove the deleted
  `--allow-test-local-forge` flag from the four final-bar call sites named in
  §7.1 and bring the whole bar back to exit 0 against the post-chapter-2
  local-only CLI (`test/final-bar/run <absolute tree path>`; tri-state exit,
  see test/final-bar/README.md). The bar has not executed since before the
  chapter 2 deletions, so further staleness found by running it is in scope.
  Owns `test/final-bar` only.
- **`gate-keep-going`** — `test/fleet-gate.sh` runs `nix flake check -L`,
  which stops at the first failing attribute; chapter 2's gate repair found
  two more stale suites hiding behind the first failure (F14/F21, three
  campaigns for three). Change the step to `nix flake check -L --keep-going`
  so one gate failure enumerates every failing attribute. The invocation
  contract (`usage: … <full-commit-sha>`) is pinned as a regression criterion
  (F13b). Owns `test/fleet-gate.sh` only.
- **`steering-grammar-negation`** — F15, the sharpest chapter 2 finding: the
  managed-agents content contract (`drivers/spec_build_driver.py:176-186`)
  rejects any text containing `!`, and the rejection redaction (`:3779`)
  replaces `!` with `.` — so a machine diagnosis cannot state a shell or Nix
  negation (`! grep …`) without being gagged or mangled. It was silenced twice
  in chapter 2 while diagnosing correctly. Permit `!` inside inline code
  spans/argv reproductions while keeping the bang-free rule for ordinary
  prose, as one named predicate the ε2 Rust port can carry verbatim. The
  `evidence.rs:1451-1478` / `journal.rs:1190` sites are test assertions on
  tally's own narration, not this validator — out of scope. Owns `drivers` and
  `test/spec_build_driver_test.py`.

The chapter checkpoint runs fleet-gate **and** the full final bar — the bar's
first-ever mechanical gate coverage (D74).

**Shakedown checklist** (operator, during ε0): observe local arm mechanics;
exercise one deliberate `steer`; run one deliberate worklist-correction cycle
(edit → commit → push → `resume --reason`) to settle correction semantics
before ε1 needs them in anger; pin a `>2^31` steering/id value (F18 regression
watch); observe completion/disarm semantics and the terminal operator signal.

## 7.4 Stage ε1 — deletion wave

Near-serial and honestly so — `conflictDomains` overlap, not `maxParallel`,
is the binding constraint. Tasks (anchors verified at `bea5c47d`–`52eff4db`):

- **P1 `campaign-nix-surface-retire`** — delete the now-vestigial module
  campaign rendering that D77 obsoleted: `mkCampaignProducer`,
  `mkCampaignFlow`, `mkCampaignArgs`, the `services.tally.campaigns` option
  set with its forge-native-only fields, and the module-layer contract
  assertions over them. Nothing reads these post-D77; the seam criterion is
  that arm/resume/poll behavior is byte-identical before and after (the D77
  test suite is the oracle). Everything gh-ward still runs after P1.
- **P2 `delete-gh-inbound-core`** — gh_intake.rs (2,105 LOC), gh_decision.rs
  (739), orphan.rs (339), the gh arms of engine/validate/config, build-effect
  and pool-reachability kinds (D57 grep executed: dotfiles AND sodimo-os
  clean — fold is unconditional), **and the daemon's outbound gh subsystem**
  (~250 LOC: completion.rs:75–110 and :186–240, startup.rs:535, daemon
  re-exports) — the goal states the behavior change: the daemon stops writing
  to GitHub for producer completions at all. Calendar and events-dir kinds
  survive and keep passing their suites.
- **P3 `delete-gh-origin-durable`** — GhOrigin/GhContextSnapshot, wire/taskdb
  legacy arms, projections. **Keep the `EnqueueSource::Gh` string-decode arm**
  (D33): measured census 0 gh-sourced events of 3,859, 0 of 1,775 rows carry
  ghOrigin — the decode arm is cheap insurance, the payload-acceptance path
  dies. Grants include `crates/tally-flow` (model.rs:622/628/634).
- **P4 `delete-gh-nix-tree`** — the nix half. `campaignPoll` units survive
  (§7.1); `pkgs.gh` leaves poll runtimeInputs; the forge option collapses to
  local; the reviewers validation surface is deleted (D75).
- **A1 `squash-migration-modules`** — capture_migration.rs and
  unit_exit_migration.rs sit at tally-core src top level (the committed
  worklist's "executor domain" is wrong); owns migrate_cli.rs (the known F25
  config leaker) and cli/mod.rs. `migrate --plan` isClean is a pre-arm
  operator step, not a lane criterion.
- **A2 `err-fallbacks`**, **A3 `rowversion-ladder`** (+ taskdb/ subdir +
  usage.rs domains; the criterion counts `ROW_MIGRATIONS` entries, not
  `RowMigration {` occurrences), **A4 `dead-cuts`** (+ CONTRIBUTING.md +
  cli/mod + driver-suite ownership; folds the orphaned
  `eval_manifest_check_test.py` deletion and the driver `forge:"local"`
  tightening).
- **H1 `brief-carries-conflict-domains`** (F22): render `conflictDomains`
  into the projected task brief at prep; owns drivers, driver suite, the flow
  and `crates/tally-flow` (prep results pass closed schemas, F24).
- **H2 `ownership-preflight-warn`**: arm-time textual lint of goal/criteria
  path tokens against declared domains — warn, never gate. Owns
  `crates/tally`.
- **H3 `poll-liveness-arm`** (F23): dispatchable work + zero live nodes ⇒
  dispatch, regardless of observation digest. Owns `crates/tally` and
  `flake.nix` (the VM poll assertions are exactly where F21 bit).

## 7.5 Stage ε2 — build wave (authored against the observed post-ε1 tree)

Substance fixed now; line anchors and consumer enumerations are written only
after ε1 merges. Three genuinely disjoint lanes (`maxParallel 3`):

- **B-lane** (domains: `crates/tally` + `test/release-probe.sh`): B1
  Rust-native `tally campaign release` reading durable state directly
  (registry, attempt-receipts JSONL, integration branch, trailer oracle); B2
  commit validator + `lint-history` in `crates/tally` (D26 "in the
  renderer"); B3 release-execute with an injectable gh program (fresh seam —
  the TALLY_GH_PROGRAM precedent is deleted) and idempotency from the local
  release record, never public-body markers (D4); B4 probe lifecycle with a
  shim-forge lane criterion — the real `tally-probe-*` run is a named
  pre-release operator step with receipt; B5 adversarial release lane (D71)
  plus the F25 sweep for `--config`-less spawns.
- **R-lane** (domains: `crates/tally-core` + `examples/flows` +
  `crates/tally-flow`): R1–R4 per chR.json substance with F24 grants added;
  R4 folds a recorded ε0 shakedown fixture through rebuild as its seam proof.
- **C-lane** (domains: driver crate + exact `test/spec_build_*.py` files): C1
  reseat (78 tests, not 49), C2 crate + nix packaging + a per-action
  dispatcher (`SPEC_BUILD_PY_FALLBACK`) so the port lands incrementally
  green, C3 worktrees, C4 fold-half, C5 effect-half, C6 argsSchema
  single-source (flow `:11–~:510`; depends on R3; owns `crates/tally-flow`),
  C7 rust-driver-seam-proof — repoint `flow_live` at the Rust binary and run
  a full campaign pass (the port's only genuine F19/F20 oracle), C8
  delete-python. C8 double-grandfathers `agency_nightly_driver.py` **and**
  `campaign_worktrees.py` (line 40 imports it) with a named retirement issue
  (D75); the reseated Python suite survives as the language-agnostic seam
  harness.
- **Shared folds ported once**: `stable_publish_branch`, `campaign_digest`,
  `render_campaign_summary` move into `crates/tally-core` beside
  `campaign_contract`, consumed by both the release verb and the driver
  crate — never ported twice.

## 7.6 Supervision playbook (local mode)

- Campaign agent and out-of-band repair workers are **Codex** (D76). The
  orchestrator writes no tally source; its sanctioned hand-commits are this
  plan, the worklist files, and the dotfiles declaration.
- Stall predicate: registration armed ∧ zero `tally-job-*` units ∧
  `campaign quiescent` ≠ 0, sustained 720 s. A campaign at rest with pending
  dependent tasks and a free slot is usually correct behavior, not a stall
  (F23 until H3 is live).
- Recovery verb is `resume --reason`, always. Never disarm-first — disarm
  destroys the auto-pardon baseline (F17).
- "Gate fails once, then passes" is the normal shape (F14/F21; three
  campaigns for three). Budget: 2–5 out-of-band repair cycles across the
  three stages; each is a Codex worker in an isolated worktree, nothing
  pushed by the worker, merged only on a green full gate, then `resume`.
- `status` blind spots on this pin are pre-briefed as *unknown*, not alarm;
  captures under `~/.local/state/tally/capture/` are first-line forensics.

## 7.7 Decision register additions

- **D72** — Part 3 Chapters 3/4/5/P and §6.3 item 3 are superseded by the
  epsilon stages (this part). chR substance rides ε2's R-lane.
- **D73** — Worklist authority for all epsilon stages is the single committed
  file `silent-factory-worklists/epsilon.json`; content replaced only at
  registry quiescence; stage bytes pinned by completion-receipt hashes.
- **D74** (amended by D77) — Per-lane gate set for the epsilon campaign:
  `driver-suite` (`python3 test/spec_build_driver_test.py`), `cargo-tests`
  (`nix develop --command cargo test --workspace`), `flake-eval`
  (`nix flake check --no-build`) — declared in the worklist's `campaign`
  section, so changing gates is a worklist commit, never a deploy; final-bar
  rides the chapter checkpoints instead of the per-lane gate list (it was
  broken on base at ε0 authoring time, and a command gate's argv is
  witnessed on the pristine base).
- **D75** — Panel open questions resolved to defaults:
  `agency_nightly_driver.py` and `campaign_worktrees.py` are grandfathered
  together with a named retirement issue; the reviewers validation surface is
  deleted in ε1; the release act runs on the operator's ambient gh auth from
  the coordinator; the off-host steering read path (D12) stays deferred;
  `maxParallel 3`.
- **D76** — Codex replaces Claude Opus as both the campaign agent adapter and
  the out-of-band repair worker for all epsilon stages.
- **D77** — Self-contained arm (operator ruling, 2026-08-13): per-campaign
  host configuration is forbidden. The worklist document owns campaign
  policy in an optional closed `campaign` section (name, maxTasks,
  maxParallel, mergeMethod, runtimes, agent, steward, gates — an armable
  campaign must declare 1–16 gates); adapter names resolve against the host
  adapter catalog; the flow and driver default to the packaged assets beside
  the tally binary; the campaign mutex is the reserved minted
  `campaign/<owner>/<repo>` pool; the checkout comes from `--checkout`/cwd
  and is recorded in the registration (authority v4), which resume and poll
  then read. The config-document declaration scan is deleted. Landed
  out-of-band by a Codex worker before ε0 armed; the module's campaign
  rendering became vestigial and P1 deletes it.
