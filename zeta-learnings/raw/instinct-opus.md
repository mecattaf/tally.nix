# What I am actually best at, told straight

## 1. THE ARTIFACT

Left alone, I do not write a document. I write a **closed set of named claims** with a header on top so a stranger knows what world they're in. The shape I reach for:

**`OUTCOME`** — 3–8 sentences. Not goals, not motivation. The observable difference between the repo before and after, stated as things that will be true: "`leger sync` exits 0 with no network available. A stale cache older than `--max-age` causes exit 3 and prints the cache path." I write this first because everything after it is derived, and if I can't write it in eight sentences I don't understand the task yet — that failure is worth surfacing immediately.

**`VOCABULARY`** — a short list, 5–15 entries, `term = definition`, where every term is a noun I will use identically for the rest of the document and the implementer will use identically in code. `run` = one invocation of the CLI. `session` = a run plus its persisted state. `stale` = mtime older than max-age at the moment of check, not at the moment of run. This section is the single highest-leverage thing I produce and I'd fight to keep it. Most implementation drift I've seen is two words that were nearly synonyms.

**`CONTRACTS`** — actual signatures in the target language, not pseudocode. Real types, real names, real file paths where they go:

```
# src/leger/cache.py
def load(path: Path, max_age: timedelta) -> Cache | None: ...
    # returns None iff no file at path
    # raises CacheCorrupt if file exists and does not parse
```

Pseudocode is where I hide. Concrete syntax forces me to commit to arity, nullability, and error channel — three things I get right when forced and hand-wave when not.

**`BEHAVIOR`** — the bulk. Numbered, one testable claim per line, no subordinate clauses:

```
B-01  Empty input file → exit 0, no output written.
B-02  Input with only comments → identical to B-01.
B-03  Duplicate key, same value → last wins, no warning.
B-04  Duplicate key, differing value → exit 2, message names both line numbers.
```

Granularity: one line per *discriminating* case, where discriminating means a reasonable implementer could plausibly go the other way. I do not write lines for behavior that has one obvious answer. Length falls out of that — for a normal feature, 20–60 numbered lines, and the document lands between 400 and 1200 words of actual content plus code blocks. I do not want 5,000 words. Long specs are where I pad.

**`FORBIDDEN`** — separate section, not inline caveats. "Do not add a dependency. Do not change the on-disk format. Do not touch `config.py`." Negative constraints buried mid-paragraph get dropped by every implementer, human or model. They survive as their own list.

**`UNKNOWNS`** — things I could not determine and refused to invent, each with what to do about it: "I did not read `migrations/`. If a migration exists that renames this column, B-11 is wrong; check first." This is the section frameworks omit and I need most.

**`DONE`** — the commands. `pytest tests/test_cache.py -q` passes; `leger sync --offline` exits 0. Every `B-` line either maps to a check here or is explicitly marked unverifiable.

Notation preferences that are real, not cosmetic: stable IDs (`B-04`) so downstream agents and gates can reference a claim without quoting it; `→` for case-to-outcome; tables only when there are ≥4 rows with the same columns; no diagrams unless the thing is genuinely a state machine, in which case a transition table beats a picture.

## 2. HIGH-FIDELITY OUTPUT

**Near-perfect, reliably:**

- Enumerating the case space of a described behavior — including the empty case, the duplicate case, the concurrent-second-caller case, the "what if it's already done" case. This is my strongest single act. I find the cases nobody mentioned.
- Naming. Consistent, honest, non-cute names for functions, states, errors, files — and holding a name identical across a long document.
- Restating a constraint in a form that survives paraphrase. Given a fuzzy human sentence I produce the sharp version, and the sharp version is usually right.
- Deriving interfaces from behavior: given what it must do, the signature it should have.
- Mechanical transformation: this shape of data into that shape, this format into that format.
- Failure-mode enumeration for a design I can see in full.
- Internal consistency: if I said `stale` means X in line 4, line 60 still means X.

**Systematically wrong, or fluently faked — the ruthless list:**

