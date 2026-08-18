# eta seam sitting C3 — chapter 5 authored (2026-08-18)

Standing consumers: the chapter-5 lanes' readFirst; the P4 close sitting;
the v0.0.1 blessing act. Authored by the supervising orchestrator under
the ETA.md charter after the chapter-4 close (run-log, this directory).

## Seam state

Chapter 4 closed the same day it was armed: four lanes merged (one
auto-diagnosed retry), C1/C2/C3 re-witnessed green over e091b46a, the
first machine fast-forward publish landed main on the proven sha,
campaign complete with the lease lapsed, pin-bump 3 deployed
(ab10ac91; `tally --version` now names it), first live parity probe
returned PARITY with an empty containment table. The deployed contract
carried live fire on X1, X2, X5, X6, and the role-aware diagnosis
steward. Remaining charter scope: chapter 5 (P1–P4), then eta exits.

## Rulings drained at this seam

- **Adapter for chapter 5:** claude-code host-default continues — the
  metered window resets 2026-08-22 13:34 UTC (charter §3 fallback
  clause). If lanes remain when it resets, switching is one amendment
  at an open task boundary; chapter 5 is not paused to wait for it.
- **The §8.1 seam ruling is SURFACED to the operator at this sitting**
  (the one decision the charter reserves; due before P3 concludes):
  whether v0.0.1 unfreezes the enqueue kernel to fix W-316 at the root
  — a task admitted under a flowRunId whose durable row does not yet
  carry the run's orchestration capsule is invisible to
  `query log/jobs --flow-run` — or carries the waiver into the
  release (specs/eta/evidence/day-docs/AUGUST-3-morning-thoughts.md
  §2/§4). The vestige-excision lane below is authored
  ruling-independent: W-316's root fix joins by amendment iff the
  ruling is "unfreeze"; the waiver path needs no task.
- **Day-doc migration executed (E4's grandfather clause ends):**
  twenty-one root record files moved verbatim to
  specs/eta/evidence/day-docs/ in this sitting commit. Pointer policy:
  standing consumers only (ETA.md §0, doc/src/flows/campaigns.md) were
  re-pointed; historical bytes — worklists, zeta-learnings, day-docs'
  own cross-references, code citation strings that already dual-cite a
  specs/** home — keep their old spellings, which git history and this
  map resolve. Root keeps README, CHANGELOG, CONTRIBUTING, RELEASING,
  SECURITY, and the three program charters (ETA.md, ZETA.md,
  EPSILON-EXTENSION.md), which the v0.0.1 cut will retire wholesale.
- **zeta-spec consolidation: disposition, no change.** specs/zeta stays
  as-is, Status: proposed, permanently. Every candidate "fix" breaks a
  live consumer or rewrites record bytes: spec-lint reads
  specs/zeta/contracts/trace.schema.json at runtime as the layer's
  trace schema, and the armed eta worklist's chapter-2 readFirst
  anchors resolve against specs/zeta/spec.md — while the spec's claims
  are enforced by checks.x86_64-linux.spec-lint on every gated head,
  which is the only bite ratification would add. The identity-law wart
  (Governs names a worklist that never existed) is bookkeeping, and it
  dissolves at the v0.0.1 history cut. Anti-ceremony rule applied: a
  process artifact exists only to gate a named capability; this one
  gates nothing.
- **V-17 (the model-name citation in epsilon-extension.json):** its own
  trigger — "regenerated at the first sitting that touches that file" —
  has not fired; no chapter-5 work touches that historical worklist.
  Deferred to the v0.0.1 cut, where forge-era artifacts die anyway.
- **Steward findings from the C1 re-witness become a lane** (run-log,
  this directory): the 120s steward node budget, the pass-killing
  projection timeout, and the red-transcript clobber each cite an
  observed defect from today's record — they gate diagnosis legibility
  for unsupervised operation, so they are feature work, not apparatus.

## Lane discipline (standing, carried from sitting-c1/c2)

Fixtures crate-local via CARGO_MANIFEST_DIR; every command foreground;
cargo-only lane verification except acceptance-named nix build
attributes; one commit plus a clean worktree, `git log -1 &&
git status --porcelain` last. New since the flaky-flash lesson: any
lane command that pipes a build MUST set -o pipefail first.

## Chapter 5 authoring sources

ETA.md §4 chapter 5; EPSILON-EXTENSION.md §ext2 ("honest proofs": the
completion-identity unification E1, probe honesty, test-isolation
guard, the judge-tier evaluation) and final-shape.md near :509 (ext2
stays as planned); specs/eta/evidence/day-docs/AUGUST-3-morning-thoughts.md
§1 (comment keep/delete rule), §2 (vestige and shim inventory), §4
(the cleanup shape and the seam ruling);
specs/eta/evidence/day-docs/AUGUST-01-DESIGN.md (the Aug-1 judge-tier
procedure); the eta run-log (steward-timeout, transcript-clobber, and
flaky-test findings; the real spend numbers P2 replays). Worklist task
ids: completion-unification (P1), judge-replay-harness (P2),
comment-sweep, vestige-excision, test-isolation-guard,
steward-timeout-legibility (P3), chapter-gate-c4 (P4).
