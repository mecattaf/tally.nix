# August 14 — the verified open-defect ledger for epsilon-extension
# Every ask, "should", pending decision and hypothesis from JULY31 → AUG14, graded against `e921cccc`

Method: swept every learnings/design document in `/home/tom/mecattaf/tally.nix`
(`JULY31-LEARNINGS.md`, `AUGUST-01-DESIGN.md`, `AUGUST-02/06/07-LEARNINGS.md`,
`AUGUST-08-HYPOTHESES.md`, `AUGUST-10/11/12-LEARNINGS.md`, `AUG11-evening-pass.md`,
`AUG12-HANDOFF.md`, `AUG12-DAYRUN-HANDOFF.md`, `AUG12-overnight.md`, `AUG13-RUN.md`,
`AUG14-LEARNINGS.md`, `SILENT-FACTORY-PLAN.md` Parts 1–7, `sodimo-aug11-learnings.md`,
`aug10-midday-session.md`) for every ask, "should", "decision waiting", "fix later"
and hypothesis that implies work. Each was then verified **against code in the tree at
`e921cccc`** — `crates/`, `nix/`, `flake.nix`, `test/`, `examples/flows/spec-build.js`,
`silent-factory-worklists/epsilon.json` — plus `git show 52eff4db:drivers/spec_build_driver.py`
for the deleted Python driver. Findings are numbered **VD-1 … VD-31**; F27–F44 are taken.

Verdicts: **OPEN** (code evidence it does not exist) · **PARTIAL** (what half exists) ·
**CLOSED** (commit/file:line evidence it landed).

Classification (load-bearing — the operator separates *what we learned building tally*
from *how to build better with tally*):

| tag | meaning |
|---|---|
| **CODE** | tally-code-defect. A wrong or missing behaviour in `crates/`, `flake.nix`, `nix/`, or the flow. Belongs in a task. |
| **DESIGN** | tally-design-decision. The code is doing what it was told; the ruling itself is what is open. Belongs to Tom. |
| **DOCTRINE** | authoring-doctrine. Belongs in a worklist template, a brief, or the `assign-tally` / `campaign-operator` skill — **not in tally's source**. |
| **DOTFILES** | host-dotfiles. Out of `tally.nix` scope entirely. |

---

## Headline

**F44 was mis-diagnosed, and the fix that closed the ladder papers over a live,
structural defect.** The run record says the trailer divergence is a Python→Rust
canonicalization skew — *"the PYTHON driver wrote every trailer, the RUST verb verifies
them, and their canonical bytes disagree"* (`AUG13-RUN.md:1004-1005`). It is not.
At `e921cccc` the tree contains **two different completion-identity contracts, both in
Rust, hashing two different tuples**: the driver writes
`sha256({contractVersion, repository, source:{repository,path}, task})`
(`crates/spec-build-driver/src/actions.rs:1199-1221`) and the release verb recomputes
`sha256({contractVersion, campaign, repository, mergeMethod, agent, steward, gates, task, content})`
(`crates/tally-core/src/campaign_contract.rs:722-753`). These can never coincide for
*any* file-worklist campaign in *any* generation. `ReleaseCompletionOracle::Exact`
(`crates/tally/src/cli/campaign.rs:1958-1962`) is therefore dead code on the only campaign
shape tally now supports, and every future release will label 100 % of its completion
proofs `bridge` — a label whose whole purpose was to mark *legacy* proofs. The bridge
proves only "some commit with the same tree id is named by a durable task ref"
(`campaign.rs:2034-2056`), which is a materially weaker statement than the trailer oracle
it replaced. See **VD-8**.

Beyond that: of the eight seeded known-opens, **seven verify OPEN or PARTIAL, and the
eighth (the `!` carve-out, F32) was never open at all** — it was fixed inside ε0 by
`2d68fca9` and mis-reported because the running driver predated the fix, which is F34's
own "H1 was not live when it was credited" error committed a second time, four sections
earlier in the same document (VD-13). The sweep found **nine further OPEN items** nobody
has written down as findings, and **fourteen items that are genuinely CLOSED** and should
stop being carried forward.

## Score

| area | verdict at `e921cccc` |
|---|---|
| the H1 refusal channel (F35) | **OPEN, CODE.** The flow has five failure classes; none is a refusal (VD-1) |
| stage-close ceremony (F38/F31) | **OPEN, CODE+DESIGN.** Zero of the nine campaign verbs archives or publishes (VD-3, VD-9) |
| D73 single identity (F38) | **OPEN by construction, DESIGN.** The summary namespace is `sha256(campaign‖issue)[..24]` — no stage, no serial, no digest (VD-4) |
| final-bar coverage (F33) | **PARTIAL, CODE.** The flake attribute still runs `--list`; execution exists only in one hand-authored checkpoint argv (VD-5) |
| per-lane gate set (F33) | **OPEN, DOCTRINE.** No clippy; `flake-eval` is `--no-build` and cannot see the class that has failed 5 of 5 chapter gates (VD-6, VD-7) |
| narrator economics (F32) | **OPEN, CODE+DOTFILES.** `NARRATION_ATTEMPTS = 2`, hardcoded; the grammar block understates the contract by ~11 rules (VD-11, VD-12) |
| the `!` carve-out (F15/F32) | **CLOSED since ε0** (`2d68fca9`); F32 mis-read a stale-pin rejection as a live defect (VD-13) |
| estate-bytes coverage (F39/R4) | **PARTIAL, CODE.** One estate-shaped row replays in a unit test; the population is still untested and no gate touches it (VD-14) |
| `projectionWaitMs` (F35) | **OPEN, DESIGN.** 10 000 ms, one scalar per registration, no per-outcome window (VD-2) |
| the ownership contract as mechanism (F22 §Ask 3) | **OPEN, DESIGN.** Still the largest unattended-operation gap; the machine diagnoses, the agent requests, only the operator can act (VD-1, VD-15) |
| the July-31 "no ceremony" rule | **regressed.** Nine ritual verbs per stage, four of them not verbs at all (VD-9, VD-16) |

---

# Part A — the seeded known-opens, verified precisely

## VD-1 — `needs-grant` does not exist anywhere in the tree; the flow has five failure classes and none of them is a refusal (F35) — **OPEN · CODE**

