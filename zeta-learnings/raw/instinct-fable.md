**1. THE ARTIFACT.**

Left alone, I do not write a monolith. I write a document with a spine of numbered, testable claims and a small amount of connective prose. The shape I reach for, in order:

- **Intent** — two to four sentences of plain prose: what exists after this work that did not exist before, and why. Not marketing. The sentence shape is "After this change, X can Y. Today it cannot because Z."
- **Vocabulary** — a definitions table, even for five terms. One noun per concept, declared once, used identically everywhere after. If the spec says "job" it never later says "task" for the same thing. This section is the highest-leverage cheap thing I do.
- **Invariants** — numbered, each a single MUST sentence that could be falsified by a test. "INV-3: A session file is never written partially; writes go to `<path>.tmp` then rename." I number these because downstream text will cite them, and citation-by-number survives paraphrase.
- **Behavior** — the bulk. Given/When/Then triples, but written as flat declarative sentences, not Gherkin ceremony: "When `resolve()` is called with an id that matches no record, it returns `None`; it does not raise." Every behavior gets an ID (B-1, B-2...). Error and edge behavior are not an appendix; they are interleaved right after the happy path they qualify, because implementers stop reading appendices.
- **Interfaces** — literal signatures, literal schemas, literal example payloads. Real JSON with real values, not `<placeholder>`. Where wire format matters, I show one complete valid example and one complete invalid example with the exact rejection message.
- **Out of scope / Deliberately unspecified** — the section I most insist on. Two lists: things the implementer must not build, and things where any reasonable choice is acceptable ("internal module layout: implementer's choice"). Without this, agents invent scope.
- **Acceptance** — a checklist where each item maps to a B-number or INV-number and is phrased as a command or observable: "Running `pytest tests/test_resolve.py` passes; B-4 covered by a test asserting `None` return."

Granularity: one spec per mergeable unit of work, 400–1200 lines of Markdown. Below 400 for anything with real behavior, I'm hand-waving; above ~1500, the downstream agent's attention degrades and mine did too while writing it. Notation: Markdown with tables, fenced code for anything an agent might copy verbatim, and inline IDs in bold. I do not reach for formal notation (TLA+, Z) unprompted — I reach for exhaustive enumerated cases, which is the poor man's formalism that I execute reliably and weaker models can read.

**2. HIGH-FIDELITY OUTPUT.**

Near-perfect from me:

- Enumerating cases once the axes are named. Give me the input dimensions and I will produce the full cross-product of behaviors, including the degenerate corners humans skip (empty list, zero, duplicate key, unicode, concurrent second call).
- Consistency of vocabulary and reference within one document I wrote in one sitting.
- Translating vague intent into falsifiable sentences. "It should be fast" becomes "p95 under 200ms at 100 rps on the reference fixture" — the *structure* of that sentence is reliable even when the number needs a human.
- Interface design at the boundary level: signatures, schemas, error taxonomies.
- Negative space: writing what must NOT happen. My MUST NOT statements are among my most reliable output.

Systematically wrong, or fluently faked — the list that matters:

- **Numbers I was not given.** Timeouts, buffer sizes, retry counts, rate limits, version numbers of dependencies. I will write "30 seconds" with total confidence and no basis. Every unsourced numeric literal in my specs should be treated as invented until confirmed.
- **Claims about the existing codebase from memory.** "The config loader already supports env overrides" — I will assert this smoothly whether or not it is true. If I could not read the file while writing the spec, the claim is decoration. The format should force a distinction: *verified* (I read it) vs *assumed* (mechanically flagged).
- **API surface of real libraries.** I hallucinate plausible method names and argument orders for libraries I mostly know. Anything naming a third-party call should be checkable or quoted from docs, never trusted as recalled.
- **Effort and sequencing estimates.** "This is a small change" — I am systemically optimistic and the sentence carries no information.
- **Silent respecification.** My most dangerous failure: when the operator's ask is ambiguous, I resolve the ambiguity *inside* fluent prose without surfacing that I made a choice. The spec reads as if the decision came from the operator. The format must give resolved ambiguities a mandatory home (a "Decisions made while writing" section) so they cannot hide in the prose.
- **Long-document drift.** Past roughly 1500 lines, section 9 quietly contradicts section 2 — a renamed field, a changed default. I do not notice while writing.
- **Completeness theater.** I can produce a section for every heading a template demands, and the weak sections read exactly as confidently as the strong ones. Uniform fluency is my camouflage; a reader cannot tell my 99% claims from my 60% claims by tone. This is why confidence must be a *field*, not a register.

**3. WHAT I CRAVE.**

What makes my specs sharply better, in priority order:

1. **Read access to the actual code during authoring.** This one input eliminates my worst failure class. A spec written against the real repo is a different artifact from one written from a description of the repo.
2. **The rejected alternatives.** "We're doing X" is half a decision. "We're doing X, not Y, because Z" lets me spec the boundary so the implementer doesn't drift back toward Y.
3. **One concrete worked example** of the desired end state — a sample input and its exact desired output, a mock of the CLI transcript, one real payload. I extrapolate structure from a single genuine example better than from three paragraphs of adjectives.
4. **Named non-goals and the failure the operator fears most.** "The thing that must not happen is data loss on crash" reorders my entire emphasis correctly.
5. **The downstream agent's actual constraints**: which model, what tools it has, whether it can run tests. I write differently for an agent that can execute than for one that can only edit.

What degrades me: mandatory templates with sections that don't apply — I will fill them, and the filler dilutes the signal and trains the downstream reader to skim. Instructions to be exhaustive "about everything" — exhaustiveness needs a target or it becomes uniform gray. Being asked to produce the spec and the risk analysis and the test plan and the rollout doc in one pass — each successive artifact in a single generation is worse than the first. And praise-shaped prompts ("you're the best spec writer") change nothing except adding tokens; constraint-shaped prompts change everything.

