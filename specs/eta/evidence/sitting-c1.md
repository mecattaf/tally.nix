# eta seam sitting C1 — chapter 3 authored (2026-08-17)

Standing consumers: the chapter-3 lanes' readFirst; the C2 seam sitting.
Authored by the supervising orchestrator under the ETA.md charter; the
charter's sequence is the authority — no operator decision points remain
at this seam (operator statement, 2026-08-17).

## Seam state

Chapters 1+2 published and gate-proven (fleet gate PASS 5fe28fe9
lineage); pin-bump 1 flashed (daemon at the 4212004b build, job-limit
flags gone from the rendered unit, 24 GiB drop-in deleted, smoke PASS);
specs/eta/spec.md ratified 2026-08-17, linter-green; specs/zeta/spec.md
stays proposed — the linter's identity law (L2: the directory stem must
govern its own worklist stem) forbids ratifying a spec whose worklist
never existed; zeta's consolidation is chapter-5 (P3) cleanup work.

## Rulings drained at this seam

- **R4 (ETA.md §8.2), ruled from the record:** `forge:"local"` promises
  REMOTE-AUTHORITY — the identity's authority surface is the local
  checkout's own configured git remote, integration branch included;
  arming and re-admission read from that remote; no forge API, no second
  push target. Source: final-shape.md open item 4 (VD-18), which names
  this resolution as the one the single-line integration model wants.
- **Auto-fast-forward, ruled from the record:** main advances by machine
  fast-forward of a gate-proven head with no per-stage human click —
  final-shape.md open item 1's own recommendation ("the record shows
  publish carried zero judgment").
- **Protected set:** {worklist, gate definitions, specs/<armed-identity>/**}
  — the third member lands mechanically with X4.
- **Adapter for chapter 3:** claude-code (host-default model) continues —
  the charter's §3 fallback clause applies while the metered window is
  exhausted (resets 08-22 13:34 UTC); switching back to the plan rail at
  reset is one amendment, taken at whatever task boundary is then open.

## Lane discipline (carried from chapters 1–2, now standing)

Fixtures live crate-local and resolve via CARGO_MANIFEST_DIR (the nix
sandbox builds from a filtered source); every command runs foreground;
verification is cargo-only in the lane (the merge gates own the flake
proof); the finish is one commit plus a clean worktree, verified with
git log -1 && git status --porcelain.

## Chapter 3 authoring sources

specs/epsilon-extension/evidence/final-shape.md — §4 item 6 (the
single-line integration model, X1/X2), the lifecycle sketch (X6), the
open-items list (rulings above); ETA.md §4 chapter 3 (task table, the
maxParallel permanence amendment); the eta run log (the re-arm orphan
incident X2 must make structurally impossible).
