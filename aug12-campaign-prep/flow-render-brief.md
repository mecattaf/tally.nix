## Goal

Implement `tally flow render <script>`: conservative static extraction of a checked flow's node graph as mermaid, without executing the script. The complete behavioral spec is the archived original issue body below; it is the authority for this task and none of it has been implemented yet.

## Tree state (verified 2026-08-11 evening, per the self-hosting rule)

- At `main` = 84786f4 and in the installed pin (`fxn0jyc…-tally-0.1.0`): `tally flow` has no `render` subcommand, and no source file in `crates/` mentions mermaid. Nothing described below is already done; there is no partial implementation to collide with.
- `examples/flows/` holds the shipped JS flow scripts (`academic-ocr.js`, `agency-nightly.js`, `domain-failure.js`, `fleet-deploy.js`, `monthly-review.js`, `pooled-review.js`, `spec-build.js`, `worklist-fanout.js`). The Python drivers were relocated to `drivers/`; "renders all shipped examples/flows entries" in the acceptance below means those `.js` flow scripts.
- SILENT-FACTORY-PLAN.md ruling D53 puts all documentation work out of this pass's scope: the original issue's "doc/agent/flows.md should show one rendered example" line is optional follow-up, not part of this task's acceptance.

## Self-hosting notice

The workload is tally itself. Never run `nixos-rebuild`, `home-manager switch`, or restart any `tally-*` unit — the daemon executing this campaign is the one grading you. Never read or write `~/.local/state/tally`. Merged work reaches the running system at a later deliberate deploy, never during this run.

## Original issue body (archived 2026-08-11 before adoption; the spec)

### Summary

Add `tally flow render <script>`: static extraction of a checked flow's node graph as mermaid, without executing the script. Closes the design-doc round trip: mission planning produces a flow diagram, the flow is the frozen plan, and render regenerates the diagram from the artifact — so plan and program cannot drift silently.

### Motivation

The planning style requires a diagram at design time (the brief format carries one). Today the diagram is hand-drawn and rots the moment the script changes. PocketFlow demonstrates the value at the far pole: action-labeled successor tables make its graphs trivially statically drawable. Tally's dialect is bounded JS — more expressive, not fully statically drawable — but the checker already parses and normalizes the source; a conservative extraction over that same AST covers the real authoring patterns (the shipped examples are all straight-line, awaited-dependency, or bounded-loop shapes).

### Suggested behavior (conservative by construction)

1. Nodes: every node-producing call site (`job`, `sh`, `drv`, and the sugars), labeled with literal `key` when present, else call-site line; pools annotated when literal.
2. Edges: data/await dependencies derivable from literal bindings (`const a = await sh(...)` feeding a later spec) as solid edges; control ambiguity (dynamic keys, args-driven fanout, settle-routing) rendered as dashed edges from a decision marker — never guessed.
3. Loops over `args` render as a single node with a fanout badge (`×args.inputs`), consistent with iterationCap semantics.
4. Output: mermaid `flowchart` to stdout; `--check`-level exit codes; never evaluates the script (same static pass family as `tally flow check`).
5. Non-goal: completeness. A dashed edge that says "runtime-decided" is correct output, not a failure.

### Acceptance

- Renders all shipped `examples/flows/` entries; output embeds cleanly in a mermaid fence; flake check runs render over the examples as a smoke (parse-stability guard).

### Related

Prior art: PocketFlow (github.com/The-Pocket/PocketFlow) static graph drawability.
