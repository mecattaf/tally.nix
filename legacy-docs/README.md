# legacy-docs — the pre-book corpus, to be read through and processed out

Everything in this directory predates the documentation site (`doc/`, mdBook, GitHub
Pages). It is kept verbatim because the code in `crates/` was built against it: these
files are the *provenance* of the implementation, not its user-facing documentation.

**Reading rule while the book is being written:** for any question about current
behavior, `FLOW-SPEC.md` and `NIX-SPEC-FLOW.md` win over `SPEC.md` and `NIX-SPEC.md`
wherever they speak to the same subject. Where the flow-era pair is silent, the pre-flow
pair still governs. Nothing here is normative once the corresponding book chapter lands.

**Superseded witness reservation (2026-07-26):** `FLOW-SPEC.md` §§19–20 deferred a witness
encoding decision to a later Tom-led session. That reservation was discharged by
[issue #84](https://github.com/mecattaf/tally.nix/issues/84) as amended: the final schema replaced
the predecessor encoding in place, archived predecessor state is inert, and TaskChampion is a
rebuildable view. The current ruling is recorded in [`doc/witness.md`](../doc/witness.md).

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

## Book processing frontier

- Book infrastructure (#63): the checked mdBook, fixed navigation, and direct Pages path are
  present; this infrastructure step absorbs no legacy subject matter.

- Generated options (#65): the shared core, Home Manager, and NixOS references are generated
  from the evaluated module schema during every book build; the wrapper-topology boundary and
  exact flow evaluation failures are documented alongside them.

- Intro, Getting started, and Concepts (#66): pending.

- Flows (#67): absorbed by the book's [authoring](../doc/src/flows/authoring.md),
  [dialect](../doc/src/flows/dialect.md), [host API](../doc/src/flows/host-api.md),
  [replay](../doc/src/flows/submission-and-replay.md),
  [pooled-review](../doc/src/flows/pooled-review.md), and
  [cross-host handoff](../doc/src/flows/cross-host-handoff.md) chapters.

- CLI, RPC, witness, and errors reference (#68): present in the book, including the advertised
  23-method wire table, current query protocol 4 and witness schema 2, offline verification,
  complete exit/error taxonomy, and the flag-only catalog plus existence-only `budgetPool`
  rulings.

- Operating tally, FAQ, Conventions, and README landing page (#69): absorbed into
  [`doc/src/operating/`](../doc/src/operating/), [`doc/src/faq.md`](../doc/src/faq.md),
  [`doc/src/conventions.md`](../doc/src/conventions.md), and the repository
  [`README`](../README.md).

- Architecture and rationale (#70): pending.

## Known divergences still being processed (legacy vs current implementation)

These were recorded by the flow campaign as ORACLE-DELTAS and by the later final-witness
ruling. Entries are struck from this processing ledger as the book states the one current
truth; the frozen legacy statements remain visible as provenance.

1. `NIX-SPEC.md §4` requires a nonempty `requiredGateIds` and treats a missing manifest as
   failure; `FLOW-SPEC.md §13` requires empty preset defaults and treats an absent manifest
   as `not-run`.
2. `NIX-SPEC.md §5` lists only `regex | jsonPath` scrape modes; `FLOW-SPEC.md §13` adds
   `jsonPathLast`.
3. `NIX-SPEC.md §2` is silent on the built-in meter's `consumptionCap` being
   token-denominated; the live module documentation now states it.
4. Catalog schema ownership: issue bodies and the §4 opening assign it to FS-7; the amended
   §4 assigns it to FS-4, which is how it shipped (FS-7 consumes it and adds goldens).
7. `FLOW-SPEC.md` §§19–20 reserve a witness epoch break and say none occurs in that campaign.
   [Issue #84](https://github.com/mecattaf/tally.nix/issues/84), Amendments 1–2, supersedes that
   reservation with the final in-place schema, inert predecessor archives, and the rebuildable
   TaskChampion view recorded in [`doc/witness.md`](../doc/witness.md).

The former `README.md` claim that tally "is not a workflow scheduler" was retired by the
flow-era doctrine amendment (`FLOW-SPEC.md §1`). The current
[`README`](../README.md) states the narrower truth: tally never *originates intent*, while
job-originated work passes the same bounded, witnessed admission door.
