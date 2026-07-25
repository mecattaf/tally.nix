# Pre-build addendum — last additions before tally.nix is built

**Date:** 2026-07-19
**Purpose:** The spec is frozen ("complete and settled") except for the items below. This is
the one and only pre-build window: everything here gets ruled IN or OUT, the spec files are
amended accordingly in one commit, and then BS-0 starts. Nothing on this list may be
reopened mid-build; anything not on this list is not in the tool.
**Context:** tally's first production workload is governing the Agency implementation
campaign (spec-kit corpus + Codex writers + Chromium build worker + daily steering). Items
were surfaced by: the supervisor-contract mapping, the selfaware/OUVERTURE absorption, the
daily-steering design, and the proof-of-work ladder. Each item says what changes, where in
the spec it lands, and a recommended default. Ruling format: mark **IN** or **OUT** (or the
named choice) on each line.

---

## A. Adapters

### A1. Codex adapter preset — RULING: [IN]
Ship a `codex` adapter alongside the existing `pi`/`claude-code` presets: launch argv
(`codex exec …`), resume template (`codex resume <session>`), model flag, and a
jsonPath scrape manifest capturing session ref, model id (verbatim, no normalization), and
the CLI's self-reported usage block. Plus the adapter tests the old draft skipped.
*Why:* the Agency writer IS Codex; this is the whole point of the campaign fit.
*Spec touch:* SPEC §11 preset list, BS-10.
**Recommended: IN.**

### A2. Scraped usage lands as attestation only — RULING: [IN]
The usage numbers scraped from codex/claude-code output are written to the attestation
chain (unauthenticated-by-construction), never to canonical metering. Readers must treat
them as advisory calibration data.
*Why:* a CLI can report any number it likes; the forgery-closure depends on reader
discipline. This just makes the discipline explicit in the adapter contract.
*Spec touch:* SPEC §11 one sentence.
**Recommended: IN.**

---

## B. Subscription / budget pools

### B1. Budget debit path — RULING: [HYBRID]
How a cloud-agent run decrements its `sub:<acct>` window. Options: estimate-at-admission
(enqueuer supplies expected draw, debited at grant — deterministic, trusted, mirrors the
old `--cost` idea), scrape-actual (advisory reconcile afterward), or hybrid.
*Why:* the frozen spec defines the cap and window but is silent on the debit; this must
exist before the pool code (BS-4/BS-6) is written.
*Spec touch:* SPEC §4.2, NIX-SPEC §2.2.
**Recommended: HYBRID — estimate-at-admission is the authoritative debit and the real
throttle; scraped actuals recorded per A2 to calibrate the estimate over time.**

### B2. Reset-window semantics — RULING: [IN]
Rolling window; remaining budget is re-derived from witness + events on daemon restart
(reconstructable like every other pool, no state file of record). Per-pool `windowSec`
already expresses provider differences (5-hour, weekly, monthly).
*Spec touch:* SPEC §4.2 paragraph; BS-14 scenario (no golden oracle exists for this).
**Recommended: IN.**

