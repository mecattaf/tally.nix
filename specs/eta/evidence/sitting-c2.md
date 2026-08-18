# eta seam sitting C2 — chapter 4 authored (2026-08-18)

Standing consumers: the chapter-4 lanes' readFirst; the C3 seam sitting.
Authored by the supervising orchestrator under the ETA.md charter; the
charter's sequence is the authority — no operator decision points remain
at this seam (operator statement, 2026-08-18: "you are supervising
chapter 4 run end to end").

## Seam state

Chapters 1–3 published and gate-proven (fleet gate PASS b54ea267, final
bar 24/24); pin-bump 2 flashed (daemon at the as412bk3 build, smoke
PASS). The deployed contract is chapter 3's: main advances only by
machine fast-forward of a gate-proven head, a worklist push is the
arming act, lanes mechanically cannot write specs/<identity>/**, gate
budgets derive from receipts when unstated, the lease and the inbox are
live. Chapter 4 is these verbs' first shakedown. The campaign registry
is empty (eta reached complete and was disarmed at the C2 close), so
this chapter re-enters by the arm verb once, after which push-to-re-admit
carries every amendment.

## Rulings drained at this seam

- **Adapter for chapter 4 (operator ruling, 2026-08-18):** claude-code,
  host-default model — the rail that closed chapters 2 and 3. The
  metered window is exhausted until 2026-08-22 13:34 UTC (run-log); at
  reset, switching chapter 5 to the plan rail is one amendment at
  whatever task boundary is then open.
- **Gate-budget retirement, executed as scheduled:** the
  gate-budgets-from-receipts goal fixed its own sequel — "the eta
  worklist's own gate numbers stay as they are this chapter — retiring
  them is one amendment AFTER this task deploys, recorded then." It
  deployed at pin-bump 2; this sitting's amendment deletes the four
  convicted runtimeMaxSec guesses (900/3600/1800/1800, V-6's class)
  from the template gates. Budgets now derive from the campaign's own
  receipts; the derivation is read at admission rehearsal, and if a
  derived budget undercuts an observed gate duration the number comes
  back by amendment with a ruling, not by silence. The checkpoint
  node's explicit 10800 stays: chapter-gate-c3 is a fresh gate id with
  no receipt history, and the declared number is the budget (min
  semantics with nothing — the zero-required-numbers doctrine).
- **The steward shim is role-aware (host-side repair, this sitting):**
  every auto-diagnosis dispatch since the C2 flash died
  result-schema-mismatch because the narrator shim answered the
  commit-narration schema unconditionally. Mechanism, established from
  the tree: the publish node hands its narration request on stdin (the
  narrator is its direct subprocess), but a diagnosis dispatch is a
  daemon job unit — job units have no stdin; the brief arrives as a
  file named by TALLY_BRIEF (executor/launch.rs near :286) — so the
  stdin-only shim answered every diagnosis from an empty read. The
  shim (dotfiles home/tally.nix, commit de49728b) now reads TALLY_BRIEF
  when present and branches on the brief's role field (only the
  diagnosis brief carries role:"diagnosis"); the diagnosis branch
  answers {verdict, diagnosis[, proposal]} with jq shape enforcement —
  an invalid answer exits nonzero and fails the node legibly. Deployed
  and smoke-proven against a synthetic brief before this chapter armed.
  Auto-diagnosis is live for the first time; the supervisor remains the
  fallback diagnosis layer per E5 rule 7.
- **The baseline-parity law, restated as this chapter's authority**
  (recorded 2026-08-15, AUG15-SESSION-FINDINGS.md §2.6; the substrate
  spec that was to carry it as a claim was never authored, so this
  sitting record is its specs/** home until the P3 cleanup): whatever
  an agent can do bare in a terminal — write temp files, use /dev/shm,
  compile at full parallelism, see its own error output — it must be
  able to do inside a lane; every deliberate gap is a documented
  containment ruling with a named justification, or it is a defect.
  D3 converts the law from prose to a witnessed property.

## Lane discipline (standing, carried from sitting-c1)

Fixtures live crate-local and resolve via CARGO_MANIFEST_DIR (the nix
sandbox builds from a filtered source); every command runs foreground;
verification is cargo-only in the lane except where a task's acceptance
argvs name specific nix build --no-link attributes (the merge gates own
the full flake proof); the finish is one commit plus a clean worktree,
verified with git log -1 && git status --porcelain.

## Chapter 4 authoring sources

ETA.md §4 chapter 4 (task table D1–D4 and dependencies); the observed
tree — the self-contained package install (flake.nix near :164-166),
exe-relative flow-asset resolution (crates/tally/src/cli/campaign.rs
near :7228/:7233), the fleet-free stockHome activation check (flake.nix
near :1127), the contract's leniency floor (campaign_contract.rs near
:698-705), the smoke genre (crates/tally/src/cli/adapter.rs);
AUG15-SESSION-FINDINGS.md §2.6 for D3's law; the eta run-log for the
misattribution incidents D3 exists to make impossible. Worklist task
ids: product-split (D1), worklist-scaffold (D2), baseline-parity-probe
(D3), product-docs (D4), chapter-gate-c3.

## Deferred, standing

deepseek-v4-pro-0813 is still undeclared in the host pi catalog — added
only when a pro-tier lane is first routed (not this chapter). The #440
launch-cwd flake and the zeta-spec consolidation stay queued under P3.