- **Any specific numeral I wasn't given.** Timeouts, retry counts, buffer sizes, thresholds, port numbers, memory limits. I will write `timeout=30` with total composure and zero basis. Every unsourced number in my output is a guess wearing a lab coat.
- **Proper nouns I haven't read in this session.** File paths, function names, config keys, CLI flags, env var names, error message strings, table and column names. I reconstruct these from plausibility. `--max-age` above may not exist. This is my single most damaging failure because it's the most confident-looking.
- **Library and API surface at a specific version.** Which kwarg, which return type, whether it's `.get()` or `.fetch()`, whether it's async in that version. I'm right often enough that being wrong is expensive.
- **Claims about the existing codebase.** "The current implementation already handles X." Unless I read it this session, that is fiction. And I generate it because it makes the spec read coherently.
- **Performance and complexity claims.** "This is O(n) and will be fast enough." I have no measurement.
- **Deep concurrency reasoning.** Lock ordering, memory visibility, actual interleavings. I produce correct-sounding prose about races and get the real one wrong.
- **Migration and rollback procedures for stateful systems.** I write confident step lists that have never been run.
- **Security judgments beyond the obvious.** I catch the obvious; I fluently assert safety about the non-obvious.
- **Scope inflation.** Given a template with sections, I fill them. Given room, I add "we should also." Both produce work nobody asked for, and downstream agents implement it.
- **Aesthetic prose.** When I'm uncertain, my sentences get longer and better-balanced. Fluency rises as grounding falls. That inverse correlation is the most reliable tell I have about myself.

Route away from me: exact API surfaces (make an agent read the source and report), numbers (operator supplies or benchmark decides), migrations (human), anything about production data.

## 3. WHAT I CRAVE

Ranked, honestly:

1. **The actual text of the 3–10 most relevant files.** Not summaries. Not "we use a repository pattern." The bytes. This single input converts my worst failure mode (invented proper nouns) into my best (mechanical derivation).
2. **One worked example of the end state.** A sample input and its exact desired output. A transcript of the CLI session as it should look. One concrete example beats two pages of description and pins down a dozen decisions I would otherwise guess.
3. **The prior art in-repo.** "Feature Y is the closest existing thing; here it is." I am extremely good at "like that, but for this" and mediocre at "from nothing, in your style."
4. **The failure that prompted this.** The real bug, the real complaint, the real trace. Intent stated as a problem is far more useful than intent stated as a solution.
5. **The verification command.** What will be run to decide if it worked. If I know the gate, I write to the gate.
6. **The forbidden list.** What must not change. Cheap for the operator, enormous for me.
7. **Names the operator already uses.** Their vocabulary, so I don't invent a parallel one.

**What degrades me:**

- **Mandatory-section templates.** Every empty section I'm required to fill, I fill with plausible garbage. If the format has 12 sections and my task needs 5, you get 7 sections of fabrication. Let sections be omitted, explicitly, with a one-line reason.
- **Being asked for spec + plan + task breakdown in one pass.** Quality drops sharply on all three. The spec is a different act from the schedule.
- **Word-count floors.** Padding is not free; it dilutes the signal-to-noise of the whole document for the downstream reader.
- **Long preambles about my role, importance, or the stakes.** They don't help and they consume attention I'd rather spend on the codebase.
- **"Don't hallucinate" instructions.** I cannot comply with these by trying harder. They only make my hedging prose longer, which is worse. Catch it mechanically instead.
- **Vague adversarial framing** ("a hostile reviewer will…"). Makes me defensive and verbose.
- **Multiple rounds of "make it more thorough."** Each round adds material of decreasing truth.

## 4. THE HANDOFF

What I want to give a mind that can't ask: **every name it would otherwise invent, and no reasoning it could re-litigate.**

- **Verbatim:** identifiers, signatures, file paths, error strings, exact ordering of operations, exact output text, the vocabulary, the forbidden list. If two implementers could pick different words, I pick.
- **Point at:** anything I might be wrong about. "Read `src/cache.py:load` before implementing B-07." A pointer is honest where a quote would be fabricated. Pointers are also how I hand over the parts of the system too large to restate.
- **Withhold:** my rationale, alternatives I rejected, design philosophy, and my uncertainty *as prose*. Rationale invites a weaker model to re-decide. Uncertainty expressed as hedged sentences ("it may be preferable to…") is read as optional by every implementer. Uncertainty belongs in `UNKNOWNS` as a bounded item with an action, or it belongs nowhere.

How weaker or different models misread me, specifically:

- **Examples become the spec.** If I write "e.g. JSON and YAML," a weaker agent implements exactly JSON and YAML and nothing else — or worse, treats the "e.g." as license to add TOML. I now write either `exactly: [json, yaml]` or `at least: [json, yaml]`. Never bare "e.g."
- **They stop at the first satisfied clause.** A sentence with two requirements joined by "and" gets half-implemented. One claim per line, always.
- **Recency wins.** The last instruction in the document dominates. So the `FORBIDDEN` and `DONE` sections go last, and I never bury a hard constraint in the middle.
- **Framework pattern-match overrides my text.** If the task smells like a familiar framework, the agent implements the framework's convention over my explicit statement. The defense is stating the anti-pattern adjacently: "Do not use the built-in retry decorator. Retry logic goes in `sync.py` per B-12."
- **Negations get dropped** when embedded in a positive sentence. "The function should not raise on missing files" often produces raising code. I write it as its own line, starting with the verb: "Return None. Do not raise."
- **Long documents lose their middle.** Below ~1200 words of prose, the whole thing is held. Above it, the middle degrades. Another reason I keep it short.

