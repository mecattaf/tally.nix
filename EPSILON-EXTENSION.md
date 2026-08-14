# EPSILON-EXTENSION — the ratified program

v2, 2026-08-14, ratified against the printed specification
(`tally-specification.pdf`, approved by the operator). Supersedes v1 of this
file in place. `SILENT-FACTORY-PLAN.md` is frozen at epsilon's close (E7);
this is the only planning surface for the extension, and it stays short: the
deliverable is the worklists and the code they land, not this file.

Evidence base: the five analyses of 2026-08-14 — process archaeology
(PA-01…47), verified-defect ledger (VD-1…31), ceremony audit (CA-1…14),
final-shape + history-replay (the two-Fable design pass), intern
lineage/audit, and the equipment ledger (EQ) — all grounded in F1–F44.

## Destination

A campaign of epsilon's shape runs with the human acts limited to: authoring
worklists, steering (rarely, from anywhere), approving rendered proposals,
prompted deploys, and invoking release. Measurable close conditions:
- a full stage completes with **zero transcription acts** (no operator act
  whose text the system had already printed);
- `tally campaign release` proves its completions through the **exact**
  oracle;
- the armed worklist carries **zero required numbers** (post-ext1);
- the judge's tier has been validated once by the Aug-1 corpus-replay
  disagreement measurement.

## The ratified decisions

| id | decision | ruling |
|---|---|---|
| E1 | one completion contract: the writer's tuple; execution policy (gates/agent/steward/mergeMethod) leaves the identity | **yes — as early as dependency order allows** (every interim release is bridge-grade) |
| E2 | capped auto-grant | **no — deleted at the root instead**: retrospective certification + the authority deny-list; the machinery never writes the authority file |
| E3 | escalation latch → notification + budget | **yes, via epoch keying + diagnosis-gated retry** |
| E4 | registration → lease; release reads durable facts | **yes, core** (one model, not deferrable) |
| E5 | steward narration | **slot deleted; subject adoption replaces it.** Evidence corrected for the record: 0-for-35 measured the harness (envelope bug + undisclosed rules), not the model |
| E6 | commit the lineage record (the untracked run/learnings docs, this file, the worklist) | **yes** — operator pre-step |
| E7 | freeze SILENT-FACTORY-PLAN.md | **yes** |
| E8 | off-host steer/approvals (D12) | **yes, core** — the inbox is the operator surface |
| R1 | `main` advance | **auto fast-forward of the re-gated proven head** (publish carried zero judgment on the record) |
| R2 | campaign start | **the deliberate `run` doorbell stays**; only re-admission is automatic |
| R3 | authority deny-list | **{the worklist file(s), the campaign gate definitions, `test/fleet-gate.sh`, `.github/`}** — enforced at the tree-delta gate from the deployed store path |
| R4 | `forge:"local"` remote semantics (VD-18) | **open — the one remaining one-word ruling**; until then the push-to-remote authority fetch stands |

The intern ruling, restated once: the resident frontier supervisor is
deleted; **the judge is promoted** — one standing model slot, adversarial by
position (read-only, artifact-fed, schema-forced, never the author of what it
judges), sonnet-grade by the standing Aug-1 ruling, downgradeable only by
corpus-replay measurement.

## Structure

Three stages, one new campaign identity
(`silent-factory-worklists/epsilon-extension.json`). Per F42, only the
current stage is fully authored; each next stage is authored against the
observed tree at the boundary, with the edge census (EQ §2.4) run at the same
sitting. Machinery changes land in-tree and take effect at the boundary
deploy (the frozen-flow rule: the deployed store path grades, permanently).

**Operator pre-steps before ext0 arms:**
1. **Deploy-3** the fleet to `e921cccc` (nothing armed; puts the Rust driver
   and the whole ε2 surface into the grading path).
2. Resolve the stale Aug-12 `skills/*.md` working-tree edits (commit or
   stash) so the doctrine lane starts from committed bytes.
3. **E6**: commit the record — the untracked `AUG*.md`/`aug*` documents,
   this file, and the worklist.
4. R4, one word, whenever — it gates nothing in ext0.

### ext0 — "honest surfaces and the substrate" (authored in full)

Ten tasks; graded by the deployed (pre-extension) machinery, so today's
verbs still apply while ext0 builds their replacements. Its own gate set
already carries the new template (clippy + a **built** check subset).

