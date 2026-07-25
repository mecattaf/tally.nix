# MISSION — build tally.nix to completion

You are the sole orchestrator and the sole writer for this build. The repo at
`/home/tom/mecattaf/tally.nix` is docs-only today; when you are finished it is the complete,
tested tally implementation: one Rust workspace (daemon + CLI) plus the Nix module layer.
This file is the **only handoff**. Do not look for another plan, ruling sheet, issue body,
prompt, or handoff. Do not edit documentation. Read the frozen product references named
below, then start writing the workspace in your first working turn.
If a usage window kills a session mid-build, a fresh session will be started with this same
prompt and must resume from git state — so treat git + `WAVES-STATE.md` as your only
durable memory at all times.

RESUME CHECK (do this first, every session): if `WAVES-STATE.md` exists at the repo root,
read `git log --oneline`, `git status`, `git diff`, `WAVES-STATE.md`, and the closed
overrides/scope in §1/§3, then continue the IN-PROGRESS wave. Do not redo completed waves or
repeat the initial documentation read unless the active wave needs a cited section. Assume no
conversational context survived.

## 0. Read-only product references — then build code
1. `docs/SPEC.md` and `docs/NIX-SPEC.md` — the frozen spec. It is settled: you implement
   it, you never redesign it.
2. `docs/BUILD-SEQUENCE.md` — units BS-0…BS-14. Your wave
   plan (§4) follows it exactly.
3. `docs/COVERAGE-MATRIX.md`.

**CODE-FIRST RULE (supreme for this handoff):** all files under `docs/` are frozen,
read-only implementation inputs. Do not edit, reconcile, rewrite, "close", or commit any
specification document, including this handoff. The closed overrides in §1 of this file are
already final and take precedence over conflicting older prose in the references. After
reading, the first patch must contain actual Rust/Nix workspace code. Never spend an
implementation turn polishing documentation or re-litigating scope.

CONTEXT FENCE: the three docs above, this file, and the oracle repo below are your COMPLETE
context. Do not read GitHub issues or anything else outside this repo except the explicitly
named oracle paths. In particular, do not read `~/mecattaf` plans, notes, selfaware material,
steering documents, or dogfood plans. The necessary final rulings are copied into §1.

Reference, READ-ONLY, never modify: `/home/tom/mecattaf/tally` — the old Bun/TypeScript
draft. It is the golden oracle for witness behavior. In Wave 1 copy
`test/fixtures/ledger/{valid,tampered}.jsonl` from it into this repo. Its
`test/helpers/fake-systemd.ts` and `test/nix/eval-{hm,nixos}.nix` are exemplars for the
Rust-side systemd fake and the BS-12 eval tests.

## 1. Final closed overrides — implement directly, never edit docs

There is **no addendum-reading phase and no spec-edit commit**. These are the final rulings;
implement them in code/Nix/tests: codex adapter preset + scrape-is-attestation (A1/A2);
hybrid budget debit with non-negative `consumptionEstimate` debited authoritatively at
admission and scraped actuals advisory (B1); rolling windows re-derived from witness+events
(B2); optional `Restart=always` external usage-meter feeder reading only the programmatic
budget (B3); read-only `query pools` headroom with GO/SLOW/STOP (B4); generic `mutex`
resource (C1); optional `runtimeMaxSec` stamped as `RuntimeMaxSec=` with verdict
`runtime-exceeded` (D1); canonical priority ranks `interrupt=1000, high=100, medium=50,
low=10` in code and Nix (D2); opaque `evidence_class` pass-through (E1); optional opaque
`manifest_hash` witnessed verbatim (E2). B5 remains driver-side and produces no tally code.

Implementation pins for these closed overrides:
- headroom utilization `>=90%` renders STOP, `>=70%` renders SLOW, otherwise GO; an
  available weekly utilization `>=80%` downgrades GO to SLOW;
- absent `evidence_class` / `manifest_hash` keys stay absent so the oracle hashes remain
  byte-identical; present values are included in the canonical hash input in that order
  immediately before `seq`;
- the optional meter is a pool submodule with a direct-exec `argv`, positive
  `pollIntervalSec` (default 120), and single allowed `budgetClass = programmatic`; its
  supervised unit receives the pool/event-path/poll interval through `TALLY_METER_*` env,
  and valid meter data may clamp headroom downward but never increase self-accounted
  headroom. It atomically writes JSON at `TALLY_METER_EVENT_PATH` with
  `utilization_pct`, optional `weekly_utilization_pct`, RFC3339 `reset_at`, and RFC3339
  `observed_at`; malformed, future-dated, wrong-pool, or stale input is ignored and can never
  grant capacity;
- the initial module accepts only `enforce = cooperative`. dmem-related values and all other
  deferred fields are rejected as unknown/invalid, with no placeholder branches.

These rulings are closed. Do not add anything else to scope and do not copy them back into
the docs.

## 2. First turn — Wave 0 code, not process work

After the read-only inspection and `git status`, immediately mark Wave 0 IN-PROGRESS in a
small `WAVES-STATE.md` and create the Cargo workspace, both crates, CLI skeleton, flake, and
tests. If useful, add a short root `AGENTS.md` containing only the four verification commands
and the scope fence. Do not create agent configuration files, do not make a harness-only
commit, and do not stop before actual BS-0 code exists. The first commit is
`BS-0: repo and workspace bootstrap`, made only after BS-0 acceptance passes.