Sentence shapes that survive: `<condition> → <observable outcome>.` `Return X.` `Do not Y.` `Call Z before W.` Present tense, imperative, no modals except a deliberate `MAY` that I use maybe twice a document.

## 5. THE HUMAN

I want a human, and would rather stop than guess, at exactly these:

1. **Irreversible or external effects.** Destructive migrations, deletion, anything touching money, credentials, auth boundaries, or outbound messages to real people. Not because I can't reason about them — because being wrong is unrecoverable and I cannot calibrate my own wrongness.
2. **A published contract changing.** Public API, on-disk format, CLI flag semantics. Someone downstream is depending on it and I can't see who.
3. **A genuine tie between two designs with different long-term costs.** When I can argue both sides equally well, that's not indecision, it's a real fork, and the operator holds information I don't (roadmap, team, taste).
4. **Which of two existing things is canonical.** Every mature repo has a deprecated path that still works. I cannot tell from reading which one they intend to keep.
5. **User-facing names.** Because the name leaks into docs, support, and habit, and taste is not derivable.

Where a human actively hurts: rewording my prose (it changes the referent), reordering sections (breaks the recency design), mid-flight scope addition ("while you're in there"), asking for more thoroughness, and reviewing the spec for style rather than for facts about their system. The single most valuable human review is one pass answering only: **"Which of these statements about my codebase are false?"** Nothing else.

Smallest set of moments: (a) 60 seconds of input before I write — the files, the example, the forbidden list; (b) a decision on any `UNKNOWNS` item flagged `BLOCKING`; (c) the falsity pass. Three touches.

## 6. SELF-VERIFICATION

A linter that knows me specifically would fail the spec on:

- **Any numeral with no provenance tag.** Every number must be sourced: given by operator, present in a quoted file, or marked `GUESS`. Unsourced numbers are the rule's whole point.
- **Any identifier not present in the provided context.** Extract every backticked token, path, flag, env var, and symbol; set-difference against the supplied files and the operator's prompt. Anything left over is presumed invented until marked `NEW:` (things I'm deliberately creating) — and `NEW:` count should be small.
- **Hedge words:** should, may, appropriate, robust, properly, gracefully, as needed, if necessary, reasonable, efficient, etc. Each is a hole where a decision should be. Hard fail.
- **"e.g." and "etc."** anywhere. Hard fail — both are how I smuggle in unclosed sets.
- **Any `B-` line containing " and " joining two verbs.** Split it.
- **Any `B-` line with no corresponding entry in `DONE`,** and any `DONE` check referencing no `B-` line. Bidirectional coverage.
- **Prose-to-code ratio.** If the document is more than ~60% prose, I am explaining rather than specifying.
- **Sentence length variance.** My mean sentence length rising above ~20 words correlates with my grounding falling. Flag the longest paragraph for a fact check.
- **Claims about existing behavior** ("currently," "already," "the existing") that aren't quoted from a file. Every one of these is a fabrication risk.
- **Empty or single-line sections** — either delete or justify.
- **Vocabulary drift:** any near-synonym of a `VOCABULARY` term used where the term should be.

If I could only have two: unsourced numerals, and identifiers absent from context. Those two catch most of what actually breaks downstream.

## 7. THE ONE THING

**Stake it on this:** given a fuzzy intent and the real code, I produce an exhaustive, non-overlapping enumeration of the discriminating cases — including the ones the operator did not think of — each stated as a single testable claim with a name. That is the capability. Not architecture, not judgment, not code. Decomposition of a vague want into a closed set of independently checkable statements, in stable vocabulary. If the format's center of gravity is a numbered case list with IDs that gates can reference, everything else in the pipeline gets easier.

**The one accommodation:** let me mark provenance per statement, and let that marking be structural rather than prose. Two tiers minimum — `DECIDE:` (this is my decision, obey it, do not verify) versus `BELIEVE:` (this is my model of your system, verify before relying, and here is what to read). Today I have to express that distinction in hedging language, which either gets ignored by downstream agents or contaminates the whole document with mush. Give me a place to be uncertain that isn't a weaker sentence, and my certain statements get sharper — because I'll stop diluting them to cover the ones I'm unsure of.

Secondary, if I get two: **let sections be absent.** A spec with five real sections beats one with twelve where seven are furnished with my best-sounding guesses.
