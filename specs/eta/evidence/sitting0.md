# eta sitting 0 — record (2026-08-15)

Authored by the supervising orchestrator per ETA.md §3; ratified by the
operator's sitting commit (this commit). Standing consumers: the armed eta
campaign; the C1 seam sitting; the per-lane spend ledger (§4 below).

## 1. What this sitting commits

- `silent-factory-worklists/eta.json` — chapters 1–2 fully goal-authored:
  chapter 1 (six substrate-repair tasks) from the vestige ledger's A10
  package + addendum (`specs/substrate/evidence/vestige-sweep.md` part 3,
  V-15/V-16 absorbed per the delta summary); chapter 2 (five lint tasks)
  verbatim from ZETA.md's task specs, with exactly one true-up: the checks
  set anchor moved from flake.nix:3455 to flake.nix:3458 on the post-ext0
  tree. Header per ETA.md §3: maxTasks 40, maxParallel 1, steward
  "narrator", agent pi with the three policy keys explicitly nulled (E8),
  the four template gates verbatim from epsilon-extension.json:8–37.
  Checkpoint `chapter-gate-c1` closes over all eleven tasks; chapters 3–5
  enter later by worklist amendment at seams (E1).
- `specs/eta/spec.md` (Status: proposed) + `specs/eta/trace.json` (twelve
  sitting rows, eta/s1). First-contact posture: the spec is linted for the
  first time when spec-lint-flake-check lands mid-campaign; any defect is
  an operator regeneration, never a lane edit.

## 2. Drained decisions (ETA.md §8 item 3)

- **DECISION-1 (steward) → `"narrator"`, settled with a structural reason.**
  The worklist steward field is a host-adapter *name* resolved at admission
  (`resolve_worklist_steward`, crates/tally/src/cli/campaign.rs:4963), and
  the diagnosis/judge slots bind the steward catalog role, not the lane
  agent (`applyDiagnosisRole`, examples/flows/spec-build.js:2498–2533). So
  `"narrator"` (a claude-family shim declared in dotfiles/home/tally.nix)
  is not just the ext0-proven value — it is what keeps every evaluating
  slot off the metered plan, satisfying dispatch rule 7 by construction.
- **UNKNOWN-1 → drained: no existing cargo test covers the read-first brief
  rendering.** Verified 2026-08-15 by grep over crates/tally/src/cli/
  campaign.rs (the renderer at :7091 has no test asserting the rendered
  heading). Zeta claim 3.2 keeps its HUMAN-ATTENDED binding; recorded as
  eta spec ruling E6.

## 3. Host-catalog state (model roster, ETA.md §6)

Which model answers is a host fact, never worklist bytes. Current state,
verified this sitting:

- pi defaults: provider `qwen-token-plan`, model `qwen3.8-max`, thinking
  `medium` (~/.pi/agent/settings.json) — exactly the S1 calibration
  configuration. The `sk-sp-` plan-rail key is wired via agenix.
- ~/.pi/agent/models.json is home-manager-managed from dotfiles/home/pi.nix
  and declares qwen3.8-max + deepseek-v4-flash-0731 on the plan rail.
  deepseek-v4-pro-0813 is NOT yet declared; it is added to pi.nix (one
  model entry + home-manager switch) when the first pro-routed lane is
  scheduled — not before S1 calibrates the flagship.
- Per-lane routing under maxParallel 1 is a host act: the supervisor flips
  pi's default model between dispatches. No worklist bytes change.

## 4. Metering verification (dispatch rule 1) — the ledger opens

- Window state at sitting 0: **zero plan-rail sessions since the reset
  stamped 08-14 10:06 UTC** (scan of ~/.pi/agent/sessions mtimes + usage
  records — the method validated against the Aug 7 numbers). The full
  10,000-credit week is available.
- Metered quantities on the plan rail: fresh input, cache reads (≈10% of
  input rate), output (≈6× input rate), scaled to each model's PAYG price.
  Transcript length is the cost center; estimate ≈3,000 credits for an
  ext0-shape lane on the flagship, pending S1's measurement.
- Ledger method: reconstruct per-lane burn from ~/.pi/agent/sessions
  usage records after each lane; console checked at seams only.
- S1 (job-limits-optional) dispatches SOLO as the calibration lane; every
  later dispatch requires a spend check (remaining ≥ lane + one retry) and
  respects the 15% reserve floor (≈1,500 credits never dispatched against).

## 5. First-contact lint exposure (recorded, not fixed)

- eta.json's spec-lint-core goal cites the verbatim ZETA sources
  `zeta-learnings/raw/instinct-fable.md` and `raw/instinct-opus.md`; the
  second filename contains a model-name token inside a path. If the L16
  implementation flags path components, the first contact bites here — the
  cure is an operator citation regeneration, per the record-don't-fix rule.
- specs/zeta/spec.md remains Status: proposed with its Unknowns typed; L10
  treats outstanding doubt as a warning at proposed, blocking only at
  ratified. Both specs ratify at the C1 seam sitting with the linter in
  hand.
- V-17's `Sonnet` token sits in silent-factory-worklists/
  epsilon-extension.json:184 — untouched this sitting (that worklist is
  not eta's governing bytes; regenerate at the first sitting that touches
  that file).

## 6. Pre-arm checklist (supervisor, after this commit)

1. `tally adapter smoke pi --assert-commit --pool campaign-agent` — the
   zero-attestation smoke proven at the adapter switch; costs one tiny
   plan-rail call, entered into the ledger.
2. Admission rehearsal: arm, then verify the rendered agent argv carries no
   codex vocabulary (E8) and the gate set matches the four templates before
   any lane dispatches.
3. Dispatch cadence is manual polls (the campaign-poll timer stays stopped,
   ETA.md §5): one poll dispatches S1; no second lane until its ledger
   entry is posted.
