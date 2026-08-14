# Learnings IX — The instinct consortium: what Fable and Opus say they are

*Two clean-room introspections — one Fable, one Opus, identical decontaminated
prompt, no tools, no tally/Nix/framework priming — on what they natively do
best as spec authors for unattended implementers. The convergence is strong
enough to treat as signal. 2026-08-14.*

## The convergent shape: a claim registry, not a document

Neither model, left alone, writes prose chapters. Both reach for a **closed
set of named, falsifiable claims with stable IDs and minimal connective
tissue**. Fable: "build the format as a claim registry with connective
tissue, and I will fill it well; build it as chapters of prose, and you get
my fluency where you needed my precision." Opus: "a closed set of named
claims," one testable claim per line, `B-04 Duplicate key, differing value →
exit 2, message names both line numbers.`

Both independently rank a **vocabulary section** as their single
highest-leverage cheap output — one noun per concept, declared once, held
identically forever ("most implementation drift I've seen is two words that
were nearly synonyms"). Both put **negative space in its own section**, never
inline (negations embedded in positive sentences get dropped by
implementers), and Opus adds the ordering rule: FORBIDDEN and DONE go last,
because recency wins in every reader. Both size hard against length — the
useful spec is 400–1,200 lines (Fable) or ~400–1,200 words of prose plus
code (Opus); above that, the middle is lost and the author drifts. Both
demand **omittable sections**: "if the format has 12 sections and my task
needs 5, you get 7 sections of fabrication."

And both, notably, reached for flat declarative case lines — *not* ceremony.
Fable: "Given/When/Then triples, but written as flat declarative sentences,
not Gherkin ceremony." Opus's notation is `condition → observable outcome`.
The instinct keeps EARS's discipline (stable IDs, one testable claim,
guard-then-consequence) and drops its costume. The house dialect should
follow: the numbered claim with an arrow is the native form; the
WHEN/WHILE/SHALL moods survive as vocabulary where they clarify, never as
mandatory boilerplate.

## The ruthless lists agree

The failure inventories are near-identical, and they matter more than the
strengths:

- **Unsourced numerals** — both models' #1. "Every unsourced number in my
  output is a guess wearing a lab coat."
- **Invented proper nouns** — paths, flags, function names, error strings,
  library API surfaces reconstructed from plausibility. Opus: "my single
  most damaging failure because it's the most confident-looking."
- **Claims about the existing codebase from memory** — "unless I read it
  this session, that is fiction. And I generate it because it makes the
  spec read coherently."
- **Silent respecification** (Fable) / **scope inflation** (Opus) —
  resolving the operator's ambiguity inside fluent prose without surfacing
  that a choice was made; filling template room with "we should also."
- **The fluency tell** — both name it as their most reliable
  self-diagnostic: confidence and correctness are indistinguishable on the
  surface, and prose quality *rises* as grounding falls. "Uniform fluency
  is my camouflage." "That inverse correlation is the most reliable tell I
  have about myself."

## The one accommodation both beg for: doubt gets a syntax

This is the strongest single finding. Fable asks for `[UNVERIFIED]`,
`[INVENTED]`, `[DECISION-n]` as legal tokens whose presence **blocks the
handoff** until resolved. Opus asks for structural provenance per statement:
`DECIDE:` (my decision, obey it) versus `BELIEVE:` (my model of your
system, verify before relying), plus `GUESS` on numerals and `NEW:` on
deliberately-invented identifiers. Same ask in different notation: *give
uncertainty a place that isn't a weaker sentence, and certain statements
get sharper* — "my worst failure mode becomes a queue of small explicit
questions instead of a fluent lie in paragraph four."

For the house format this is a direct mandate: provenance marks are part of
the claim grammar, and the spec-lint treats unresolved marks as admission
blockers. It composes exactly with byte-oracle-or-nothing: `[HUMAN-ATTENDED]`
was already the honest mark for an oracle gap; `[GUESS]`/`BELIEVE:` are the
honest marks for a grounding gap.

## What they crave is what tally already has

The ranked inputs both models want are, almost embarrassingly, tally's
existing artifacts: read access to the actual code during authoring (the
observed-tree law); one worked example of the end state (the golden
fixture); the prior art in-repo (styleReferences); the failure that
prompted the work (the evidence ledger); **the verification command — "if I
know the gate, I write to the gate"** (the gate set, handed to the author);
the forbidden list (forbidPaths, the deny-list); the operator's own
vocabulary (the glossary the lineage already maintains).

What degrades them is equally recognizable: mandatory templates, word-count
floors, spec-plus-plan-plus-tasks in one pass (each successive artifact in
a single generation is worse — the house's staged sittings are the native
cadence), "don't hallucinate" instructions ("I cannot comply by trying
harder — catch it mechanically instead"), and repeated "make it more
thorough" rounds ("each round adds material of decreasing truth").

## The divergence that isn't one

Fable wants rejected alternatives as *input* and a mandatory "decisions
made while writing" section; Opus wants rationale *withheld* from the
implementer ("rationale invites a weaker model to re-decide"). These
resolve into the altitude split the house already drew: rationale and
rejected alternatives live in the spec layer, for the operator and the
judge; the worklist goal hands the implementer pre-digested contract with
no reasoning to re-litigate. The consortium independently derived the
spec/worklist boundary — including Opus's rule for what crosses it: "every
name it would otherwise invent, and no reasoning it could re-litigate."

The handoff rules likewise mirror committed doctrine: verbatim for anything
that will be copied (fenced, "use exactly this"); **point at, don't
restate — one authority per fact** (D68, rediscovered verbatim); explicit
"implementer's choice" because silence is read as a gap to fill; closed
sets only ("exactly:" / "and no others" — bare "e.g." is banned); one claim
per line because compound sentences get half-implemented.

## The human, minimized and typed

Both want the human at the same few moments: irreversible or external
effects; unsourced constants (one-line confirmations); user-facing names
and taste with long half-life; genuine forks where both sides argue equally
well — presented and stopped at, never silently resolved. Both insist
humans must not line-edit the artifact: "give me the objection and let me
regenerate the section"; decisions arrive as answers to typed questions the
author emits. That is tally's steering protocol and the judge's structured
proposal, applied to authoring. Opus adds the single most valuable review
pass, one question only: **"which of these statements about my codebase are
false?"** — the falsity pass, which is the edge census run in reverse.

## The self-designed linter

Merged, the models specified spec-lint's model-facing half: numerals
without provenance (hard fail); backticked identifiers set-differenced
against the provided context, leftovers presumed invented unless marked
NEW; hedge lexicon (should, ideally, appropriately, robust, gracefully, as
needed) — hard fail in claims sections; "e.g."/"etc." banned; claims joined
by "and" split; bidirectional claim↔check coverage; vocabulary drift and
near-synonyms; example-executability against in-document schemas;
prose-to-code ratio; sentence-length variance as a grounding alarm; empty
mandatory sections as their own red flag; and a contradiction pass run by a
fresh model instance reading only the numbered statements. Opus, forced to
two rules: unsourced numerals and out-of-context identifiers — "those two
catch most of what actually breaks downstream."

## The staked capability

Both stake the same thing: **decomposition of fuzzy intent into an
exhaustive, non-overlapping, internally consistent enumeration of
discriminating cases, each a named testable claim** — including the cases
the operator did not think of. Not architecture, not judgment, not code.
The format's center of gravity should therefore be the numbered claim list
that gates can reference, with everything else — vocabulary, contracts,
forbidden list, unknowns, done — as its supporting cast.

The meta-finding, stated once: asked with no priming what they would build,
both models drew tally's discipline — executable checks as the deciding
gate, one authority per fact, typed questions instead of edits, staged
passes instead of one-shot thoroughness. The house shape is already
model-native; the format's remaining job is to give that shape a claim
grammar, a provenance syntax, and a linter that knows these two failure
lists by name.
