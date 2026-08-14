All reads done. I have the machinery mapped. Composing the seam map now.

# SEAM MAP — where the house spec layer plugs into tally

Method note: every "today" claim below was read from the tree at HEAD, not recalled. Line numbers cite the current working tree of `/home/tom/mecattaf/tally.nix`.

The one governing observation, before the seams: **the pressure that justifies the spec layer is already visible as strain inside one field.** `goal` is capped at 12,000 chars (`examples/flows/spec-build.js:1057`) and the live worklist uses it as a spec surrogate — `epoch-scoped-budgets`' goal carries requirement prose, evidence citations (CA-3, F37, F38), and even source coordinates ("campaign.rs near :4183") in a single string (`silent-factory-worklists/epsilon-extension.json:77`). Every seam below is, one way or another, a place where that compressed payload can be decompressed into a citable layer without the machinery noticing.

---

## Seam 1 — `readFirst.specSections`: the worker context pointer

**What exists today.** The field is required, minItems 1, free strings up to 1000 chars (`examples/flows/spec-build.js:1063–1075`; mirrored validation at `crates/tally/src/cli/campaign.rs:7248–7260` and `crates/spec-build-driver/src/actions.rs:670–715`). The strings are rendered verbatim into the task brief under `## Read first` (`campaign.rs:6800–6810`), and the flow's mission tells the worker: *"Before changing code, read the cited spec sections and style references"* (`spec-build.js:2209`). **No machine check resolves them.** The D68 rule ("pointers must exist at the authority revision") lives only as a doctrine sentence in `skills/assign-tally/SKILL.md:25`. The live worklist points at prose locators, not paths: `"EPSILON-EXTENSION.md ext0"`. Meanwhile the *documentation* already models the destination genre: `doc/src/flows/campaigns.md:1341` shows `"specs/001-crm/spec.md#customer-model"` — the doc anticipated this layer before it existed.

**What attaches.** Pointers of the form:

```json
"readFirst": {
  "specSections": [
    "specs/epsilon-extension/spec.md#requirement-2",
    "specs/epsilon-extension/spec.md#unchanged-behavior",
    "specs/epsilon-extension/contracts/receipt-envelope.schema.json"
  ],
  "styleReferences": ["crates/tally-core/src/campaign_registry.rs"]
}
```

**Granularity: requirement, not criterion, not file.** A worker discharging 2.1 needs Requirement 2's *why* and its sibling criteria for context; criterion-level anchors would fragment that. File-level pointers fail the other way: agency `spec.md` files run to hundreds of lines across 20 domains, and whole-file pointers destroy the D13 two-budget model (spec bytes read ≠ spec bytes needed). Contracts are cited whole-file — they are already byte-granular.

**Anchor stability is a format obligation, not a hope.** GitHub-style heading slugs derived from titles break on retitling. Format rule: requirement headings follow the fixed grammar `### Requirement N — <name>`, and the anchor is derived from the *number only* (`#requirement-2`), never the name. Section anchors are the fixed set `#destination`, `#rulings`, `#unchanged-behavior`, `#stages`. The linter (Seam 3) enforces the grammar; the doc example's name-slug anchor (`#customer-model`) should be re-rendered to match once format v1 lands.

**What flows across / what never crosses.** Only the pointer string crosses into machinery (brief rendering). The worker reads the bytes itself in its lane worktree, which is checked out at the admitted revision — **D68 is mechanized by co-location**: the spec must live in the same repository as the worklist, or the pointer is phantom by construction. No spec content is ever copied into machinery state; the machinery never parses the md. And one exclusion: `specs/constitution.md` and `specs/README.md` never appear in `readFirst` — law and format are authoring-time inputs, not worker context; putting the constitution in every task floods the context budget with text no acceptance argv exercises.

---

## Seam 2 — epoch keying: should a `specSha256` join the stamps?

**What exists today / lands in ext0.** `arm_serial` lives in `crates/tally-core/src/campaign_registry.rs:49`; the ext0 task `receipt-authority-stamp` adds `armSerial + worklistSha256 + writtenAt` to every receipt; `epoch-scoped-budgets` derives attempt counting from "the task's own bytes in the admitted worklist, the campaign gate set, and the steering high-water mark." The flow schema *already* carries a per-task `revision: "sha256:…"` byte-hash (`spec-build.js:1051,1099`, computed via `sha256_json`, `campaign.rs:4189–4193`), and the worklist source record is a triple `{path, sha256, revision}` where `revision` is the admitted **commit** (`spec-build.js:1132–1136`).