A tree-wide search over `*.rs`, `*.js`, `*.nix`, `*.json`, `*.md` for
`needs-grant | needsGrant | needs_grant` returns **three hits, all prose**:
`AUG13-RUN.md:942`, `AUG14-LEARNINGS.md:308`, `AUG14-LEARNINGS.md:672`.

The flow's classifier is `failureClass` at `examples/flows/spec-build.js:2101-2158`. It
returns exactly five values, and the comment above it states the whole doctrine:

> ```js
> // and each one spends one of the task's two steering attempts. Preparing a
> // worktree, rebasing, publishing and merging are campaign machinery: when they
> // fault they say nothing about the work, so they buy a bounded receipt-counted
> // retry instead.
> ```

| class | trigger (line) | budget it spends |
|---|---|---|
| `machinery` | codex tool-router stderr (2113); default (2157) | receipt-counted retry |
| `deferred` | checkpoint with unrelated work outstanding (2129) | nothing |
| `breach` | `stage === "treeDelta"` (2137) | never a retry |
| `ungated` | `stage === "treeDelta:ungated"` (2145) | aborts the lane |
| `work` | `agent`/`ownership`/`checkpoint`/`gate:*`/`regate:*` (2148-2155) | a steering attempt |

A boundary refusal is an `agent`-stage no-commit exit, so it lands in `work` and burns a
steering attempt — or times out first and lands in `machinery`. Both are wrong: the
attempt is neither the agent's fault nor a machinery fault. The driver's terminal message
for the shape is a flat error string, `crates/spec-build-driver/src/actions.rs:8068-8071`:

> ```rust
> return Err(DriverError::new(
>     "agent produced no commit relative to the prepared base",
> ));
> ```

The cruelty is that the brief **already uses the word**. `conflictDomainsBoundary`
(`spec-build.js:2184-2203`) tells the agent:

