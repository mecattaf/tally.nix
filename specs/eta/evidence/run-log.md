# eta run log — per-lane ledger and incident record

Standing consumer: the operator's status checks; the C1 seam sitting; the
P2 judge-tier replay (real spend numbers). Appended by the supervising
orchestrator after every lane. Credit figures use the doc-derived rates
(input ≈800/M, output ≈4,800/M, cache ≈80/M at flagship scale) until S5
lands scraped totals; the window is 10,000 credits, reset ~2026-08-22.

## Ledger

| # | when (UTC) | what | model | turns | in | out | cacheRead | ≈credits | window remaining |
|---|---|---|---|---|---|---|---|---|---|
| 0 | 08-15 13:34 | adapter smoke pi (pre-arm) | flagship | 3 | 4,751 | 258 | 0 | ≈5 | ≈9,995 |
| 1 | 08-15 13:52–13:58 | job-limits-optional attempt 1 | flagship | 24 | 51,501 | 16,276 | 0 | ≈120 | ≈9,875 |
| 2 | 08-15 14:20–14:47 | job-limits-optional attempt 2 | flagship | 37 | 62,739 | 16,302 | 0 | ≈130 | ≈9,745 |
| 3 | 08-15 15:03–16:10 | job-limits-optional attempt 3 — MERGED ffcb9a15 | flagship | 76 | 138,727 | 46,001 | 0 | ≈332 | ≈9,413 |
| 4 | 08-15 16:14–16:57 | oom-legibility attempt 1 — MERGED first try | flagship | 66 | 139,379 | 55,220 | 0 | ≈377 | ≈9,036 |
| 5 | 08-15 17:13–17:55 | excerpt-derivations attempt 1 — MERGED first try | flagship | 101 | 197,959 | 90,205 | 0 | ≈591 | ≈8,445 |
| 6 | 08-15 18:07–18:46 | adapter-relative-policies attempt 1 — gate-refused | flagship | ~125 | ~200,000 | ~85,000 | 0 | ≈570 | ≈7,875 |
| 7 | 08-15 18:55–19:42 | adapter-relative-policies attempt 2 — typed refusal (driver corpus outside domains) | flagship | 187 | — | — | 0 | ≈670 | ≈7,205 |
| 8 | 08-15 19:45–20:20 | substrate-numerals-guard attempt 1 — MERGED first try | flagship | 59 | 92,262 | 35,793 | 0 | ≈246 | ≈6,959 |

## Attempts 6–7 postmortem: the drip stops with a supervisor enumeration

S4's true consumer surface is eighteen files across six crates, the JS
flow, the presets, the python driver corpus, and the final-bar cases —
the cross-adapter literals are baked everywhere, which is what made V-15
the severity it was. The vestige ledger's four-domain list undersized the
blast radius, and each attempt found exactly one more wall (correct
behavior, expensive recon). Amendment 4 grants the grep-enumerated
surface and writes the file list into the goal so attempt 3 spends its
turns editing, not re-deriving. Standing lesson for later sittings:
before arming a delete-a-default task, grep the default's name and put
every hit inside the boundary.

## Attempt 6 — adapter-relative-policies (cargo-tests gate catch; domain gap again)

The lane deleted the three policy consts from the contract but
crates/tally/src/cli/campaign.rs imports and uses all three — and
crates/tally was not in S4's declared domains, so the lane could not
have fixed the consumer it broke. The cargo-tests merge gate refused the
head with the E0432 lines verbatim; no false green. Amendment 3 adds
crates/tally to the domain and names the consumer in the goal. Same
lesson as S1: the vestige ledger's domain lists were authored against
group shapes, not against the consumer graph — each gap surfaces once,
legibly, and becomes goal bytes.

## Calibration verdict (dispatch rule 2, closed)

S1 landed for ≈579 credits across three attempts; the winning
implementation lane alone was ≈332 credits, 76 turns, 67 minutes. pi's
usage records report zero cache reads on the plan rail at these
transcript lengths — the Aug 7 cache-dominance model does not apply at
small-lane shape, and real burn is roughly a tenth of the ≈3,000-credit
pre-calibration estimate. Projection: chapters 1–2 complete inside a
single weekly window with the 15% reserve untouched. Serialization
(maxParallel 1) stays until C1 regardless — the containment is the point,
not the credits. Console cross-check deferred to the C1 seam per the
ledger method note.

## Attempt 2 — job-limits-optional (ownership refusal; implementation done, wrong assertion home)

The lane produced the full implementation commit but placed the
daemonArgv-clean assertion in flake.nix; ownership containment refused
the merge — one path outside declared domains, named exactly. Attempt 1's
recon had already ruled the module-side placement, but that conclusion
lived only in its session; amendment 2 moves it into the goal bytes,
which is where inter-attempt knowledge survives. The steward-diagnosis
admission defect fired again as expected (S4 owns it); supervisor
performed the diagnosis manually per E5 rule 7.

## Attempt 1 — job-limits-optional (typed refusal; both defects legible)

Lane exited 0 with no commit and a typed `needs-authority` envelope naming
six files: the worklist declared `crates/tally/src` where the vestige
ledger's group-1 package said `crates/tally` — the six integration tests
under `crates/tally/tests/` construct `UnitLimits` struct literals and
must change when the fields become `Option`. Supervisor authoring error;
the lane's refusal was correct and cheap (≈120 credits, 6 minutes).
Fix: worklist amendment restores `crates/tally` as the domain (this
commit); the amendment refreshes the task's budget by epoch derivation —
the ext0 machinery working as designed.

Second finding, a live product defect on the ext0 machinery's first
diagnosis dispatch since the boundary deploy: `applyDiagnosisRole`
(examples/flows/spec-build.js near :2525) stamps `sandboxPolicy:
"read-only"` on the steward-bound diagnosis node, while the steward seam
refuses adapters that declare launch policies — so diagnosis-via-steward
can never render against any legal steward. The pass died at admission:
`invalid adapter "narrator": sandboxPolicy value "read-only" is not
authorized by this adapter`. V-15's class, one function beyond the
ledger's citation. Absorbed into adapter-relative-policies by amendment
(this commit). Until that task merges and deploys, failed-lane diagnosis
cannot auto-dispatch; the supervising orchestrator performs diagnosis
manually — which is where E5 rule 7 puts it anyway.

Calibration note: a recon-shaped six-minute lane on the flagship cost
≈120 credits with zero cache reads reported in pi's usage records; the
cache-read term of the Aug 7 model did not appear at this transcript
length. The ≈3,000-credit ext0-shape estimate stands until a full
implementation lane closes.
