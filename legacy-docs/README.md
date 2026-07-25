# legacy-docs — the pre-book corpus, to be read through and processed out

Everything in this directory predates the documentation site (`doc/`, mdBook, GitHub
Pages). It is kept verbatim because the code in `crates/` was built against it: these
files are the *provenance* of the implementation, not its user-facing documentation.

**Reading rule while the book is being written:** for any question about current
behavior, `FLOW-SPEC.md` and `NIX-SPEC-FLOW.md` win over `SPEC.md` and `NIX-SPEC.md`
wherever they speak to the same subject. Where the flow-era pair is silent, the pre-flow
pair still governs. Nothing here is normative once the corresponding book chapter lands.

| File | What it is | Status |
|---|---|---|
| `SPEC.md` | Pre-flow normative product spec — kernel, pools, leases, executors, evidence, witness, producers, adapters | Authority for everything the flow era did not amend; absorb into the book's Concepts + Reference chapters |
| `NIX-SPEC.md` | Pre-flow normative Nix surface | Authority *except* the six divergences below; absorb into the generated options reference |
| `FLOW-SPEC.md` | Flow-era normative spec (§1–§20): submission idempotency, orchestration provenance, brief, counters, concurrent wire, fairness, runner, dialect, host API, replay, semantic truth, data plane, meter, trailers, blast radius | Current truth; absorb into the Flows chapters |
| `NIX-SPEC-FLOW.md` | Flow-era normative Nix surface: `services.tally.flows`, flow pool, eval-time validation, catalog contract, gh→flow wiring, hardening presets, transport/scheduling options | Current truth; absorb into the module reference |
| `FLOW-GAP.md` | Ground-truth extraction of the tree at `main = 1ff5f3d` (what existed, what was wrong, what was left alone) | Historical; useful only as archaeology |
| `BUILD-SEQUENCE.md`, `FLOW-BUILD-SEQUENCE.md` | Unit decomposition and merge order for the implementation campaigns | Historical |
| `CODEX-HANDOFF.md`, `PRE-BUILD-ADDENDUM.md` | Agent-session handoff artifacts | Historical |
| `transfer/` (9 briefs) | Style-transfer corpora: Boa, rquickjs, workflows.js, Inngest/Cloudflare, durable execution, spec-kit vocabulary, dotfiles prior art, Nix module style, attic/Trustix | Keep as design-rationale sources for the book's Architecture chapter; each records *why* a mechanism looks the way it does |
| `campaign/` | Agent-campaign provenance, previously loose at the repo root and untracked: `CODEX-HANDOFF.md`'s companions — `completion-july20.md` (waves 12–13 correction handoff), `wave-11-5.md` (the r2 removal wave), `ORCHESTRATION-HANDOFF.md` (state at checkpoint 2), `FLOW-CAMPAIGN-HANDOFF.md` + `FLOW-CAMPAIGN-STATE.md` (the 2026-07-24→25 flow campaign's mission and step ledger), `MORNING-REPORT.md` (its outcome report), `wave-log.jsonl` | Historical. Records how the code came to exist and which sessions decided what; the surviving conclusions are in issue #82 |

## The six known divergences (legacy vs flow era)

Recorded by the flow campaign as ORACLE-DELTAS 4–9. In every case the implementation
followed the flow-era spec; the legacy sentence is the stale one. These must be resolved
in prose exactly once, in the book — not patched here.

1. `NIX-SPEC.md §4` requires a nonempty `requiredGateIds` and treats a missing manifest as
   failure; `FLOW-SPEC.md §13` requires empty preset defaults and treats an absent manifest
   as `not-run`.
2. `NIX-SPEC.md §5` lists only `regex | jsonPath` scrape modes; `FLOW-SPEC.md §13` adds
   `jsonPathLast`.
3. `NIX-SPEC.md §2` is silent on the built-in meter's `consumptionCap` being
   token-denominated; the live module documentation now states it.
4. Catalog schema ownership: issue bodies and the §4 opening assign it to FS-7; the amended
   §4 assigns it to FS-4, which is how it shipped (FS-7 consumes it and adds goldens).
5. `FLOW-SPEC.md §11.5` mentions a catalog path in the runner environment; the amended
   `NIX-SPEC-FLOW.md §4` requires `--catalog` and forbids `TALLY_FLOW_CATALOG`. The CLI
   contract won.
6. `NIX-SPEC-FLOW.md §1` `budgetPool`: normative producer rendering fixes the runner pool to
   `[ "flow" ]`; `budgetPool` is validated for existence only, and no extra render channel
   was invented.

Additionally, `README.md`'s claim that tally "is not a workflow scheduler" predates the
flow-era doctrine amendment (`FLOW-SPEC.md §1`: tally never *originates intent*;
job-originated work passes the same admission, bounded and witnessed). The book's landing
page states the amended doctrine.