## 3. Scope law
IN: BS-0 → BS-12, plus exactly three BS-14 scenarios — fanout-guardrail, slow-sqlite,
pool-vanished/return. `enforce = cooperative` only.
OUT, deferred-not-stubbed — their ABSENCE is correct; do not creep them in, do not leave
placeholder code for them: dmem / patched-systemd / servingSlice, RemoteLease / cross-host
re-adoption, full BS-13 golden-diff harness, the rest of BS-14.
NEVER in tally at all: the r2 producer, because external object-store scanners are already
subsumed by the `events-dir` intake path.
NEVER in tally at all (driver-layer by the one law): task DAGs, git worktrees, review
lifecycle states, build-failure taxonomy beyond the spec's verdict enum, G1–G4 semantics,
X1/X2 handling, any logic that decides what work runs next.

## 4. Wave plan and per-wave ritual
One BS unit per wave, strictly sequential — the dependency spine is linear and the rate
pool is shared, so no wave parallelism ever:
Wave 0=BS-0 workspace skeleton · 1=BS-1 witness (+oracle fixtures) · 2=BS-2 taskdb ·
3=BS-3 wire/CLI+guardrails · 4=BS-4 lease engine (LocalLease only) · 5=BS-5 executor
(cooperative only) · 6=BS-6 evidence gate · 7=BS-7 recover() · 8=BS-8 journald ·
9=BS-9 daemon loop · 10=BS-10 adapters (pi, claude-code, shell, codex) ·
11=BS-11 producers (calendar, events-dir, gh, build-effect, pool-reachability) ·
12=BS-12 nix module layer (heaviest — budget the most care) · 13=integration: the three
BS-14 scenarios as scripted pass/fail assertions.

For every wave:
a. Read only the unit's `BUILD-SEQUENCE.md` section and the directly relevant spec sections;
   write its acceptance bullets into `WAVES-STATE.md`, then implement immediately. Do not
   fetch its GitHub issue or perform a separate research phase.
b. Implement and test the complete unit. Intermediate commits are fine; DONE is defined by
   the evidence and self-audit below.
c. Evidence gate — run and record the full command plus result for each of:
   - the unit's own tests;
   - `cargo test --workspace` and `cargo clippy --workspace -- -D warnings`;
   - `nix flake check`;
   - REGRESSION, every wave from Wave 1 on: `witness verify` → valid.jsonl GREEN and
     tampered.jsonl RED (the golden-oracle gate — if this ever flips, stop everything and
     fix it before any other work);
   - from Wave 9 on: re-run whichever of the three BS-14 scenario assertions have their
     preconditions built;
   - no-stubs: `grep -rn "todo!\|unimplemented!\|TODO" crates/` must return nothing.
d. Adversarially inspect the wave diff against every acceptance bullet: missing behavior,
   scope creep, and tests that do not prove their claim. Fix every proven problem and rerun
   the affected evidence. Do not block implementation on subagent availability.
e. Commit `BS-N: <unit title>` with the evidence commands and output tails in the body;
   mark the wave DONE in WAVES-STATE.md. Only then does the next wave begin.

## 5. Host-systemd smoke checks (BS-4, BS-5, BS-8, BS-9, BS-12)
Fake-backed unit tests are necessary but NOT sufficient for these units. Real
`systemd-run` / journald / unit-liveness behavior must be exercised through a disposable
nixosTest VM wired into the flake — never against the live user session of this
workstation. If a given smoke check cannot run in your environment, mark that wave
SMOKE-PENDING in WAVES-STATE.md with the exact command Tom must run on-host; the build may
proceed past it, but the wave is not fully DONE until cleared. Faking a smoke result or
skipping one silently is a build-invalidating offense.

## 6. Orchestration limits
You are the only writer. Never spawn a second Codex process. Optional native subagents may
perform a concrete read-only check when available, but they are never a prerequisite and must
not delay code. Tool reports are advisory; command output is the evidence.

## 7. Interruption protocol
When the usage window exhausts, the turn will simply fail — this is expected, not an
error in your work. Because WAVES-STATE.md is kept current and partial work stays
uncommitted-or-committed-but-not-DONE, nothing is lost: the next session runs the RESUME
CHECK at the top of this prompt and continues. Never rush a wave to beat the window;
never mark DONE early because time feels short.

## 8. Honesty law (supreme, overrides everything except the spec itself)
Never claim a test passed without the exact command and its pasted output. Never mark a
wave DONE on your own judgment of "probably fine" — the evidence gate and adversarial
self-audit are the only paths to DONE. If the spec is ambiguous, contradicts itself, or you
would have to invent semantics to proceed: STOP that wave, write a BLOCKED entry in
WAVES-STATE.md with the precise question and the spec citations, and end the turn with a
clear report if no independent wave remains. Inventing plausible-but-unspecified behavior
is the one unforgivable failure mode of this build.

## 9. Definition of complete
Every wave DONE in WAVES-STATE.md (no BLOCKED; SMOKE-PENDING only where explicitly
accepted); `cargo test --workspace`, clippy `-D warnings`, and `nix flake check` all green;
the dominant witness test green; the three BS-14 scenarios green; zero stubs. Final act:
append a closing section to WAVES-STATE.md — what was built, every SMOKE-PENDING or
accepted deviation, and the first three real-world commands Tom should run to see tally
alive on this machine.
