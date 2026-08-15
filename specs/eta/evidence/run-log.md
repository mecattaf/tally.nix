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