**Answer: the spec must NOT join the epoch input, and needs no stamp of its own.**

*Epoch input — no.* If spec bytes keyed the epoch, every spec-layer commit — a typo fix, an evidence ledger landing under `specs/<id>/evidence/`, next-stage census notes — would refresh every task's budget: a global budget reset triggered by a document the machinery never reads. Worse, it reintroduces exactly the mutable-counter races `epoch-scoped-budgets` exists to delete. The correct channel already exists: **the derivation sitting is the filter that turns spec churn into precise per-task epoch bumps.** If a spec change matters to a task, the sitting amends that task's goal or `readFirst`; the task's bytes change; the epoch refreshes by the existing derivation. The spec influences epochs only through the worklist — which is the A2 authority chain restated as a data-flow fact.

*Stamp — redundant, with one caveat.* Git already content-addresses the spec: the admitted commit (`revision` in the source triple) pins the entire `specs/` tree transitively. A separate `specSha256` would be a second copy of a fact the repository proves. **The caveat worth acting on:** if ext0's receipt stamp carries only the worklist *blob* sha and not the admitted *commit*, add the commit revision to the stamp instead of inventing per-artifact shas — one field that pins the whole tree beats one field per artifact class, and it also future-proofs binding for contracts, evidence, and skills.

*What improves by staying out:* spec-layer commits (evidence, errata, ratification text for the next identity) can land mid-campaign without perturbing running budgets, and — critically for Seam 7 — without tripping ext1's poll re-admission.

---

## Seam 3 — the spec linter as a gate

**What exists today.** Campaign gates are 1–16 command gates with `preflightArgv`/`argv`/`runtimeMaxSec` (`epsilon-extension.json:8–37`), plus the `forbidPaths` kind (`spec-build.js:1414`). `test/fleet-gate.sh` runs a ladder that includes `nix flake check -L --keep-going` (`fleet-gate.sh:255`), and `fleet-gate-cheap-first` is moving metadata predicates to stage 0. The flake exposes named check attributes (`spec-build-driver-tests`, `module-layer`, `campaign-runtime`, and the dishonest `final-conformance-bar-harness` being fixed by `final-bar-executes`).

**What attaches: one script, wired through existing tiers — no new gate kind.** `test/spec-lint` (Python, the genre of `test/spec_build_driver_test.py`), checking, per `specs/<identity>/`:

1. **Grammar** — requirement headings unique, numbered, ordered, matching `### Requirement N — name`; criterion IDs `N.M` unique; EARS keyword shape per criterion (`WHEN/WHILE/WHERE/IF…THEN/bare SHALL`); `SHALL CONTINUE TO` only under `#unchanged-behavior`; a criterion naming no oracle carries `[HUMAN-ATTENDED]` explicitly (byte-oracle-or-nothing, enforced as absence-of-pretending).
2. **Cross-resolution** — every worklist `specSections` entry matching `specs/**` resolves to a real file + anchor at HEAD; every task id in `trace.json` (Seam 6) exists in the worklist file; every requirement id in `trace.json` exists in `spec.md`; every requirement is either traced to a task or listed under an unauthored stage's area — anything else is a defect.
3. **Contract lint** (where `contracts/` exists) — cross-schema references resolve; fixtures parse against their schemas. This is the D13 headline imported verbatim: the missing gate was a linter, not better prose analysis.
4. **Status coherence** — the status block names a standing consumer; a ratified spec names its worklist identity.
5. **Perturbation self-test** — the linter's own test fixtures include a deliberately broken spec that must fail. A linter never shown to bite is the `--list`-only flake attribute reborn (VD-5, F33).

**Tier placement, precisely:**

- **Flake check attribute** `checks.x86_64-linux.spec-lint` — the primary wiring. Because fleet-gate already runs `nix flake check`, this single attribute gives fleet-tier coverage on every gated head *with zero fleet-gate edits*. It is also the spec layer's **standing consumer** in the A15 sense — the thing whose existence keeps the spec from being deleted.
- **Campaign gate** — add it to the gate-set template's built subset (`nix build .#checks.x86_64-linux.spec-lint`) for campaigns whose worklists cite specs. Per-lane it re-checks an invariant lanes cannot violate (specs are deny-listed, Seam 4), so it is cheap insurance, not the main line.
- **Sitting-time** — `skills/author-spec` runs it as part of rehearse-admission, because the defects it catches (worklist↔spec drift) are *created at the sitting*, not by lanes.