### B3. External usage meter (optional feeder) — RULING: [IN]
Self-accounting (B1) only counts what tally itself spawns. If the same account is used
interactively during the day, tally over-estimates remaining headroom. Add an optional
per-pool meter input: a small `Restart=always` feeder unit (the selfaware `poller`,
promoted) refreshes actual window utilization from the provider's usage endpoint, and the
pool treats it as a clamp on remaining headroom — advisory in provenance, mechanical in
effect. Pool works fine without it (self-accounting only).
*Why:* the daily-steering design assumes headroom numbers reflect reality, and Tom uses the
account interactively too. Note: the meter must read the **programmatic** budget pool
(post-2026-06-15 plans split interactive credits from the monthly programmatic pool, and
tally's non-interactive drain spends the programmatic one).
*Spec touch:* SPEC §4.2 (one input), NIX-SPEC (one optional unit), BS-11-adjacent.
**Recommended: IN — this is the mechanized remainder of the selfaware protocol.**

### B4. Pool headroom query (GO/SLOW/STOP surface) — RULING: [IN]
A read-only CLI query (`tally query pools` or a `standup` section) exposing, per pool:
remaining capacity/budget, window reset time, and a rendered GO/SLOW/STOP word. Thresholds
(inlined here — this is the complete definition, no external source): short-window
utilization ≥ 90% → STOP, ≥ 70% → SLOW, else GO; long-window utilization ≥ 80% downgrades
GO to SLOW. Pools without a second (long) window skip the downgrade rule. Read-only; changes no admission behavior (admission is already the
gate — this is the human/steering-agent review surface).
*Why:* the daily steering session reads headroom + standup to size the day; keep the terse
vocabulary that already works.
*Spec touch:* SPEC §CLI/query surface, BS-8.
**Recommended: IN.**

### B5. Account rotation (pool-assigner) — RULING: [IN, DRIVER-SIDE]
A driver-visible assignment rule that rotates enqueued cloud-agent work across multiple
`sub:<acct>` pools with headroom, falling back to the local-GPU tier when every window is
exhausted. This mechanizes what the selfaware protocol explicitly reserved as a human
decision ("do not switch accounts yourself") — a deliberate supersession, per the
OUVERTURE ruling, needing explicit confirmation here.
*Placement note:* pure contention arbitration, so it may live daemon-side without breaking
the one law; equally implementable in the driver. If OUT, work targets one named pool and
account choice stays with the steering session.
*Spec touch:* SPEC §4 (small), or nothing if driver-side.
**Recommended: IN, driver-side first (no spec change; revisit daemon-side only if the
driver version proves annoying). OUT is also defensible — this is genuinely Tom's call.**

---

## C. Pools / leases

### C1. Generic resource kind for mutual-exclusion pools — RULING: [IN]
Add one opaque member to the `resource` enum (e.g. `mutex` or `generic`) so a capacity-1
pool can legitimately model any one-holder resource — concretely, the Agency
one-writer-per-source-slice path locks the driver will declare (one pool per declared
writable slice; overlap detection stays driver-side).
*Why:* today the enum is `[vram, build-slot, cpu-slot, budget]`; abusing `build-slot` for
path locks would lie in the ledger.
*Spec touch:* SPEC §4.1 / NIX-SPEC enum, one line.
**Recommended: IN.**

---

## D. Executor

### D1. Per-job wall-clock bound — RULING: [IN]
Optional `runtimeMaxSec` on the job/exec line, stamped as `RuntimeMaxSec=` on the transient
unit; expiry lands as a distinct verdict (not `failed`), eligible for bounded requeue.
*Why:* a no-tool "thinking loop" in a cloud-agent session burns subscription budget
invisibly; CPUWeight/MemoryMax don't catch it. This is the substrate-level runaway stop.
*Spec touch:* SPEC §3.5 exec line, §5.3 verdict enum (+1 value).
**Recommended: IN.**

### D2. Priority-tier rank reconciliation — RULING: [SPEC]
The two docs disagree on the "single canonical enum": SPEC §6.1 says
`interrupt=1000, high=100, medium=50, low=10`; NIX-SPEC §2 says
`interrupt=1000, high=30, medium=20, low=10`. One side must win before check-config exists.
*Materiality:* only relative order matters today; SPEC's wider spacing leaves room for
future intermediate tiers.
**Recommended: SPEC values (100/50), fix NIX-SPEC.**

---

## E. Witness / evidence

### E1. Evidence-class label (proof-of-work ladder) — RULING: [IN]
Add one opaque, driver-supplied string field to the evidence block, recorded verbatim in
the witness (suggested values by convention, not enum: `compiled`, `behavioral`,
`capture`). tally keeps enforcing exactly what it enforces today (exit / artifact-exists /
content-hash) — the label only records *which class of proof* this artifact is, so the
review layer can require "capture-class evidence" for visual/attended-seam tasks and
"behavioral" for protocol surfaces, and the ledger can answer "what tier of proof does this
verdict carry."
*Why:* the compiled → CLI/IPC-checks-out → snippet-video ladder; a video artifact is hashed
and witnessed like any other, its *meaning* stays attestation (one law untouched).
*Spec touch:* SPEC §5.4, one field, pass-through.
**Recommended: IN.**

### E2. Run-manifest hash field — RULING: [IN]
Add one optional, driver-supplied `manifest_hash` field to the job record, witnessed
verbatim. The driver stamps the frozen-corpus hash (CORPUS-MANIFEST + execution tag) into
every run; admission-refusal-on-mismatch is driver logic, but the ledger then proves which
corpus state every verdict was earned against.
*Why:* this is what makes the spec-drift closure auditable end-to-end — the only legal way
to change ratified text becomes a delta that re-cuts the manifest, and stale-corpus runs
are provably distinguishable. (Could ride an existing UDA instead; a named field makes it
first-class in `witness verify`.)
*Spec touch:* SPEC §5 witness fields, one optional field.
**Recommended: IN.**

---

## F. Build-scope confirmations (not spec changes — freezing the half-day shape)

### F1. Deferred at build time — RULING: [IN]
Confirm the minimal critical path: BS-0→BS-12 with `enforce = cooperative` only. Deferred:
the entire dmem / patched-systemd / servingSlice / remote-enforcement complex, RemoteLease
and cross-host re-adoption; **calendar stays** (daily-steering pacing) — there are no
deferred producers; BS-13 golden-diff beyond the BS-1 dominant test;
BS-14 except the fanout-guardrail, slow-sqlite, and pool-vanished/return scenarios (pulled
forward — they cover the no-oracle surface this campaign rides on).
**Recommended: IN as stated.**

### F2. Standing OUTs restated (so build agents don't creep them in) — RULING: [IN]
Explicitly NOT in tally, now or later, per the one law — these live in the Agency driver:
task DAG / ready-set / dependencies; git worktrees and workspace manifests; the review
lifecycle (REVIEW/REPAIR/VERIFIED states); the 6-way build-result taxonomy
(TEST_FAILURE vs COMPILE_FAILURE vs STALE_INPUT — content); G1–G4 semantics and the
byte-identical convergence compare; X1/X2 attended-seam blocking; reviewer policy;
approval-gated / streaming-interactive harness driving; any loop that decides *what* runs
next.
**Recommended: IN (i.e. confirmed OUT).**

### F3. R2 object-store intake is a standing OUT — RULING: [OUT]
The r2 producer is permanently out of tally. External object-store scanners deliver event
files through `events-dir`, which already applies the same enqueue narrowing as every other
producer kind; duplicating that intake inside tally would add credentials and scanner state
without adding a distinct resource-arbitration capability.
**Recommended: OUT.**

---

## Tally of the sheet

| # | Item | Default |
|---|---|---|
| A1 | Codex adapter preset | IN |
| A2 | Scraped usage = attestation only | IN |
| B1 | Budget debit path | HYBRID |
| B2 | Rolling window, ledger-re-derived | IN |
| B3 | External usage meter (programmatic pool) | IN |
| B4 | Pool headroom query (GO/SLOW/STOP) | IN |
| B5 | Account rotation | IN, driver-side (genuine call) |
| C1 | Generic mutex resource kind | IN |
| D1 | Per-job wall-clock bound | IN |
| D2 | Priority ranks | SPEC values |
| E1 | Evidence-class label | IN |
| E2 | Run-manifest hash field | IN |
| F1 | Build-scope deferrals | IN |
| F2 | Standing OUTs restated | IN |

Once ruled: amend SPEC.md / NIX-SPEC.md / BUILD-SEQUENCE.md in one commit citing this file,
then this addendum is closed and BS-0 begins. Spec-confirmation is complete at that commit.
