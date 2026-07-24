# Transfer brief: spec-kit's workflow-engine step vocabulary

Source: github/spec-kit's `specify workflow` engine (studied 2026-07-24 in the
managed-agents deliberation; not cloned here — this digest is the settled ruling record).
Role in tally-flow: **completeness checklist only.** The engine itself is disqualified as a
runtime (synchronous foreground CLI that holds compute while waiting — violates tally's
central principle). Its step-type catalog is used once, as a checklist proving the flow
dialect can express every shape a mature workflow tool ships, then it exits the
architecture.

## 1. The catalog → dialect realization

| spec-kit step type | tally-flow realization |
|---|---|
| `run` (invoke an agent) | `job()` / adapter sugar |
| `if` / `switch` | plain JS on witnessed results |
| `while` (loops, back-edges) | plain JS loop; per-back-edge iteration counter + per-run maxNodes cap (FLOW-SPEC counters section) |
| fan-out / fan-in | `parallel()` / `pipeline()` over `job()`s; join = daemon barrier await |
| `gate` (human approval as durable wait) | **DELIBERATELY NOT PORTED** — closed ruling: no mid-run human gates ever; human validation is the culmination artifact (PR, report, morning queue). This deletes mid-run external-event wait semantics from the design. |
| per-step agent selection (claude/copilot/gemini per step) | native via the adapter map — each `job()` names its adapter+pools; the one thing workflows.js structurally cannot do and tally already does |

## 2. What stays with spec-kit (out of tally-flow scope)

The SDD artifact cadence — constitution/specify/plan/tasks as committed intent documents —
stays in the agency/spec repo as vendored, inert markdown. `specify` never schedules
anything. The boundary contract: tally-flow consumes "a worklist file with stable task IDs,
acceptance criteria, and parallelism hints" (tasks.md-the-shape, not spec-kit-the-tool);
any SDD tool producing that shape feeds the same runner.

## 3. Trace philosophy (the one conceptual borrow beyond the checklist)

Spec-kit's trace is *intent* (spec.md/plan.md, committed, diffable); tally's witness chain
is *execution truth*. They compose: the repo carries intent artifacts between flow nodes
("the repo is the message bus"), the witness chain proves what ran. Flow scripts should
treat committed artifacts as the inter-node data plane and never invent a third channel.