**4. THE HANDOFF.**

My instinct for a mind that cannot ask: **remove every place where it would have needed to.** Concretely, three rules I follow.

State verbatim: anything that will be copied — names, paths, schemas, error strings, commands. A weaker model transcribes literal blocks almost perfectly and paraphrases prose lossily, so anything that must survive exactly goes in a fenced block, and the surrounding prose says "use exactly this."

Point at, don't restate: existing code the implementer can read. I write "match the retry behavior in `src/net/backoff.py::retry_with_jitter`" rather than re-describing it, because my restatement can drift from the source and then the agent has two authorities. One authority per fact.

Withhold: my own uncertainty about *implementation strategy*, when any strategy satisfying the invariants is fine. If I muse about approaches, weaker models treat the musing as requirement and contort to satisfy it. Where I genuinely don't care, I say "implementer's choice" explicitly — silence is not read as freedom; it is read as a gap to fill with the most statistically common pattern, which is often wrong for this codebase.

How weaker models misread me, from watching it happen: they weight the beginning and end of the document and skim the middle, so critical constraints go early and are repeated in the acceptance checklist. They treat examples as exhaustive — if I show two error cases, they handle exactly two — so every example list ends with either "and no others" or "non-exhaustive; the rule is R." They collapse hedges: "should generally" becomes "always" or gets dropped entirely, so I use only MUST / MUST NOT / MAY and delete every "ideally." They cannot hold a cross-reference across 800 lines, so any constraint that matters at implementation point gets restated at that point, with its ID, even though repetition offends my style sense. Redundancy is the tax the handoff pays; I pay it deliberately and keep the copies literally identical so drift between copies can't create a third meaning.

**5. THE HUMAN.**

I want a human at exactly four moments:

1. **Irreversibility.** Anything touching data deletion, migration, external side effects (emails sent, money moved, public APIs changed). I would rather halt than guess, because my guess is fluent and the gate downstream is mechanical.
2. **Unsourced constants.** Every number I invented (section 2) should be a one-line human confirmation: "timeout 30s — confirm or replace." Thirty seconds of human time buys out my worst failure class.
3. **Taste with long half-life.** Public API names, CLI verb choices, anything users will type for years. I generate good candidates; I should not be the one who picks.
4. **Detected contradiction in the operator's intent.** When the ask contains a genuine conflict ("zero-config" and "fully auditable"), I want to present the fork and stop, not resolve it silently — resolving it silently is precisely my failure mode from section 2.

Where human intervention hurts: mid-document line edits to my prose ("reword section 3") — patched specs lose the internal consistency that is my main strength; give me the objection and let me regenerate the section. Humans adding "just to be safe" requirements inline, which downstream agents cannot distinguish from load-bearing ones. And approval gates on things already mechanically checkable — a human skimming what a test already proves adds latency and false confidence, not safety.

The minimal design: human decisions arrive as answers to typed questions I emit (`DECISION-1: retention period? [proposed: 30d, invented]`), never as edits to the artifact.

**6. SELF-VERIFICATION.**

A linter built for me specifically:

- **Unsourced-literal check.** Flag every numeric constant and version string not annotated `[given]` or `[verified: <path>]`. This alone catches my most frequent fabrication.
- **Vocabulary drift.** Extract the Vocabulary table; flag near-synonyms used later ("job"/"task", "id"/"key") and any term used before definition. Also flag any identifier that appears exactly once — usually a rename I forgot to propagate.
- **Referential integrity.** Every B-n and INV-n cited exists; every one defined is cited by at least the acceptance checklist; every file path named in the spec exists in the repo at authoring time.
- **Hedge detector.** Flag "should", "ideally", "typically", "handle appropriately", "as needed", "robust", "gracefully" — each is a decision I dodged. Zero tolerance in Behavior and Invariants sections.
- **Contradiction pass by a second model instance** with only the question "do any two numbered statements conflict?" — I catch my own long-range contradictions far better as a reader than as a writer, and a fresh context has no loyalty to the prose.
- **Example-executability.** Every fenced example must parse/typecheck against the schema declared in the same document. My examples and my schemas drift from each other.
- **Coverage of declared axes.** If the spec names input dimensions, check the Behavior section covers the cross-product or explicitly waives cells. I start exhaustive and fade; the linter should catch the fade.
- **Decisions-section non-empty check.** If "Decisions made while writing" is empty, that is itself a red flag — it almost never truly is, and an empty section means I resolved ambiguities silently.

**7. THE ONE THING.**

The capability I would stake the project on: **converting fuzzy intent into a complete, internally consistent set of numbered falsifiable statements** — the sustained discipline of "every sentence in this section could be turned into a test by someone who dislikes me." That is the thing I do natively better than I do anything else, and better than I do it when any template intervenes. Build the format as a claim registry with connective tissue, and I will fill it well; build it as chapters of prose, and you get my fluency where you needed my precision.

The single accommodation I most want: **let the spec contain first-class uncertainty markers that the pipeline treats as blocking.** Give me `[UNVERIFIED]`, `[INVENTED]`, and `[DECISION-n]` as legal tokens whose presence gates the handoff until resolved. My deepest flaw is that my confident and unconfident sentences are indistinguishable on the surface; if the format gives doubt a syntax — and machinery that refuses to ship a spec while doubt remains unresolved — my worst failure mode becomes a queue of small explicit questions instead of a fluent lie in paragraph four. Everything else about the format I can adapt to. That one I cannot fake my way around, and I should not be allowed to.