Argv, concretely: `["bash","-lc","python3 test/spec-lint specs/epsilon-extension silent-factory-worklists/epsilon-extension.json"]`.

---

## Seam 4 — the deny-list, and whether machinery may read specs

**What exists today.** R3: the deny-list is `{worklist file(s), campaign gate definitions, test/fleet-gate.sh, .github/}`, enforced at the tree-delta permission gate from the deployed store path (`EPSILON-EXTENSION.md:41`); the ext0 `outcome-envelope` makes a deny-list refusal a first-class `needs-authority` outcome naming paths.

**What attaches — with one scoping carve the draft misses.** Specs join the deny-list, but **only the governing spec of the running campaign**: `specs/<identity>/**` for the armed identity. Not `specs/**` wholesale — because a campaign whose *deliverable* is a spec is already scheduled (tally's own crystallization, "one or two overnights away"): its lanes must write `specs/tally/` while being governed by, say, `specs/spec-crystallization/`. A blanket entry would make the first real spec-layer campaign refuse itself. The deny-list entry is therefore parameterized by campaign identity, which the tree-delta gate already knows.

**May machinery read specs? Yes — on one side of a bright line.** The A2 draft says "the machinery never reads or writes a spec." Half of that is right. The defensible law is:

> **No machine *decision* — admission, dispatch, budget derivation, merge, failure classification — takes spec bytes as input. Machine *rendering* — the escalation report, the release record, `campaign status` — may resolve citations for human and judge eyes.**

This line is checkable (spec bytes may flow into artifacts whose only consumers are humans and the read-only judge, never into state transitions), and it permits exactly the two things worth having: the escalation report quoting the requirement title a failing task discharges, and the judge's structured amendment proposal (`judge-verdict`, `epsilon-extension.json:178`) citing requirement ids in its goal text. Note the judge needs no machinery change at all for the second: it is a model reading the tree in a read-only sandbox; if the goal cites `spec.md#requirement-2`, the judge can already follow the pointer. The write half of A2 stays absolute.

---

## Seam 5 — the release record binds the spec

**What exists today.** `release_closing_summary` (`campaign.rs:2306`) resolves the closing summary from `refs/tally/spec-build/v1/<digest>/summary/complete`; release renders the *operator-authored project intent verbatim* into the sparse issue (`skills/campaign-operator/SKILL.md:75–77`); E1/ext2 persists `completionProofs` and the plan document in the release record.

**What attaches.** The release binds the spec by **(admitted revision, `specs/<identity>/spec.md` blob sha)** — both already determined by the epoch chain (Seam 2) — and renders a **coverage table**: for each `trace.json` row, requirement id → discharging task → witnessed criterion ids → merged sha. What "this release proves requirements 1.1–9.4 discharged" honestly takes:

- The **proof** is the receipt chain: the cited acceptance argvs passed as witnessed. That part is machine fact.
- The **mapping** — that passing `epoch-refresh` discharges 2.1 — is the sitting's authorship claim, frozen in `trace.json` at derivation time.

So the release *renders a join*, never computes discharge: committed table ⋈ durable completion facts. The zero-machinery version exists now: since release already renders operator intent verbatim, the plan document handed to release can *be* `spec.md#destination` plus the coverage table rendered by `test/spec-lint --coverage` at the interim-close sitting. The ext2 version merely moves that rendering inside the verb.

---

## Seam 6 — the derivation sitting

**What exists today.** Stage boundary = edge census (EQ §2.4) + authoring per `skills/assign-tally` + commit, push, arm; F42 says author only against the observed tree.

**What the sitting produces besides the worklist — and a draft defect this exposes.** `specs/README.md:61–64` puts the traceability table *inside* `spec.md` §7, while `README.md:78–80` freezes `spec.md` at ratification — but traceability rows are authored *per stage, after ratification*. The draft contradicts itself. Resolution: **traceability moves out of `spec.md` into `specs/<identity>/trace.json`**, append-only, linted (Seam 3 check 2). A ratified `spec.md` then has exactly three legal post-ratification changes: status-block transitions, nothing else in-file; `trace.json` appends and `evidence/` additions happen beside it.

The sitting's committed output is one commit (or PR) containing:

1. the worklist stage (new/amended tasks),
2. appended `trace.json` rows,
3. the census report as `specs/<identity>/evidence/census-<stage>.md` (record-don't-fix: deviations recorded, not patched into frozen inputs).

**What makes a sitting witnessable: nothing new.** The sitting is witnessed the way everything in this system is witnessed — its output is committed bytes, and a gate bites them: `spec-lint` passing on that commit (via the flake check on the next gated head, and immediately via rehearse-admission) *is* the witness. Optionally a `Tally-Sitting: <identity>/<stage>` commit trailer for grep-ability, but this is decoration; the linter is the witness. Post-ext1, the same commit's push is also the arming act (poll re-admission) — the sitting collapses to: sit, author, commit, push, walk away.

---

## Seam 7 — poll re-admission does not extend to specs

**What exists today / ext1.** A new committed worklist sha at the base is admitted by the poll; `armSerial` becomes a derived counter; the deliberate `run` doorbell stays (R2).

**Answer: no, and precisely because "admission" has no spec-side meaning.** Three grounds:

1. **Admission is meaningful only for artifacts the machine executes.** The poll admits a worklist because reconcile passes consume it. The machinery has no spec consumer (Seam 4's line), so a "spec admission event" would be a state transition with no state.
2. **Ratification is a judgment act at a boundary** (A19). Making a commit auto-operative would relocate the one human gate the layer is allowed to have into the poll's blind spot.
3. **The safe version already exists transitively.** A spec becomes operative at exactly one moment: when a worklist derived from it is committed to the base. The worklist commit *is* the spec's admission event — filtered through the sitting, which is where the judgment lives. Seam 2's exclusion (spec bytes out of the epoch) is what makes this safe: spec commits between sittings are inert to the machine by construction.

The `ratified` bit is consumed by humans and the linter (status coherence), never by the poll. The poll watches one file class, forever.

---

## Seam 8 — the skills split

**What exists today.** `skills/assign-tally/SKILL.md` (authoring procedure) and `skills/campaign-operator/SKILL.md` (operating procedure); the ext0 task `authoring-doctrine-skills` **already owns rewriting both** ("no new files", conflictDomains `skills/assign-tally`, `skills/campaign-operator` — `epsilon-extension.json:316,343`).

**The placement test:** *if the linter can check it → `specs/README.md` (format); if an agent performs it stepwise at a sitting → a skill; if it arbitrates between artifacts or survives format versions → the constitution.*

Applying it:

- **`skills/author-spec/SKILL.md` — new file, collision-free now** (it does not touch the two owned skills). Contents: the analyze pass and the grind, which `specs/README.md:94–104` currently holds and shouldn't — they are *procedures* (steps an agent runs), misfiled in a format document. Plus: the sitting checklist (census → author stage → append trace rows → run spec-lint → commit/push), and the ratification procedure (Seam 9).
- **`specs/README.md`** keeps only what the linter enforces or renders: artifact set, anchor grammar, EARS dialect, status-block grammar, lifecycle states, the anti-rot rule. A useful discipline: every sentence in README should be either a lint rule or a pointer to one; prose that can't be linted migrates to the skill or the constitution.
- **The two owned skills** gain their spec-layer sentences *in a later stage's task*, not by colliding with ext0: `assign-tally` gets "when a governing spec exists, goals cite `N.M`/evidence ids, `readFirst` points at requirement anchors, the sitting appends trace rows"; `campaign-operator` gets "the interim-close checklist renders the coverage table." Sequencing matters: ext0's doctrine task lands first from committed bytes (operator pre-step 2), the spec-layer amendments ride the next boundary.

---

## Seam 9 — ratification and the inbox

**What exists today / ext1.** E8: notifications carry the escalation report + the judge's rendered proposal; replies (approve/deny/steer) ride one authenticated append-only channel, ingested by the poll, durable in the ledger.

**Split the question.** *Stage approvals and amendment approvals* are worklist-side acts — they already ride the inbox by design, nothing to add. *Ratification* is different in kind: it must be attributable, durable, and bound to exact bytes.

**The minimal durable representation of "ratified" is an ordinary operator commit.** A commit on the base branch that flips the status-block line (`Status: proposed` → `Status: ratified 2026-08-NN`) is already the strongest authenticated act in the system — authority is committed bytes (A1), and the operator's push credential is the existing trust root. A signed tag or a ledger line would be a second authority mechanism tending a fact the first already carries: it fails the placement law (A8) because it deletes no operator rule and adds a tended artifact. The linter checks coherence (a `ratified` status implies the predecessor surface carries its freeze pointer, E7-style).

**Phone ratification: deliberately no.** Having the inbox authorize a machine-prepared ratification commit would put the machinery's hands on a deny-listed file — the exact move E2 deleted at the root ("the machinery never writes the authority file"). The honest boundary: steer from anywhere (rare), *ratify at a keyboard* (rarer). This asymmetry is a feature of the trust model, not a gap in the inbox.

---

## Extra seams found

**S10 — the `goal` field and the two-budget model.** Once requirements live at anchors, `goal` reverts to its D13 role: the *pre-digested*, task-scoped contract slice (what to change, what makes it true/false, in-task line coordinates) plus citations — while the requirement's full statement, its *why*, and the evidence live one pointer away. Expected, measurable effect: goal lengths drop from ~2,000 chars toward a few hundred. Do **not** lint a maximum — no speculative rules — but the shrink is the metric that tells you the layer is working.

**S11 — acceptance-criterion ids vs `N.M`.** Do not rename acceptance ids to requirement ids. `epoch-refresh` is task-local; `2.1` is spec-global; conflating the namespaces makes both fragile. The join lives in one place — `trace.json` — as `(taskId, acceptanceId) → criterionId`. This is also what keeps `deliveredBehaviors` honest: each is the compressed projection of the criteria its trace row names.

**S12 — the worklist schema is a non-seam, and that is load-bearing.** Admission refuses unknown keys (D77); adding a `spec` key to the campaign object would break admission on the *deployed* machinery (frozen-flow rule) and invert the direction `specs/README.md:62–64` correctly fixes: **the spec points at tasks; the worklist schema does not change.** The join is: directory name = campaign identity, plus `trace.json` naming task ids. Every integration below respects this.

**S13 — the documented genre.** `doc/src/flows/campaigns.md:1341` already teaches `specs/001-crm/spec.md#customer-model`. When format v1 lands, one small task re-renders the doc example to the number-derived anchor grammar so the shipped documentation and the linter agree.

---

## The md+json split, derived

Rule applied: every JSON element must name the machine join that needs it.

| Artifact | Form | Machine join that requires it |
|---|---|---|
| `specs/<id>/spec.md` (status, destination, rulings, requirements, unchanged-behavior, stages) | **md with lintable grammar** | `spec-lint` parses headings/criteria by deterministic grammar; a parallel `index.json` would be a second copy that drifts — the id index is *derivable*, so it must not be stored |
| `specs/<id>/trace.json` | **JSON** | three-way id join (task ↔ criterion ↔ acceptance ↔ evidence) consumed by `spec-lint` cross-resolution *and* the release coverage rendering; md tables are miserable to append and parse mechanically at every sitting and every release |
| `specs/<id>/contracts/*` | **JSON/schema + fixtures** | the contract linter (cross-schema resolvability, fixture producibility) — the D13 gate |
| `specs/<id>/evidence/*.md` | **md** | no machine join; cited by id from goals and trace rows; committing them is what makes citations resolvable (A9) |
| status block, EARS clauses, `[HUMAN-ATTENDED]` marks | **md grammar, no json twin** | linted in place — the single-file-with-grammar case wins wherever the data is authored and read as prose and only *checked* by machine |

`trace.json` sketch:

```json
{
  "schemaVersion": 1,
  "spec": "specs/epsilon-extension/spec.md",
  "rows": [
    {
      "task": "epoch-scoped-budgets",
      "criteria": ["2.1", "2.3"],
      "acceptance": { "2.1": ["epoch-refresh"], "2.3": ["resume-deleted"] },
      "evidence": ["CA-2", "PA-05"]
    }
  ]
}
```

Where the md+json *pair* is wrong: requirements. Where the single md file is wrong: traceability. The draft currently has both inside `spec.md`; splitting exactly there resolves the freeze contradiction (Seam 6).

---

## Constitution critique (blunt)

**Load-bearing for the seams:** A1, A2 (with two amendments below), A8 (the placement law is the judging criterion for this whole exercise), A9 (the linter's existence-check is its mechanization), A10, A12, A13, A15 (the linter is the spec layer's standing consumer — without A15 the layer rots), A19 (grounds Seam 7's refusal).

**Needs amendment:** A2 overstates the read half — replace "never reads or writes" with the deciding/rendering line (Seam 4), and scope the deny-list entry to the *governing* spec (a campaign may write specs it is not governed by).

**Decoration or misfiled:**
- **A5, A6, A7** are true machine facts already enforced in code and tests. They belong in *tally's own crystallized spec* as `SHALL CONTINUE TO` clauses, not in the constitution of authoring. Keeping them here is harmless but they carry no spec-layer load.
- **A16, A17** are procedures dressed as law. The grind is a checklist an agent executes; it belongs in `skills/author-spec`, with the constitution retaining at most one sentence each ("contracts of consequence get dual derivation"; "frozen inputs are recorded, never edited").
- **A18** is operator doctrine that the ext0 `authoring-doctrine-skills` task is *already* landing into `campaign-operator` — the constitution restating a skill's rule creates the contradiction-maintenance burden A8 forbids.
- **A20, A21** describe machine behavior that `judge-verdict` and `epoch-scoped-budgets` are landing as code with tests. Once the code enforces them, the articles are commentary. By the constitution's own A15 logic, each article should name its concrete consumer or shrink.

**Missing (the real gaps):**
1. **The freeze/append article** — what may legally change after ratification: status-block transitions in `spec.md`; appends to `trace.json`; additions under `evidence/`; nothing else. This is the spec-layer analog of A21 (terminal states, recovery only by new input) and nothing in the draft states it. It is the most load-bearing absence.
2. **The traceability-direction article** — "the spec points at tasks; the worklist schema is closed" is currently a README aside (`specs/README.md:62–64`), but it is an *authority* law and belongs beside A2.
3. **The deciding/rendering line** (Seam 4) as an article, since it bounds every future integration.

The "citation is the argument" device is excellent — keep it, and require it: an article that cannot cite a paid-for finding is a candidate for deletion.

---

## The zero-machinery-change subset (works today)

1. **Commit `specs/epsilon-extension/`** — `spec.md` (anchor grammar), `trace.json`, `evidence/` holding the five Aug-14 ledgers currently in a session scratchpad. This is E6 territory and it is what makes every citation in the live worklist's goals resolvable (A9).
2. **Write `test/spec-lint`** + `checks.x86_64-linux.spec-lint` in `flake.nix`. Because `test/fleet-gate.sh:255` already runs `nix flake check`, the spec gets fleet-tier standing coverage from one flake attribute and zero fleet-gate edits. Include the perturbation fixture from day one.
3. **Author the next stage's worklist with real pointers** — `specSections: ["specs/epsilon-extension/spec.md#requirement-N"]`. The schema accepts any string; the brief already instructs the worker to read them; lane-worktree co-location supplies D68.
4. **Goals cite, trace rows append at the sitting** — pure authoring discipline plus the linter.
5. **Release intent = `#destination` + rendered coverage table** — release already renders operator-authored intent verbatim, so the coverage story ships without touching the verb.
6. **Governing spec appears in no task's `conflictDomains`** — until R3 lands, ownership certification alone keeps lanes out of it.

## Smallest ordered ext-era deepenings, priced against A8

1. **`specs/<armed-identity>/**` joins the R3 deny-list** (ext1, configuration of a mechanism already landing). *Deletes:* the operator's post-run glance "did any lane touch the spec," and gives `needs-authority` envelopes a path to name.
2. **Admission resolves `specs/**` pointers** — `arm`/poll refuses a worklist whose `specSections` match `specs/**` but do not resolve at the admitted revision. *Deletes:* the doctrine sentence at `skills/assign-tally/SKILL.md:25` and the sitting's manual pointer check; closes the 48-phantom-pointer class mechanically.
3. **The escalation report resolves citations** — when a failing task's goal cites spec anchors, the report quotes the requirement title and criterion text (rendering side of the Seam 4 line). *Deletes:* the operator rule "open the spec to see what this task was for" during every escalation — a transcription act in A19's sense.
4. **Release renders coverage from durable facts** (rides E1/ext2 `completionProofs`) — the verb joins `trace.json` with completion proofs itself. *Deletes:* the operator's hand-rendered coverage table from step 5 above; Destination is written once, at ratification, never again at release.

**Explicitly rejected, to show the law biting:** a `specSha256` receipt stamp (redundant — the admitted commit pins the tree; Seam 2); spec poll-admission (Seam 7); a `Tally-Discharges:` commit-trailer scheme (deletes no operator rule today — pure speculative schema); a `spec` key in the worklist campaign object (breaks closed-schema admission on deployed machinery and inverts the authority direction).

The through-line of the whole map: the spec layer needs almost nothing from the machinery because the machinery already ends in exactly the right places — a free-string pointer field the worker is told to read, a gate ladder that executes any argv, a release that renders operator bytes verbatim, and an epoch key that pins a commit. The layer's integrity comes from one new artifact class, one linter with standing consumers, and the discipline of leaving every join as committed bytes the existing seams already carry.
