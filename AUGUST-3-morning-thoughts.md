# August 3 morning thoughts — baggage, vestiges, and the shape of v0.0.1

Written at the close of wave 3 (board at zero, main `520681c`). These are the
observations to watch before the planned baggage-reduction run and the v0.0.1
re-release. The operator's instinct prompted the measurements; the numbers and
the fossil record are below so the future run argues from evidence.

## 1. The comment accretion is real, recent, and structural

Measured on `crates/**/*.rs` + `**/*.nix` (legacy-docs excluded):

| Revision | Code lines | Comment lines | Ratio |
|---|---|---|---|
| First commit | 30,366 | 105 | 0.3% |
| `65a5bbb` (wave-3 baseline, Aug 1) | 105,077 | 1,088 | 1.0% |
| `origin/main` (Aug 3) | 113,655 | 3,544 | 3.0% |

Of added Rust/nix lines per era: **0%** were comments before PR #200, **2%** in
#200–300, **18%** in #300–370. Comment lines tripled during wave 3 alone.

The cause is the eval→repair loop culture, not any one author: every repair
narrates its constraint into the code, and 22 places in `crates/` now cite
issue numbers or "the evaluator" directly. That is changelog-in-code. Git
history already records provenance; code that re-records it is the bloat.

Nuance to keep: 3% overall density is *low* for a Rust codebase (mature
projects run 10–25%). The problem is style, not volume — long narrative
"how this came to be" prose (292 comments over 60 characters) versus terse
statements of invariants the code cannot show. The sweep should distinguish:

- **Keep**: constraint comments ("the ledger is deliberately not hash-chained
  because…" — states an invariant a reader cannot infer).
- **Delete**: provenance/narration ("the evaluator found…", "#NNN moved this",
  "this was changed because the previous shape…").

After the v0.0.1 history reset, every `#NNN` referent becomes a dangling
pointer. The provenance comments do not merely *deserve* deletion — they stop
resolving the moment the lineage is cut.

## 2. Vestigial structures: confirmed non-zero, clustered on two seams

The wave's own evals are the fossil record. The vestiges are not pervasive rot;
they cluster exactly where concepts were added mid-development.

### Seam A — flows layered on the frozen enqueue kernel

- **W-316** (ledger, wave 3): a task admitted under a `flowRunId` whose durable
  row has not yet carried the run's orchestration capsule is invisible to
  `query log/jobs --flow-run`. Reproduced in-tree; made legible
  (`flowRunTasks`); NOT fixed, because the fix sits on the kernel §6 froze.
- The exit-20 contract split (#251 eval): the same wire code carries different
  `details` depending on whether it is raised at startup or mid-run — two eras
  of error plumbing coexisting.

The kernel freeze was wave discipline, not eternal law. A release boundary is
the one place the freeze can be re-ruled.

### Seam B — sub-issues (forge-native) versus the artifact-worklist campaign

- **W-321**: the full-form `Closes owner/spec#N` grammar, the
  foreign-repository completion refusal, and issue-coordinate checkbox sync
  all exist in the tree and are all **unreachable** — they require
  `task.brief`, set only by the forge-native read path, and the split seam
  refuses forge-native. Shipped, tested-looking, dead. Docs are honest about
  it since the #321 repair; the code still awaits a design that does not
  exist. For v0.0.1: delete the staged grammar rather than carry it, and
  reintroduce it with the design that actually reconciles the two.

### Deliberate compat shims (vestigial by design, mostly deletable at v0.0.1)

- `retired_duplicate_acknowledged` serde field (accepted on read, never
  written) — #340 repair.
- Legacy `refs/tags/` checkpoint-receipt namespace fallback — #334 repair.
- Read-side absorption of pre-repair UUID renderings in `flow-lineage.jsonl` —
  #251 repair.
- The `mention = "@tally build"` default, kept solely for back-compat with
  deployments of the pre-release lineage (now pinned by `campaign-render`).

Every one of these protects deployments of the current lineage. If v0.0.1 is a
clean break with a single operator, they protect nothing and can go. Precedent
that organ removal works here: taskdb (#304) was deleted whole and #326 swept
its residue cleanly.

## 3. Model-tier observation for the next wave

Where opus earned its keep: the **evals** (small diffs hid big reasoning — the
R-A "residue" eval found a trust-boundary HIGH; the #251 eval found two HIGHs
the gate structurally could not see) and mechanism implementation. Where a
smaller model would have sufficed: docs batches, doc-residue repairs,
CHANGELOG/wording fixes, LOW-only repair passes — roughly a third of the
late-wave sessions. Tier accordingly: opus for mechanism lanes and all evals;
sonnet-grade for docs, residue, and LOW-only repairs.

## 4. Rebuild, heavy cleanup, or refine? — the recommendation

**Not a rebuild.** The evals argue against it: forty-odd findings across wave
3 and every one was an edge defect — trust boundaries, torn tails, ordering,
claim drift — never architectural rot. The mechanism (admission door, witness
ledger, pools, gates, flows) survived adversarial review intact. A rewrite
discards exactly the invisible correctness that does not look load-bearing:
the FETCH_HEAD-not-rev-parse fix, the torn-tail truncation, the canonical-UUID
absorption, the contention doctrine. Second systems die on what the first one
silently got right.

**The history reset and the code are separable.** The v0.0.1 goal — clear of
issue numbering and long PR lineage — is achieved by resetting *history*
(fresh repo or squashed root), not by rewriting *code*. Conflating the two
buys risk with no return.

**So: heavy cleanup of the tree, carried into a fresh history.** One planned
run, three lanes, most of it sonnet-grade:

1. **Comment sweep** — the keep/delete rule from §1, mechanical.
2. **Shim + vestige excision** — delete the §2 compat shims and the W-321
   unreachable grammar; each deletion is a small PR with the fleet gate as the
   safety net.
3. **Seam ruling** — the one genuinely architectural decision: whether v0.0.1
   unfreezes the enqueue kernel to fix W-316 at the root, or carries the
   waiver into the release. That is an operator ruling, not a lane's choice.

Then cut the cleaned tree over as v0.0.1's root commit. Refinement continues
after the release on the lean base — refinement and cleanup are not rivals;
the cleanup is what makes further refinement cheap.