> ```js
> return `The task's conflictDomains ${JSON.stringify(projected)} are the binding write
> boundary: files your change makes false must be inside these prefixes; anything else is
> the operator's to grant. …`
> ```

So H1 instructs the agent to refuse and names the remedy, and then gives it no channel to
say which paths. F34's four grants — two of them *"agent-requested and adopted verbatim"*
(`AUG14-LEARNINGS.md:254-261`) — arrived as prose in a captured stderr tail.

**The one-line shape of the fix:** a sixth class `needs-grant`, produced when the agent's
final message parses as `{outcome: "needs-grant", paths: [...]}`, priced like `deferred`
(spends nothing) and surfaced in `campaign status` beside `blocked`.

## VD-2 — `projectionWaitMs` is one 10-second scalar per registration; a refusing agent cannot narrate inside it (F35 ask 2) — **OPEN · DESIGN**

`crates/tally-core/src/campaign_registry.rs:29-30`:

> ```rust
> /// Effective host tuning when a stable-v4 authority has no tuning sidecar.
> pub const DEFAULT_CAMPAIGN_PROJECTION_WAIT_MS: u64 = 10_000;
> ```

The sidecar's **absence is the default**, not an unknown (`campaign_registry.rs:680-684`),
so every campaign armed without `--projection-wait-ms` runs at 10 s. The value is one
`Option<u64>` on the whole registration (`campaign_registry.rs:143`) and is pushed into
argv once (`crates/tally/src/cli/campaign.rs:5644-5656`). **There is no per-node, per-stage
or per-outcome window.** Raising it to cover a refusing agent raises it for every publish
and merge node in the campaign.

This is DESIGN rather than CODE because the honest fix is VD-1 — a refusal that emits a
structured envelope needs no extra window at all. Widening the timeout is the workaround.

## VD-3 — there is no archive verb; the campaign verb set is closed at nine, and the release verb already consumes a namespace nothing produces (F38 ask 1) — **OPEN · CODE**

`crates/tally/src/cli/args.rs:183-200` — the complete `CampaignCommand` enum:

`Arm · Steer · Resume · Release · Poll · Status · List · Quiescent · Disarm`

No `archive-summary`, no `archive`, no `close`. The manual dance recorded at
`AUG14-LEARNINGS.md:379-381` — *"archive to `summary/archive/eps0-*`, delete the canonical
names, resume"* — has no mechanical form and **went wrong at both stage boundaries it
crossed**, the second time live at the ε2 tail.

Sharpest evidence that the namespace is already a first-class concept the tool refuses to
manage: the release verb **reads** archived summaries and publishes them as release
artifacts, `crates/tally/src/cli/campaign.rs:2666-2680`:

> ```rust
> .filter(|summary| {
>     summary.reference != closing_summary.reference
>         && summary.reference.contains("/summary/archive/")
> })
> .map(|summary| CampaignReleaseArtifact {
>     kind: "archived-summary".to_owned(),
> ```

Tally has a consumer of `summary/archive/` and no producer of it. That is the definition
of a half-landed mechanism (the F19/F20 shape, ninth instance).

## VD-4 — D73's collision is structural, not operational: the summary ref namespace is a function of (campaign name, issue number) and nothing else (F38 ask 2) — **OPEN · DESIGN**

`crates/spec-build-driver/src/actions.rs:4145-4160`:

> ```rust
> identity.push(0);
> identity.extend_from_slice(issue_number.as_bytes());
> sha256::digest(&identity).chars().take(24).collect()
> }
>
> fn local_state_prefix(campaign: &str, issue_number: &str) -> String {
>     format!("refs/tally/spec-build/v1/{}", state_scope(campaign, issue_number))
> }
> ```

Mirrored on the Rust CLI side, `crates/tally/src/cli/campaign.rs:4194-4198`:

> ```rust
> fn campaign_state_ref_prefix(campaign: &str, issue_number: u64) -> String {
>     let scope = format!("{campaign}\0{issue_number}");
>     let digest = format!("{:x}", Sha256::digest(scope.as_bytes()));
>     format!("refs/tally/spec-build/v1/{}", &digest[..24])
> }
> ```

**Neither carries the worklist sha256, the graph digest, `armSerial`, or a stage tag.**
Three stages that share a name and a synthesized local identity share one summary
namespace, by construction, with no operator error required.

The failure is then *guaranteed* rather than merely possible, because the write is a
byte-equality assertion, `actions.rs:5545-5563`:

> ```rust
> let reference = format!("{}/summary/{}", local_state_prefix(campaign, issue_number), digest.outcome);
> …
> let (_, observed) = write_local_blob(config, &reference, &expected)?;
> if observed != expected {
>     return Err(DriverError::new(format!(
>         "local campaign summary {reference:?} disagrees with this outcome"
>     )));
> ```

F38's own root-cause note is correct and worth restating as mechanism, not doctrine:
`quiescent` is *written by the quiescence act*, so any archive step scheduled before the
operator's terminal act is unreachable by definition.

**Two clean exits.** (a) Put `graph.executableDigest[..8]` or the worklist sha in the ref
path and the collision disappears without an archive step at all. (b) Keep D73 and mint
`tally campaign archive-summary <tag>` as the terminal half of `disarm`. (a) is cheaper
and deletes a verb instead of adding one — the F27 move.

## VD-5 — the final bar's only flake attribute still executes nothing; fleet-gate has no final-bar step at all (F33 ask 2) — **PARTIAL · CODE + DOCTRINE**

`flake.nix:3465-3471`, unchanged since the plan called it out at `SILENT-FACTORY-PLAN.md:783-787`:

> ```nix
> final-conformance-bar-harness = pkgs.runCommand "tally-final-conformance-bar-harness" { } ''
>   ${finalConformanceBar}/bin/tally-final-conformance-bar --list > cases.txt
>   grep -Fq 'campaign-full-pipeline' cases.txt
>   grep -Fq 'parallel-population-wave' cases.txt
>   grep -Fq 'usage-codex-cumulative-delta' cases.txt
>   cp cases.txt "$out"
> '';
> ```

It asserts that three strings appear in a `--list` listing. It runs zero cases. This is
the attribute that let *"four `test/final-bar` call sites still pass the deleted
`--allow-test-local-forge`"* (`AUG13-RUN.md:833-835`) survive an entire chapter.

`test/fleet-gate.sh:248-254` — the full ladder — has **no final-bar step** either:

> ```sh
> run_step "cargo fmt" nix develop --command cargo fmt --all --check
> run_step "cargo test" …
> run_step "cargo clippy" … -- -D warnings
> run_cargo_deny_stage
> run_step "nix flake check" nix flake check -L --keep-going
> ```

The **only** place the bar executes is a hand-written checkpoint argv in one worklist,
`silent-factory-worklists/epsilon.json` → `chapter-gate.argv`:

> ```json
> ["bash","-lc","test/fleet-gate.sh \"$(git rev-parse HEAD)\" && exec test/final-bar/run \"$PWD\""]
> ```

So AUGUST-08's standing commitment #1 — *"The bar joins the permanent gate… the bar is the
artifact that [holds the whole contract], so it runs forever"* (`AUGUST-08-HYPOTHESES.md:95-99`)
— **has never been honoured**. Coverage is one author remembering to type it.
CODE half: `final-conformance-bar-harness` is a false-coverage attribute and should either
run cases or be deleted. DOCTRINE half: until it does, every chapter-gate argv must carry
the bar, and that belongs in the worklist template.

## VD-6 — the per-lane gate set has no clippy; the class has now cost two chapter-gate cycles in one run (F33 ask 1) — **OPEN · DOCTRINE**

`silent-factory-worklists/epsilon.json` → `campaign.gates` is exactly three entries:

| id | argv |
|---|---|
| `driver-suite` | `python3 test/spec_build_driver_test.py` |
| `cargo-tests` | `nix develop --command cargo test --workspace` |
| `flake-eval` | `nix flake check --no-build` |

`grep -n clippy silent-factory-worklists/epsilon.json` returns two hits, both inside the
**amendment task** `schema-example-stderr-lint` (lines 795, 814) that the class *caused*.
Clippy runs in `test/fleet-gate.sh:251-252`, i.e. only under the chapter gate.

Measured cost, from `AUG14-LEARNINGS.md:233-236`: **74 minutes for the clippy cycle
(22:06Z → 23:20Z) and 61 for the final-bar cycle (23:20Z → 00:21Z)**, plus an amendment task
and a re-arm each. Since D77 (F27) a gate change is a worklist commit, so this is one line
of JSON. Classified DOCTRINE precisely because tally already supports it and nothing in
tally is wrong — the worklist template is what is missing.

## VD-7 — `flake-eval` is `nix flake check --no-build`; it structurally cannot see the class that has failed five of five chapter gates — **OPEN · DOCTRINE** (new)

`--no-build` evaluates derivations and does not realise them. Every chapter-gate failure of
the F14/F21 class was a **check derivation failing at build time**, not an eval error:

- ch0: `checks.x86_64-linux.system-socket-execution`, `KeyError: 'observed'` (`AUG13-RUN.md:452-460`)
- ch1: `spec-build-conflict-domains`, `DriverError not raised` (`AUG13-RUN.md:658-662`)
- ch2: `spec-build-checkpoint-receipts` 3/21, plus two more found by `--keep-going` (`AUG13-RUN.md:807-812`)
- ε1: 12 of 24 final-bar cases stale (`AUG14-LEARNINGS.md:222-223`)
- ε2: clippy `large_enum_variant`, then the schema-generator stderr macros

`flake-eval` caught none of them and could not have. It is coverage-shaped and empty —
the same defect species as VD-5. AUGUST-12's F21 ask 1 asked for *"a cheap `nix flake check`
subset as a lane gate — the non-VM attributes alone would have caught both"*
(`AUGUST-12-LEARNINGS.md:134-135`). That is **not what shipped**; what shipped was the eval,
which is the cheap half that catches nothing.

Fix, one JSON line: `nix flake check -L --keep-going --no-build` → an explicit
`nix build .#checks.x86_64-linux.{spec-build-driver-tests,module-layer,…}` subset naming the
non-VM attributes.

## VD-8 — F44's real root cause: two divergent completion-identity contracts, both in Rust, both live; the "bridge" oracle is now permanent and weaker — **OPEN · CODE (highest severity in this ledger)**

The run record's diagnosis (`AUG13-RUN.md:1000-1006`):

> "failed the trailer oracle: expected sha256:c1a6a166…, merged trailer sha256:b68c64f9…,
> with task+campaign bytes verified identical and `campaign_contract.rs` untouched in ε2 —
> the PYTHON driver wrote every trailer, the RUST verb verifies them, and their canonical
> bytes disagree. **A release verb's first run always faces its predecessor generation's proofs.**"

The bolded sentence is the mis-attribution. Verified at `e921cccc`:

**The writer** — `crates/spec-build-driver/src/actions.rs:1199-1221`, called once at
`:1422` when the worklist is witnessed:

> ```rust
> fn file_task_completion_revision(repository: &str, source: &BTreeMap<String, Json>, task: &Json) -> Result<String> {
>     …
>     Ok(canonical_sha256(&Json::object([
>         ("contractVersion", Json::Number("1".to_owned())),
>         ("repository", Json::from(repository)),
>         ("source", Json::object([("repository", …), ("path", …)])),
>         ("task", task.clone()),
>     ])))
> ```

**The verifier** — `crates/tally-core/src/campaign_contract.rs:722-753`, called only from
the release verb (`crates/tally/src/cli/campaign.rs:1912`):

> ```rust
> let bytes = canonical_json(CompletionPolicy {
>     contract_version: 1,
>     campaign: &manifest.name,
>     repository: &manifest.repository,
>     merge_method: &manifest.merge_method,
>     agent: &manifest.agent,
>     steward: &manifest.steward,
>     gates: &manifest.gates,
>     task: reference,
>     content,
> })?;
> ```

**Not variants of one tuple — two different tuples with different semantics.** And the
deleted Python driver's formula (`git show 52eff4db:drivers/spec_build_driver.py:1514-1535`)
is byte-for-byte the *writer's*, field for field:

> ```python
> return canonical_sha256({
>     "contractVersion": 1,
>     "repository": repository,
>     "source": {"repository": source.get("repository", repository), "path": source["path"]},
>     "task": task,
> })
> ```

So **the divergence is driver-versus-release-verb, not Python-versus-Rust**, and it is
fully present in the all-Rust tree. Three consequences:

1. **`ReleaseCompletionOracle::Exact` is unreachable for file-worklist campaigns.**
   `campaign.rs:1949-1962` looks up `revisions.get(task_id)` — the manifest-policy value —
   in a map keyed by the trailer the driver wrote. The keys are drawn from different
   functions. Every future release falls into the `None` arm at `:1968` and bridges.
2. **The `bridge` label will mark 100 % of proofs, forever.** Its own CHANGELOG entry
   (`e921cccc`) describes it as bridging *"one legacy Python-revision trailer claim"*.
   It is not legacy; it is the steady state.
3. **The bridge is a materially weaker proof.** `release_completion_bridge_ref`
   (`campaign.rs:2034-2056`) accepts a commit when a durable ref under the campaign
   generation prefix has leaf `{task_id}-{revision[7..23]}` **and the same `tree_id`**. It
   does not verify the completion policy at all — it verifies that some ref the campaign
   itself wrote points at a tree identical to the merged one. The trailer oracle's whole
   value (a commit message binding a task to an approved policy) is gone.

A fourth, quieter divergence: the release-verb contract hashes **`gates`**. Under it,
amending a gate rotates every task's completion revision — which directly contradicts F27's
headline benefit, *"Changing a gate is a worklist commit, never a deploy"*
(`AUG14-LEARNINGS.md:80`). Under the driver's contract it does not. **The two contracts
disagree about what a completion proof means**, and that disagreement is the finding, not
the sha mismatch.

Also verify: `CanonicalCampaignTaskV1` is `{number, title, body}`
(`campaign_contract.rs:185-189`) and the CLI builds it from rendered worklist prose
(`campaign.rs:4785-4789`) — it never carries the driver's per-task `revision`. There is no
path by which the release verb could have learned the writer's value.

---

# Part B — OPEN items the sweep found that no finding number covers

## VD-9 — the stage close is a nine-verb ritual and four of its steps are not verbs — **OPEN · DESIGN**

Reconstructed from `AUG13-RUN.md:990-1011` and `AUG14-LEARNINGS.md`, the ε2 close in order:

| # | act | mechanised? |
|---|---|---|
| 1 | wait for `complete`, campaign **stays armed** | ✓ |
| 2 | `tally campaign quiescent` | ✓ verb |
| 3 | `tally campaign disarm` | ✓ verb |
| 4 | archive **both** summary refs post-disarm | ✗ raw `git update-ref`/`push` (VD-3) |
| 5 | rebase the integration head onto the main tip | ✗ raw `git rebase` (VD-10) |
| 6 | push the published sha; record it **as distinct from the proven sha** | ✗ operator bookkeeping |
| 7 | re-arm `--no-enqueue` because release needs an armed registration | ✗ workaround (VD-16) |
| 8 | `tally campaign release --plan` → probe → `--execute` | ✓ verb |
| 9 | final `disarm`; archive summary refs **again** (`eps2-final-*`) | ✗ |

Five of nine steps have no verb. This is the exact shape JULY31 named as defect #1 —
*"Ceremony scaled with campaigns, not with work"* (`JULY31-LEARNINGS.md:15-19`) — and the
lineage's standing house rule is "no ceremony". The ε-era ceremony is smaller than
July's but it is **per stage**, so it scales with stages, and the ladder ran three.

The cheapest collapse is `disarm` growing a terminal contract: archive both summary refs,
emit the published/proven sha pair, and leave the registration in a state `release` accepts.

## VD-10 — the publish rebase is permanent, unmechanised, and silently decouples the proven sha from the published sha (F31) — **OPEN · DESIGN**

F31's own conclusion (`AUG14-LEARNINGS.md:164-167`):

> - **Never assume the checkpoint sha is what lands.** Record both.
> - The publish is an operator act with a rebase in it, every stage, forever, as
>   long as amendments are how ownership is granted.

Verified: the integration branch is cut once at arm
(`stable_publish_branch(campaign, &registration.registration_id, "integration", None)`,
`crates/tally/src/cli/campaign.rs:1497`) and nothing re-bases it onto operator commits.
Since grants *are* worklist commits on `main` (F34, four of them this run), the divergence
is structural.

Two costs the run record does not connect:
- Every stage produced a pair (`914c791f`→`6fdf108f`, `6afee3aa`→`b4e655c8`, integration→`a8077295`)
  and the *only* record of the pairing is prose in `AUG13-RUN.md`.
- **VD-8's bridge oracle matches on `tree_id`.** A content-disjoint rebase preserves tree
  ids, so it works today — but the release verb's only surviving proof mechanism is now
  coupled to a property of a hand-run `git rebase`. Nothing tests that coupling.

## VD-11 — `NARRATION_ATTEMPTS = 2` is a hardcoded constant; two independent rules firing guarantees fallback (F32 ask 2) — **OPEN · CODE**

`crates/spec-build-driver/src/actions.rs:31`:

> ```rust
> const NARRATION_ATTEMPTS: u64 = 2;
> ```

Consumed at `:8965` (`for attempt in 1..=NARRATION_ATTEMPTS`). Not a manifest field, not a
`campaign` section key, not an env var. The measured outcome across the run:
**70 rejections, 35 merges, 0 narrated subjects** (`AUG14-LEARNINGS.md:58`).

The loop is otherwise well built — it feeds `previousRejection` forward
(`actions.rs:8972-8977`) — which makes the budget the binding constraint rather than the
model. Attempt 2 of `tally-rebuild-verb` (`git show b1905c86`) is the canonical shape:

> `attempt 1 (rejected): final message is not valid JSON; attempt 2 (rejected): header is 75 characters, over the 72 cap`

Two orthogonal faults, two slots, fallback. Raising the constant to 4, or not charging a
slot for a malformed envelope, recovers most of the 70.

## VD-12 — the steward is told four rules and graded on fifteen (F32 ask 3) — **OPEN · CODE** (new; this is the mechanical cause of ~38 % of rejections)

The request payload's `grammar` block, `crates/spec-build-driver/src/actions.rs:9578-9588`,
contains exactly four keys:

> ```rust
> ("grammar", Json::object([
>     ("types", Json::Array(NARRATION_TYPES…)),
>     ("headerMaxChars", Json::from(NARRATION_HEADER_MAX)),
>     ("bodyMaxChars", Json::from(NARRATION_BODY_MAX)),
>     ("bodyMaxColumns", Json::from(NARRATION_BODY_LINE_MAX)),
> ])),
> ```

`validated_narration` (`actions.rs:8534-8692`) plus `validate_outcome_first`
(`:8475-8520`) enforce, in addition and **without telling the model any of it**:

| enforced rule | line | in the grammar block? |
|---|---|---|
| unknown proposal keys rejected | 8543-8552 | no |
| `scope` must match `^[a-z0-9][a-z0-9._/-]{0,31}$` | 8574 | no |
| subject must not end with `.` | 8598 | no |
| subject must not start with a capital | 8601 | no |
| header cap applies to `type(scope): subject`, not to subject | 8613 | **no — this is the 72-char trap** |
| no managed completion trailer | 8661 | no |
| no `Assisted-by:` trailer | 8667 | no |
| no GitHub closing keyword | 8673 | no |
| no `@mention` | 8679 | no |
| body leading sentence must end `.` or `:` | 8499 | no |
| body must open with a past-tense verb | 8514-8519 | no |
| body must not open with a list marker | 8492 | no |
| body leading sentence ≤ 240 chars | 8506 | no |

Measured consequence, `AUG14-LEARNINGS.md:184-189`: *"proposal body leading sentence must
end with a period"* 11, *"body wraps past 100 columns"* 11, *"header is N characters, over
the 72 cap"* 6, *"must open with a past-tense verb"* 2 — **30 of 70 rejections are rules the
model was never given**. F32 asked for the *budget* to be stated; the sharper statement is
that the contract is stated at 4/15 fidelity.

`headerMaxChars: 72` is actively misleading: the model reads it as a subject budget and it
is a header budget, so a `type(scope): ` prefix silently eats ~24 characters. F32 computed
the real budget at ~48.

The remaining 38 of 70 — *"final message is not valid JSON"* — is `DOTFILES` (the narrator
shim), and F32 ask 1 is correct that fixing it recovers 54 %.

## VD-13 — the `!` carve-out already covered narration; F32 credited a live rejection to code the running pin did not carry — the F34 error, second instance (F32 ask 3, third clause) — **CLOSED · CODE**

The finding said (`AUG14-LEARNINGS.md:201-203`):

> **F15's bang rule still gags the steward.** ε0's `steering-grammar-negation` (`2d68fca9`)
> permitted `!` inside inline code for *machine diagnoses*; the steward narration validator
> still rejects it outright. Two rejections this run.

**The premise is wrong.** `2d68fca9` did not carve out the diagnosis path; it carved out
`validate_outcome_first` itself — *the shared validator*, which the narration path calls.
From `git show 2d68fca9 -- drivers/spec_build_driver.py`:

> ```diff
> -    if "!" in text:
> +    if contains_bare_exclamation_mark(text):
> ```

and the surrounding comment names the design as one contract for three callers:
*"One validator, three callers, so the contract lives in exactly one place."*
Confirmed live on the ε1 head: `git show b4e655c8:drivers/spec_build_driver.py` has the
carve-out in `validate_outcome_first` (`:236-244`) and four call sites (`:1477`, `:1599`,
`:3280`, `:4222`), narration among them.

So why were there two `!` rejections? **Because the running driver predated the fix.** The
deployed generation through ε0 and ε1 was 125 = `6a7c841a` (`AUG13-RUN.md:971`), which
predates `2d68fca9` — a commit that merged *inside ε0* and only reached the fleet at
deploy-2 (gen 126/127, 00:49Z/01:35Z, `AUG14-LEARNINGS.md:56-57`). Its own merge commit
message proves it, verbatim:

> `Rejected 2 steward narration proposal(s) … attempt 1 (rejected): proposal body contains an exclamation mark`

— the commit that fixes the bang rule was itself gagged by the bang rule, because the
driver grading it was the old one.

**This is F34's error a second time, and nobody caught it.** F34 correctly demoted the H1
credit — *"Mechanically that cannot be the cause. The driver is resolved from the deployed
store path"* (`AUG14-LEARNINGS.md:263-270`) — and then F32, four sections earlier in the same
document, made the identical mistake in the opposite direction. **Doctrine, and it should go
in the operator skill: before attributing a live behaviour to any merged commit, diff the
deployed store path.** `tally campaign quiescent` prints `flow` and `driver` for exactly this
(`AUG14-LEARNINGS.md:646-649`) and neither finding used it.

At `e921cccc` the property survives the port: `crates/spec-build-driver/src/actions.rs:8483-8485`
calls `contains_bare_exclamation_mark` (`:8432-8442`), which skips `closed_inline_code_spans`
(`:8391-8429`), doc-commented *"Byte ranges with the exact stage-0 inline-code negation semantics."*

Residual (LOW · CODE): the *subject* has no `!` check at all (`:8588-8605`), so `!` is legal
in a header and constrained in a body. Harmless, but the asymmetry is unintended.

## VD-14 — the estate replay is one hand-composed row in a unit test; the population is still untested and no gate reads a real estate (F39 ask 1 / R4) — **PARTIAL · CODE**

The plan's own instruction, `silent-factory-worklists/epsilon.json` → `tally-rebuild-verb.goal`:

> "Its seam proof MUST include an estate-shaped replay: a fixture sampled from real
> historical rows including the legacy gh-field shapes the D33 decode sink tolerates (the
> deploy-2 regression proved suites that only test fresh bytes miss the estate class —
> **this verb is where estate-bytes coverage lives from now on**)."

What landed (`b1905c86`), verified:

- A fixture exists and is real-derived: `test/fixtures/ledger/events/legacy-gh-origin.enqueue.json`
  and `test/fixtures/ledger/legacy-gh-fields.payload.json`, both built by the PR #605 worker
  from *"two real captured rows"* (`AUG14-LEARNINGS.md:451-452`). The enqueue fixture carries
  `"source": "calendar"` with an explicit-null `"ghOrigin"` — exactly the deploy-2 shape.
- One test replays it: `estate_gh_row_rebuild_matches_the_live_derived_projection`,
  `crates/tally-core/src/durable_view.rs:997-1060`. It writes one event into a tempdir,
  decodes it, runs `rebuild_run_view`, and compares against an independently constructed
  live side — a genuinely good test.

**What is still missing:**
1. The test **overwrites** the fixture's `ghOrigin` with an inline literal at
   `durable_view.rs:1010-1017` and synthesises `orchestration` and `runtimeMaxSec` at
   `:1018-1027`. So the bytes under test are hand-authored, not sampled. The fixture is
   estate-*shaped*; the case is not an estate *sample*.
2. **One row.** The deploy-2 regression was a property of **4,272 event files**
   (`AUG14-LEARNINGS.md:427-431`). Nothing replays a population, and nothing samples the
   operator's `~/.local/state/tally/`.
3. **No gate touches it.** `rebuild` is a CLI verb; there is no flake check, no fleet-gate
   step, and no per-lane gate that runs `tally rebuild` against anything durable. The class
   with the only fleet-down event of the ladder has one unit test and zero gate coverage.

F39 ask 3 — *"Any task deleting a serde field on a `deny_unknown_fields` struct must carry a
named legacy accept-and-discard arm as a delivered behavior"* — is **DOCTRINE and OPEN**:
`DiscardedLegacyGhField` exists (`crates/tally-core/src/wire.rs:452-468`,
`taskdb.rs:328-337`) as an *instance*, but nothing in the worklist template or any lint
enforces the rule for the next deletion.

## VD-15 — the machine can only diagnose; that is still the largest unattended-operation gap, unchanged since AUGUST-12 — **OPEN · DESIGN**

`AUGUST-12-LEARNINGS.md:178-182` (F22 ask 3) and `AUG14-LEARNINGS.md:605-608` say the same
sentence three months apart:

> **The machine can only diagnose.** On a failed attempt it prints the exact paths a task
> would need. It has no verb that can act on its own conclusion. **This remains the largest
> single unattended-operation gap in the system.**

Verified: nothing in `crates/` or the flow writes to a worklist. `campaign.rs` reads the
worklist at the fetched base revision (`:4663-4747`) and never writes one. The four grants
of this run are four hand commits (`663de5bc`, `1324eaa4`, `ef0443f8`, `05aec25d`).

This is DESIGN, and it is the decision that determines whether epsilon-extension is
attended or not. VD-1 is its cheap half (make the request first-class); the expensive half
is whether a machine-proposed worklist diff may ever be auto-applied. F22's own framing —
*"Decide whether the ownership contract becomes mechanism or doctrine"*
(`AUGUST-12-LEARNINGS.md:264-268`) — is still the open question and is now measured: H2's
lint caught **0 of 4** (F40), so "doctrine" currently means "the operator, every time".

## VD-16 — `campaign release` requires an armed registration, so the operator must re-arm after disarming — **OPEN · CODE** (new)

`AUG13-RUN.md:997-1000`:

> "the release window requires an ARMED registration, so the identity was re-armed
> `--no-enqueue` after my premature disarm, and the integration ref restored under the new
> registration id"

Re-arming a completed campaign to release it, then disarming a second time, is ceremony
that also **rotates the registration id**, which the durable refs and the VD-8 bridge
oracle are keyed on ("the integration ref restored under the new registration id" is a
hand-repair of exactly that). Release should read the durable completion refs, which are
the canonical facts — the registration is derived state.

## VD-17 — `#518` preflight rehearsal verb: never built — **OPEN · CODE**

Filed 2026-08-11 (`AUGUST-11-OVERNIGHT.md:92`: *"#518 preflight rehearsal verb (#484 ask 1)"*),
descended from AUGUST-10's item 9 — *"validate every checkpoint argv under an equivalent
`systemd-run` sandbox before arming, because the checkpoint environment is stricter than
your shell"* (`AUGUST-10-LEARNINGS.md:228-234`).

`grep -rn "rehears" crates/` returns **nothing**. `preflightArgv` exists in the gate schema
(used by all three epsilon gates) and is a *precondition test*, not a rehearsal of the real
argv under the real unit. AUGUST-10's item 9 cost *"three sandbox iterations to green"* and
its lesson is currently pure operator memory.

## VD-18 — impedance 1: a `forge:"local"` campaign still requires a push to a remote before it can arm — **OPEN · DESIGN**

`SILENT-FACTORY-PLAN.md:365-370`:

> **Worklist authority is the remote base branch.** Even a `forge:"local"` campaign runs
> `git fetch <remote>` and reads the worklist at `<remote>/<baseBranch>` — so these files
> must be *merged to main and pushed* before arming.

Still exact at `e921cccc`. `crates/tally/src/cli/campaign.rs:4663-4664`:

> ```rust
> &["fetch", "--prune", "--no-tags", &repository.remote],
> "fetching the local campaign worklist authority",
> ```

and the worklist is resolved *"at fetched base revision"* (`:4736`, `:4747`). Comment at
`:9241`: *"The checkout may be dirty; the fetched remote base remains the only …"*.

This is the mechanism behind F31 (VD-10) *and* the grant cycle: every grant is a push, which
is why the integration branch drifts. It is DESIGN — the property is deliberate — but it
means "local mode" is a misnomer, and it should either be renamed or given a path-URL remote
path so a genuinely offline campaign is possible.

## VD-19 — the F25 class (host state leaking into the suite) closed by deletion, not by isolation — **PARTIAL · CODE**

The instance is gone: `crates/tally/tests/migrate_cli.rs` no longer exists (the `gitAi`
chapter deleted `tally migrate`). The **class** is unguarded. Config resolution still falls
back to ambient `$HOME` (`crates/tally-client/src/lib.rs:325-329`):

> ```rust
> if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") { … }
> Ok(PathBuf::from(home).join(".config/tally/config.json"))
> ```

Only two integration tests pin an isolated config home (`direct_file_defaults.rs`,
`flow_cli.rs`). Any future test that spawns `tally` without `--config` reproduces F25
verbatim, and — as F25 noted — *"only a campaign that edits the very tool whose config is
deployed on the grading host can hit it"*, which is every campaign in this repo.
The cheap guard is a test-harness assertion that no spawned `tally` inherits a real
`XDG_CONFIG_HOME`/`HOME`.

## VD-20 — the campaign gate cap is 1..=16 and the epsilon set uses 3; nothing prevents the right gate set, and nothing encodes it — **OPEN · DOCTRINE**

`crates/tally-core/src/campaign_contract.rs:485-487`:

> ```rust
> pub fn validate_gates(gates: &[CampaignGate]) -> Result<(), CampaignContractError> {
>     if gates.is_empty() || gates.len() > 16 {
>         return Err(invalid("campaign gates must contain 1..=16 entries"));
> ```

Thirteen slots unused. Combined with VD-6 and VD-7 the honest reading is that **the gate set
is the single highest-leverage authoring artifact in tally and there is no template for it.**
Every finding in the F14/F21/F33 family is a gate-set authoring miss, five chapters running.
That is a worklist-template item, not a tally item.

## VD-21 — D55 / `#523` is still unruled, and this repo's root is 15 uncommitted lineage documents — **OPEN · DESIGN**

`SILENT-FACTORY-PLAN.md:109`:

> **D55 — `#523` (lineage prose / `tally note` technotes) stays a standing queue item** —
> the root `AUGUST-*.md` question defers to it; the tree stays code either way.

`AUGUST-11-OVERNIGHT.md:114-118` asked for *"one word"* and got none. The consequence is
material: `git status` at session start shows `AUG12-DAYRUN-HANDOFF.md`, `AUG12-HANDOFF.md`,
`AUG12-overnight.md`, `AUG13-RUN.md`, `AUGUST-11-OVERNIGHT.md`, `AUGUST-12-LEARNINGS.md`,
`aug10-midday-session.md`, `aug12-campaign-prep/` **all untracked**, alongside two modified
`skills/*.md` first noted as stale on **2026-08-12** (`AUG13-RUN.md:549-551`: *"They are still
uncommitted and are NOT this session's work"*) and still uncommitted two days later. The
ladder's own primary sources are not under the ladder's own merge control.

---

# Part C — the CLOSED ledger (stop carrying these forward)

Verified landed. Each row names the evidence.

| # | item (source) | evidence at `e921cccc` |
|---|---|---|
| VD-22 | **F18** Boa `i32` boundary — keep a test pinning a real comment-magnitude id | `crates/tally-flow/src/engine/interop.rs:306` pins `5266404097` and the ±2^53 exact limits; `:328` pins the non-integral case |
| VD-23 | **F19/F20** D62 repo-scoped pools reachable end to end | daemon admission `78dd4871`, flow argsSchema `2cc08bec`; epsilon armed on `campaign/mecattaf/tally.nix` for 3 stages with no pool fault |
| VD-24 | **F22 ask 1 / H1** ship the boundary into the brief | `examples/flows/spec-build.js:2184-2203` `conflictDomainsBoundary`; projected by the driver at `crates/spec-build-driver/src/actions.rs:7070-7077` and `:7016-7021` |
| VD-25 | **F22 ask 2 / H2** ownership preflight that warns, never gates | `crates/tally/src/cli/campaign.rs:5136` `ownership_preflight_warnings`, surfaced as arm warnings at `:6013`. *Caught 0 of 4 grants (F40) — keep it, stop expecting it to be the answer* |
| VD-26 | **F23 / H3** poll liveness arm | `crates/tally/src/cli/campaign.rs:4299` `dispatchable_poll_liveness_arm`, `:4354`, wired at `:6305`. Zero wake-pardons in ε2 (F41) |
| VD-27 | **F30 / H4** `status` renders reconciled truth | `crates/tally/src/cli/campaign.rs:6556` `reconciled_campaign_status`, `:6442` `most_recent_reconciled_campaign_run`, unreconciled predicate `:6565` |
| VD-28 | **F21 ask 2** chapter gate reports all failing attributes | `test/fleet-gate.sh:254` — `nix flake check -L --keep-going` |
| VD-29 | **#455** steward diagnosis literal-substring grammar never told the model | `crates/spec-build-driver/src/actions.rs:2102-2108` now emits `" Required literal check id: {}."` and `" Required literal offending path: {}."` into the prompt; diagnosis quality 16-for-16 this run (F36) |
| VD-30 | **#460** `maxTasks` error names its field | `crates/tally-core/src/campaign_contract.rs:359-362` — *"campaign contains {} tasks but manifest maxTasks is {} — raise \"maxTasks\" in the campaign manifest"* |
| VD-31 | **#519** journal campaign key | `crates/tally-core/src/journal.rs:1292` — scopes `["attempt","task","pass","campaign"]` |
| — | **D56** the ~6k-LOC gh producer stack ruling | answered by deletion in ε1 (`delete-gh-inbound-core`, 14,741 lines). `crates/tally-core/src/producers/` is `{config,engine,ingress,mod,tests,validate}.rs`; `gh_intake.rs`/`gh_decision.rs`/`ghProducerType` all absent |
| — | **D57** `build-effect` / `pool-reachability` producer kinds | absent from `crates/` and `nix/` |
| — | **F15** the `!` gag on machine diagnoses | `crates/spec-build-driver/src/actions.rs:2116` `replace_bare_exclamation_marks(excerpt, ".")`; carve-out at `:8391-8442`. Confirmed working under redaction (F36, episode 3) |
| — | **#457** checkpoint output capture; **#233** `tally adapter smoke`; **#456** auto-pardon | all in the pin and exercised this run (`AdapterSmoke` in `args.rs:385`; auto-pardons recorded on every ε re-arm) |
| — | **AUGUST-10 item 5** `[marker]` PR title prefix | obsolete: marker PRs do not exist in local mode; the integration branch replaced them (D14–D15) |
| — | **F25 instance** `migrate_cli.rs` reads the deployed config | file deleted with `tally migrate`. *Class open — see VD-19* |

**Still not started, low value, recorded so it stops being re-derived:** `test/eval_manifest_check.py`
(D57's second clause) is wired into no flake check; it is reached only through `test/final-bar`
fixtures (`test/final-bar/fixtures/eval-manifest/`), which VD-5 shows do not execute in any
permanent gate. Either wire it or delete it, as D57 said on 2026-08-11.

---

# Part D — classification roll-up

| verdict | CODE | DESIGN | DOCTRINE | DOTFILES |
|---|---|---|---|---|
| **OPEN** | VD-1, VD-3, VD-8, VD-11, VD-12, VD-16, VD-17 | VD-2, VD-4, VD-9, VD-10, VD-15, VD-18, VD-21 | VD-6, VD-7, VD-14(ask 3), VD-20 | VD-12 (JSON envelope, 38/70), F32 ask 1 |
| **PARTIAL** | VD-5, VD-14, VD-19 | — | VD-5 (second half) | — |
| **CLOSED** | VD-13, VD-22…VD-31 | D56, D57 | VD-13 (the stale-pin attribution rule) | — |

**The separation, stated plainly.** Seven of the sixteen open items are things tally's code
does wrong or does not do (VD-1, 3, 8, 11, 12, 16, 17). Seven are rulings only Tom can make
(VD-2, 4, 9, 10, 15, 18, 21). **Four are not tally's problem at all** — VD-6, VD-7, VD-20 and
F39 ask 3 are worklist-authoring misses that a gate-set template and a census rule would have
prevented, five chapters running, and putting any of them into tally's source would be the
wrong fix. One is dotfiles.

The single most valuable artifact epsilon-extension could produce that is **not code** is a
**worklist template with a mandatory gate set** — clippy, a named non-VM `flake check` subset
built rather than evaluated, and the final bar — plus the two census rules F39 and F42 already
wrote down. That erases the largest recurring cost in the ledger without touching a crate.

---

# Asks / decisions

1. **VD-8 is the only thing on this list that is currently shipping a weakened proof.**
   Decide which completion contract is canonical — the driver's file-worklist coordinate or
   the release verb's manifest-policy coordinate — and delete the other. If the release
   verb's wins, note that it binds `gates`, which reverses F27's "changing a gate is a
   worklist commit". Until then every `tally campaign release` labels 100 % of its proofs
   `bridge`, and the run record's Python-vs-Rust explanation should be corrected in
   `AUG13-RUN.md` so it is not inherited.
2. **Make `needs-grant` a sixth `failureClass` (VD-1), priced like `deferred`.** The agent
   already produces the content and the brief already uses the word. This plus (3) is what
   makes the next campaign unattended, and it makes VD-2 unnecessary rather than tuned.
3. **Choose between D73 and a verb (VD-4).** Putting eight hex of the graph digest in
   `local_state_prefix` deletes the archive step entirely; minting
   `tally campaign archive-summary` keeps D73 and adds a verb. The first is the F27 move
   ("the cheapest campaign mechanism is the one that does not exist") and I recommend it.
4. **Fold the four unmechanised steps of the stage close into `disarm` (VD-9).** Nine acts,
   five without verbs, per stage, is the July-31 ceremony finding wearing 2026-08 clothes.
5. **Write the gate-set template (VD-6, VD-7, VD-20) before authoring epsilon-extension's
   worklist.** Three lines of JSON erase two of this run's three chapter-gate cycles.
6. **Either run the final bar in `flake.nix` or delete `final-conformance-bar-harness`
   (VD-5).** A check attribute that asserts a `--list` listing contains three strings is
   worse than no attribute: it is the reason the bar rotted for a whole chapter.
7. **Ship the narrator's real contract (VD-12) before touching the shim.** Four rules
   advertised against fifteen enforced accounts for 30 of 70 rejections; the shim's JSON
   envelope accounts for 38. Both are cheap; only the first is in this repo.
8. **Rule on D55/#523 (VD-21).** Fifteen root lineage documents and two `skills/*.md` edits
   from 2026-08-12 are uncommitted. The ladder's own evidence base is outside its own merge
   control, and every agent that reads this repo reads an untracked tree.
9. **Standing authoring rule from VD-14:** `tally-rebuild-verb` declared two delivered
   behaviours and its acceptance criteria pinned only the first. `verb-exists`
   (`grep -rn 'rebuild' crates/tally/src/cli/args.rs | head -1`) was honest — `git show
   b1905c86^:crates/tally/src/cli/args.rs | grep rebuild` is empty, so the verb genuinely did
   not exist — but **nothing in the ACs named the estate fixture**, and `workspace-green`
   would have passed with the second behaviour absent. The rule: *every delivered behaviour
   needs an acceptance criterion that fails without it.* The one task that owns the ladder's
   only fleet-down defect class is the one where this slipped, and VD-14's gaps are the
   direct result.
10. **Add the stale-pin attribution rule to the operator skill (VD-13).** Twice in one
    document a live behaviour was attributed to merged code the running driver did not
    carry — once demoted (F34, H1), once not (F32, the bang rule). The check is one command
    the run already documents as its best health probe: `tally campaign quiescent` prints
    `flow` and `driver` store paths. *Diff the deployed store path before crediting or
    blaming any commit.* This is the cheapest doctrine item in the ledger and it invalidated
    a finding in the ladder's own closing record.
