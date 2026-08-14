# ORIGINAL BODY ARCHIVE — issue #467
# flow: tally flow render — static mermaid extraction from a checked script
# labels: enhancement cli 
# archived 2026-08-11 before any `tally campaign project` adoption (project OVERWRITES title+body)

## Summary

Add `tally flow render <script>`: static extraction of a checked flow's node graph as mermaid, without executing the script. Closes the design-doc round trip: mission planning produces a flow diagram, the flow is the frozen plan, and render regenerates the diagram from the artifact — so plan and program cannot drift silently.

## Motivation

The planning style requires a diagram at design time (the brief format carries one). Today the diagram is hand-drawn and rots the moment the script changes. PocketFlow demonstrates the value at the far pole: action-labeled successor tables make its graphs trivially statically drawable. Tally's dialect is bounded JS — more expressive, not fully statically drawable — but the checker already parses and normalizes the source; a conservative extraction over that same AST covers the real authoring patterns (the shipped examples are all straight-line, awaited-dependency, or bounded-loop shapes).

## Suggested behavior (conservative by construction)

1. Nodes: every node-producing call site (`job`, `sh`, `drv`, and the sugars), labeled with literal `key` when present, else call-site line; pools annotated when literal.
2. Edges: data/await dependencies derivable from literal bindings (`const a = await sh(...)` feeding a later spec) as solid edges; control ambiguity (dynamic keys, args-driven fanout, settle-routing) rendered as dashed edges from a decision marker — never guessed.
3. Loops over `args` render as a single node with a fanout badge (`×args.inputs`), consistent with iterationCap semantics.
4. Output: mermaid `flowchart` to stdout; `--check`-level exit codes; never evaluates the script (same static pass family as `tally flow check`).
5. Non-goal: completeness. A dashed edge that says "runtime-decided" is correct output, not a failure.

## Acceptance

- Renders all shipped `examples/flows/` entries; output embeds cleanly in a mermaid fence; flake check runs render over the examples as a smoke (parse-stability guard).

## Related

doc/agent/flows.md should show one rendered example. Prior art: PocketFlow (github.com/The-Pocket/PocketFlow) static graph drawability.