| task | closes | one line |
|---|---|---|
| `receipt-authority-stamp` | CA-3 | every receipt gains `armSerial`, `worklistSha256`, `writtenAt` (+ actor coverage) — the epoch key; legacy receipts still reconcile |
| `epoch-scoped-budgets` | CA-2, PA-05, the 0-for-9 | attempt counting becomes derivation: a receipt counts only when its epoch matches the task's current input (task bytes + gates + steering seq); steer bumps the epoch; un-authored lifetime backstop; **`resume` and the pardon concept delete** |
| `summary-ref-stage-digest` | CA-7, VD-4, F38 | the admitted graph digest joins the summary ref name; the archive ritual's cause is deleted; no archive verb is built |
| `outcome-envelope` | VD-1, F35 | structured final-message outcomes `needs-authority` (deny-list refusals, spends nothing, names paths) and `impossible` (a claim, not a verdict); crash stays signal-level; a refusal and a crash are never again the same signal |
| `judge-verdict` | intern audit §2/§4 | the diagnosis result gains the typed verdict `retry \| blocked \| transient` that gates attempt 2 (checkpoints never retry) and a structured amendment proposal the escalation report renders as a ready diff; the slot rebinds from the worker adapter to the steward catalog role (Sonnet first); full contract disclosed to the model |
| `subject-adoption-narrator-retire` | PA-25/26, F32 | the squash layer adopts the lane tip's own subject under the grammar with deterministic repairs; the narration slot leaves the merge path; the steward seam survives for the judge |
| `final-bar-executes` | VD-5, F33 | the bar's check attribute executes cases or dies; fleet-gate gains a final-bar step |
| `fleet-gate-cheap-first` | PA-09, PA-36 | metadata predicates to stage 0; the local-audit arm ends throwaway PRs |
| `authoring-doctrine-skills` | EQ §4, VD-13, F39/42/43 | the equipment becomes committed bytes: the 11-item equipment list, the edge census, the gate-set template, the interim close checklist, the stale-pin rule |
| `chapter-gate` | — | fleet-gate + final bar on the integration head |

### ext1 — "the last mile" (authored at the ext0 boundary, after deploy)

- **Retrospective certification + the R3 deny-list** replace prospective
  refusal: lanes commit what the task requires; ownership certifies the
  committed set; refusal survives only for deny-list paths and live-sibling
  collisions (machinery-priced). The grant concept ends.
- **Lease registration + release-from-durable-state** (E4): acquire on
  `run`, lapse at complete+quiescent, receipts/refs outlive it, nothing
  durable named after a registration id; `release` requires no armed
  registration; `disarm` deletes, `stop` stays.
- **Poll re-admission**: a new committed worklist sha at the base is admitted
  by the poll; `armSerial` becomes a derived counter; hand re-arm ends.
- **Publish as a machine stage**: content-disjoint rebase, **re-gate of the
  rebased head**, auto fast-forward of `main` (R1), proven ≡ published
  recorded in the receipt. The `shas.txt`/`bodies.txt` harness dies.
- **The inbox (E8)**: notifications carry the escalation report + the
  judge's rendered proposal; replies (approve/deny/steer) ride one
  authenticated append-only channel, ingested by the poll, durable in the
  ledger; steer reaches a phone.
- **Authored caps off the schema**: `maxTasks`/`maxParallel`/per-gate
  `runtimeMaxSec` optional then gone; concurrency derived (host agent-slot
  pool ∩ disjoint frontier); output-liveness watchdog; generous un-authored
  backstops. (Caps replay: no authored cap ever fired correctly.)

### ext2 — "honest proofs" (authored at the ext1 boundary)

- **Completion-identity unification (E1)** — pulled forward into ext1 if the
  tree allows: writer's tuple canonical, policy out of the identity, exact
  oracle revives, bridge demotes to legacy, `completionProofs` + the plan
  document persist in the release record.
- **Estate population replay**: `rebuild` replays a sampled population of
  real estate rows, and a gate runs it (the only fleet-down class gets
  standing coverage).
- **Probe honesty**: `gh` scope preflight before any repository exists;
  `releaseComplete` is the verdict, teardown a separate field.
- **Test isolation guard**: no spawned `tally` inherits the real
  `HOME`/`XDG_CONFIG_HOME` (the F25 class).
- **The judge tier evaluation**: replay the journaled diagnosis corpus
  against a smaller candidate; measure disagreement; downgrade only on the
  numbers (the Aug-1 procedure, finally runnable).

## Out of scope for the campaign (host items, unchanged)

Dotfiles PR #225; the narrator shim retires with E5 (no hardening needed);
`gh auth refresh -h github.com -s delete_repo` + delete
`mecattaf/tally-probe-20260814-6bf9bac2`; deploy-3 (named above).
