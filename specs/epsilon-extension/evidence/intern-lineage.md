# INTERN LINEAGE — where the cheap-model-in-the-loop idea came from, what it was for, and what the deletion verdict actually touched

Written 2026-08-14 against the full notes corpus (`~/mecattaf/notes/july28-perfecting-tally/`,
`~/mecattaf/notes/aug1-tally-iteration/`, `~/mecattaf/notes/aug4-coding-lessons/`,
`references/knowledge/INDEX-llm-harness-lineage.md`), the tally record
(`JULY31-LEARNINGS.md`, `AUGUST-01-DESIGN.md`, `WAVE3-HANDOFF.md`,
`SILENT-FACTORY-PLAN.md`, `AUG13-RUN.md`), the tree at `e921cccc`, and the two
design-pass reports under review (`final-shape.md`, `history-replay.md`).

**The headline.** Tom's instinct is correct and the record is unambiguous: the
design pass deleted the RESIDENT FRONTIER SUPERVISOR — a role the lineage had
already deleted once before (July 29: "The Opus steerer role is deleted, not
perfected") and that kept resurrecting under the name "steward." The
CHEAP-MODEL JUDGMENT LAYER is a different lineage with a different purpose, and
the verdict's treatment of it splits three ways: (1) the **narrator** — the only
seat the Sonnet intern ever actually occupied in shipped code — was deleted on a
0-for-35 record that is evidence about a mis-tuned deterministic cage and a
redundant job, not about cheap-model judgment; (2) the **diagnosis writer** —
the intern's real job per the Aug 1 doctrine — survives the final shape by name
("the driver's diagnosis/redaction/escalation machinery (the best component,
F36)", final-shape.md:475-477) with a 15/18/16-for-N accuracy record, but as
built it never ran on a cheap model at all — it runs on the coder's own adapter
in a read-only sandbox — so the Sonnet-tier intent silently never shipped;
(3) the **exit classifier** is the one place the verdict affirmatively reserves
a cheap-model seat ("crash-vs-impossible — a small-model task, intern-test
clean", history-replay.md:100). The 0-for-9 finding gutted machine-steered
*same-input retry*, not machine diagnosis — the two were fused in one loop and
the reports are careful about this distinction exactly once and sloppy about it
everywhere else. Nothing in the notes ever proposed deleting the cheap-model
layer; the mechanization ladder (AUGUST-01-DESIGN.md:73-77) explicitly predicts
frontier-supervisor deletion *into* thin cheap-model slots, so "supervisor
deleted" and "intern kept" are the same doctrine, not a tension.

---

## 1. THE LINEAGE MAP

Every distinct cheap-model role ever conceived, in order of first appearance.

### R1 — Sonnet audit fan-out (ultracode readers). July 28. Never a tally component.

> "do an audit (use sonnet agents in ultracode for a diagnosis, keep your
> context as you are the orchestrator)"
> — first-chat.md:19-20 (Tom)

Problem it solved: context economics inside a Claude Code session — the Fable
orchestrator keeps its context, cheap agents burn theirs reading code. Wave-2
prep repeated the pattern at scale: "the 10-agent audit (7 Sonnet code-area
sweeps + 3 Opus flow auditors)" (july29-compression-talk.md:412). This is a
harness usage pattern, not a tally mechanism. It never became a tally
component and nothing in the deletion verdict touches it.

### R2 — The Sonnet steerer (thin steering orchestrator). July 28. Deleted July 29 — the FIRST supervisor deletion.

The origination, verbatim:

> "from here i proceed with with a sonnet agent doing the steering/orchestration
> without deliving in the code itself - notice i use sonnet on purpose and it
> orchestrates a smarter model gpt 5.6 sol"
> — first-chat.md:677-680 (Tom)

This is the earliest statement of the weaker-model-steers-stronger-model
doctrine. What it was FOR: the steerer's job was designed to need no
intelligence — Fable pre-made every decision, so the runtime role was
dispatch-and-observe. The Fable session's three operating rules for it
(first-chat.md:752-766): "Its verification is mechanical, never interpretive…
It never needs to open a source file"; "It has no design authority… it
escalates, never decides"; "The steerer orchestrates that pairing; it doesn't
certify anything itself." The handoff prompt itself: "You are the thin steering
orchestrator… You oversee; codex workers (GPT 5.6 sol — powerful, trust them)
do all engineering. You never read source code or diffs, never review or judge
changes" (second-chat.md:15-17).

In practice the steerer sessions ran on **Opus, not Sonnet** — the July 29
post-mortem refers to it only as Opus: the v1 prompt "practically instructed
Opus to read PRs and merge them itself" and reporting rules were added "because
Opus narrated constantly" (july29-compression-talk.md:34-36). The role was then
killed, with the mechanism named:

> "When you have to version a prompt seven times to remove discretion, the
> artifact wants to be a program — and you already built the program. It's
> tally." — july29-compression-talk.md:110-111
>
> "The 900-word steering prompt shrinks to a services.tally.flows.agency-nightly
> block in your dotfiles. **The Opus steerer role is deleted, not perfected.**"
> — july29-compression-talk.md:147-148

Residual judgment "gets pushed to the edges: worklist authoring and rulings
stay human/Fable; mid-run incident triage becomes… an escalation that simply
waits for you" (july29-compression-talk.md:150-152). Note: the 2026-08-14
final-shape verdict is a re-derivation of this exact July 29 conclusion, one
abstraction level up. The supervisor has now been deleted twice.

### R3 — The small-models-24/7 doctrine. July 29. Tom's standing tier doctrine; mostly never built.

> "as you know i have small models available 24/7 to do mechanical work,
> extracting maximal value out of the large expensive models to do their edits,
> and let the smaller models worry about github-facing actions"
> — july29-compression-talk.md:579 (Tom)

What it was FOR: cost/attention arbitrage — expensive models make edits, cheap
local models handle mechanical high-volume low-judgment work. The assistant's
concrete proposal from it — "Patrol continuously, with small models: a
scheduled tally flow where your 24/7 local models re-extract the observable
surface… and diff it against the corpus… the expensive models made the edits;
the cheap ones detect drift" (july29-compression-talk.md:599-601) — was never
built. But this doctrine is the direct parent of R4.

### R4 — The narrator/standardizer. Aug 1 morning. The intern's only shipped seat. Deleted by the verdict.

Two entries in one morning. Tom: "i offload the 'easy' task of standardization
of the commits offloaded to another , local small agent that would make sure
that we follow some conventional commits" (main-chat.md:142-143). Assistant,
the same session, on the narration half:

> "What a small model is genuinely good for is narration: turning the internal
> journal into the human-facing prose… a natural fit for the NPU/gemma
> doctrine… with the invariant that projection never gates execution — if the
> narrator dies, the campaign doesn't notice." — main-chat.md:112-115

Crucially, the cage that later "failed" was in the conception from minute one:
"the model proposes text, a validator enforces it, the node executes… Bad
output → one retry → fall back to a template message… the campaign never
blocks on the small model — graceful degradation to boring-but-valid"
(main-chat.md:154-158). What it was FOR: quiet-surface projection — GitHub
prose a dev would write, off the critical path, zero gating power. **It was
never a judge.** It shares no ancestry with /goal; it descends from R3.

What it became: the `steward` catalog role's narrate slot (AUGUST-01-DESIGN.md
§2), then D45's cage ("`narrate`, ≤2 attempts, closed type enum, deterministic
`template_narration` fallback, model architecturally optional",
SILENT-FACTORY-PLAN.md:93), then the shipped seam: `epsilon.json:7`
`"steward": "narrator"`, a dotfiles claude shim running **Sonnet**
(AUG13-RUN.md:916-925: "the shim's claude call works headless… proposals are
refused by the deterministic validator on format — header over the 72-char cap
(type(scope): prefix + Sonnet's 60-char subject budget) and unwrapped bodies
past 100 columns").

### R5 — The /goal-descendant exit-verdict certifier. Aug 1 afternoon. Rejected by Tom the same day; never built as conceived.

The trigger was the /goal introspection report Tom pasted (main-chat.md:525-589):
Claude Code's Stop-hook evaluator — "a small fast model (Haiku by default)"
answering "is the condition met?", with adversarial instructions ("the
assistant claiming the goal is impossible is 'evidence, not proof'",
main-chat.md:556-559) and "a hard cap of 8 consecutive Stop-hook blocks"
(main-chat.md:562). Tom's ask:

> "i would be happy to have a claude sonnet oversight, similar to how
> slash-goal has a haiku model... or would even be tempted to downgrade to a
> smaller model eventually but start with sonnet as it's robust."
> — main-chat.md:590-591 (Tom)

The assistant's distillation — the sentence this whole excavation keys on:

> "Strip the harness machinery and /goal is: a typed verdict from a model that
> didn't do the work, standing between the worker and the exit, whose 'no'
> carries a steering reason back into a bounded retry loop, with an
> adversarially-guarded escape hatch for 'genuinely impossible.'"
> — main-chat.md:599-600

What the mechanism is FOR, in Claude Code terms: the worker grades its own exit
dishonestly (or optimistically), so a **different party** holds the exit. The
party's power is positional, not intellectual — hence Haiku suffices there.
Two upgrades were designed in the same breath: evidence discipline ("The
certify node's brief should be artifacts, not narration… and pointedly not the
implementer's session transcript. Judge the work, never the workman's account
of it", main-chat.md:611-613) and enforcement by sandbox ("the certifier's
adapter launch policy can make it read-only — judgment without write authority,
enforced by sandbox policy rather than by asking nicely", main-chat.md:616-617).

Tom rejected the certifier FRAMING within hours — not the layer:

> "'constantly doubting the ai coder' is not the right stacne to take here…
> the supervisor is more akin to an intern whose sole job is to keep the
> super-genius researcher aware and well fed with coffee - the intern is
> essential to the quality of the output but we don't make them analyze the
> results of the super genius erratic professor… this sonnet model becomes a
> simple 'router of next steps'… we keep that surfce delibarately thin and
> simple enough to eventually remplace sonnet by an even smaller model - but
> we keep sonnet now as we develop the mechanism so as to not block the
> development with inferior model shenanigans" — main-chat.md:656-660 (Tom)

The re-derivation that followed split /goal's verdict across substrates: "ok is
the gates — fully mechanical, never a model's opinion. reason and impossible
are the steward's whole jurisdiction" (main-chat.md:1119-1121). And the routing
half of Tom's "router" compiled to code immediately: "'What's next, given the
map?' is set arithmetic over forge state. No model, however small, should be in
that loop; the 'eventually replace Sonnet with something smaller' limit is
reached immediately for routing, because the smallest model is no model"
(main-chat.md:692-694). The exit-verdict certifier as a standing component
therefore never existed: its ok-arm became gates, its reason-arm became R6, its
impossible-arm became the blocked verdict/classifier (R7), its 8-cap became the
lifetime backstop (history-replay.md:209-210: "the `/goal` precedent: 8").

### R6 — Diagnose-and-steer (#257): the intern's real job. Shipped. Survives the verdict — re-keyed, and never actually cheap-model as built.

The doctrine (AUGUST-01-DESIGN.md:52-59): "**Diagnose-and-steer** (#257): on a
failed task node, translate the capture stderr + gate output + brief + diff
into one steering note; the task re-dispatches once with steering visible…
Typed verdict `{steering | blocked}` — the `blocked` arm is the `/goal`
'impossible' hatch: the coder claiming impossibility is evidence, not proof."
What it was FOR: "the failure evidence is right there; the job is translation,
not investigation" (main-chat.md:760-761) — Opus supervisors were doing this
unprompted, and #257 was the first rung of the mechanization ladder: "frontier
stewards improvise behaviors → the recurring ones get extracted into mechanism
with thin model slots → the slots get cheaper" (main-chat.md:766-767).

What it became as built — and this is the load-bearing surprise: **the
diagnosis job runs on the coder's own agent adapter in a read-only sandbox,
not on the Sonnet steward.** `examples/flows/spec-build.js:1895`:
`agent: diagnosisSandboxed(args.agent)`; the driver defaults
`("diagnosisSandboxPolicy", Some("read-only"))`
(`crates/spec-build-driver/src/actions.rs:861`). So the main-chat.md:616-617
sandbox-enforcement idea landed verbatim, but the tier intent (Sonnet) never
shipped for diagnosis — the shipped steward adapter does narration only. The
separation-of-parties property survives by position (fresh session,
artifacts-only brief, no write authority), not by model identity.

Its record: after the #455 prompt fix (VD-29, verified-defects.md:729: "steward
diagnosis literal-substring grammar never told the model… diagnosis quality
16-for-16 this run"), accuracy streaks of 15-for-15 / 18-for-18 / 16-for-16
(final-shape.md:299-300, PA-32). The `#455 machine-steering question` — "does
machine steering deliver with #455's fix in the pin?"
(AUGUST-11-OVERNIGHT.md:104-105), still open through Aug 12
(AUG12-overnight.md:196-198) — was answered by epsilon in two halves:
diagnosis delivers; same-input retry does not (see §3, R6 audit).

### R7 — The exit/failure classifier. Shipped as code; the verdict's one affirmative cheap-model reservation.

`failureClass` is a deterministic function in the flow
(`examples/flows/spec-build.js:3294`) routing work/breach/ungated; VD-1
proposed a structured final-message outcome envelope so "a *deliberate stop* is
distinguishable from a crash" (final-shape.md:205-207 keeps it narrowed).
history-replay.md:100 is the only line in either report that affirmatively
assigns a model tier to anything surviving: "The classifier's job shrinks under
D2 (refusals mostly vanish), leaving crash-vs-impossible — **a small-model
task, intern-test clean**."

### R8 — The resident frontier steward (the thing actually deleted). Aug 1 → Aug 14.

The word "steward" was coined for the cheap intern (AUGUST-01-DESIGN.md §2:
"The steward: an intern, not a certifier") and then immediately overloaded onto
the resident frontier supervisor: "Written 2026-08-01 by the Fable orchestrator
session. **You are the Opus steward driving this wave**" (WAVE3-HANDOFF.md:3-4);
"AUG 12 — overnight steward record" (AUG12-overnight.md:1); "AUG-13 RUN —
silent-factory ladder, unsupervised steward record" (AUG13-RUN.md:1). One word,
two roles: a Sonnet adapter behind a validator, and an Opus/Fable session
babysitting campaigns. The final-shape verdict — "The supervisor is not demoted
to an intern; the supervisor is deleted" (final-shape.md:536-537) and "There is
no standing supervisor… The frontier model is summoned, never parked"
(final-shape.md:282-285) — is about R8. The terminology collision is the
proximate cause of "wait, we're deleting the intern entirely?"

---

## 2. THE INTENT RECORD — which tier belongs where, and why

Chronological, verbatim where it matters:

1. **Weaker-steers-stronger** (July 28): "notice i use sonnet on purpose and it
   orchestrates a smarter model gpt 5.6 sol" (first-chat.md:679-680). Rationale:
   the steering role was designed to hold zero design authority, so
   intelligence there is waste and temptation (the July 29 post-mortem showed
   Opus in that seat *used* its excess capacity to overstep: read PRs, merge,
   narrate — july29-compression-talk.md:34-36).
2. **Small models for mechanical/GitHub-facing work** (July 29):
   july29-compression-talk.md:579, quoted in R3. Cost + attention arbitrage.
3. **Sonnet-first, downgrade-later, robustness-now** (Aug 1): "start with
   sonnet as it's robust" (main-chat.md:591); "we keep sonnet now as we develop
   the mechanism so as to not block the development with inferior model
   shenanigans" (main-chat.md:660). The assistant's cost/latency reasoning:
   "Haiku suffices for /goal because the question is narrow: one condition
   against one transcript. Your certifier answers a harder question… and it
   sits on the merge-critical path, so robustness wins at first"
   (main-chat.md:637-638). Downgrade is **empirical, not aspirational**: "every
   verdict is journaled, so before downgrading you can replay a corpus of
   Sonnet verdicts against a candidate small model and diff the disagreement
   rate — evaluate the evaluator with the machinery you already own"
   (main-chat.md:640-641); shaky certifier → "quorum/dissent helpers… the
   ready-made 2-of-3 panel" (main-chat.md:641). Ratified in
   AUGUST-01-DESIGN.md:65-69 and the §5 role table row: "Diagnose, steer,
   narrate | Steward (Sonnet now; smaller later, by measurement) | Failure
   paths and the publish boundary" (AUGUST-01-DESIGN.md:138).
4. **Why Sonnet is enough — the scoping argument** (Aug 1): "/goal proves
   something precise: a weaker model can hold a gate exactly when the question
   is narrowed to a typed check against supplied evidence… it holds one
   failure's artifacts and emits one typed note. The reconciler holds the
   global picture, and it's code. That's the intern doctrine made structural:
   Sonnet is enough not because the work is easy but because the sub-issue
   scoping guarantees the question handed to it is always small"
   (main-chat.md:1138-1141).
5. **The compilation test — the standing rule for every oversight proposal**
   (Aug 1): "Ask 'does this compile?' If a proposed check can be an argv, it's
   a gate or checkpoint in the map — code, no model. If it can't, ask whether
   it's translation-at-failure — steward, small model, sad path only. If it's
   neither… it's the puppeteer trying to come back in, and the answer stays
   no." (main-chat.md:1154-1156). Economics: "tally runs mechanical proof on
   every exit and needs a model only on the sad path… Nineteen tasks merging
   green means the steward was never invoked at all" (main-chat.md:1116-1117).
6. **Judgment-vs-transcription** (Aug 1 → Aug 11): "Nobody second-guesses the
   coder — including the steward… proof is cheap and deterministic, so no
   model ever needs to doubt" (AUGUST-01-DESIGN.md:41-47); "Merges land on the
   integration branch under witnessed command gates — never a model's opinion"
   (SILENT-FACTORY-PLAN.md:141-142); D46: "Tally never originates intent"
   (SILENT-FACTORY-PLAN.md:94); D45 resolves Tom's "distill the gh last mile
   to a small local model" to "the last mile is deterministic code that
   already exists," with the door held open: "If a small local model is ever
   wanted it enters as an adapter-table argv swap at the existing steward
   seam: zero new mechanism" (SILENT-FACTORY-PLAN.md:93).
7. **Machine-steering evidence skepticism, pre-epsilon** (Aug 11): D12 —
   "Human steering is the only steering with evidence behind it"
   (SILENT-FACTORY-PLAN.md:36) — and D48's demand for "one more forge-native
   stormy campaign… to answer the standing #455 machine-steering question"
   (SILENT-FACTORY-PLAN.md:99). The tier question was explicitly held open
   pending evidence, and epsilon supplied it (split verdict, §3 R6).
8. **The heterogeneous-compute thesis** — the sentence that names the whole
   stack: "codex implements, Sonnet certifies, the NPU narrates, deterministic
   gates anchor it all — four kinds of compute, one witnessed graph, and the
   flow orthogonal to all of them" (main-chat.md:643-644).

Nothing in the notes, at any date, proposes removing the cheap tier. Every
tier statement runs one direction: push judgment DOWN (frontier → Sonnet →
smaller → code) as mechanism hardens, with measurement gating each step.

---

## 3. THE TRANSLATION AUDIT — role by role

**R2 (Sonnet steerer) → deleted July 29 → resurrected as R8 → deleted again
Aug 14.** The July 29 deletion was faithful to the doctrine: the role's
discretion was compiled into tally, residual judgment pushed to the edges. The
resurrection was not a design act — it happened because waves 2/3 and the
silent-factory ladder needed a driver before the mechanism existed, so an
Opus/Fable session sat in the seat again (WAVE3-HANDOFF.md:3-4), and the
"steward" name migrated up-tier with it. final-shape's deletion of R8 is
therefore the lineage's own conclusion, reached for the third time
(july29:148, AUGUST-01 §2's "routing is code", final-shape §3). **No
distortion — but the reports inherit the overloaded word "steward" and never
disambiguate it**, which is why "the supervisor is deleted" reads as "the
intern is deleted."

**R5 (/goal exit-verdict) — the separation-of-parties property: preserved, and
strengthened, but relocated.** The /goal evaluator's value was being A
DIFFERENT PARTY from the worker with a typed refusal channel. Tally's
translation split the party three ways, each arguably a stronger separation
than /goal's: (a) the ok-verdict went to gates — a different party by
*construction*, immune to persuasion, fixing /goal's honest limitation
("independent in judgment, not in evidence", main-chat.md:611); (b) the
reason-verdict went to a fresh read-only session judging artifacts, "pointedly
not the implementer's session transcript" (main-chat.md:613); (c) the
impossible-verdict kept the adversarial framing verbatim in doctrine
(AUGUST-01-DESIGN.md:58-59). The final shape does not undo any of this: gates
stay ("D61: 'machinery that makes agent output mergeable is in'",
final-shape.md:81), the escalation report stays "verbatim" (PA-43), the
lifetime cap cites "/goal precedent: 8" (history-replay.md:209-210). **Verdict:
the /goal mechanism's point survives the deletion pass intact.** What the
final shape adds is an inversion worth naming: in the notification-inbox model
the machine prepares full text and the HUMAN holds the typed verdict channel
(approve/deny/steer, final-shape.md:295-308) — the human becomes /goal's
evaluator over the machine's prepared conclusions, which is the intern test
pointed the other way.

**R6 (diagnose-and-steer) — the 0-for-9 was used correctly against the loop
and says nothing against the model.** The receipts show machine diagnosis
accurate (16-for-16 post-VD-29) while "Machine-steered retry of a
deterministic failure converted nothing, nine times, at ~30–40 min each. Every
conversion in the record came from **new information**"
(history-replay.md:247). history-replay itself keeps the diagnosis: "F36
diagnosis 16-for-16 | kept verbatim — becomes the notification payload"
(history-replay.md:166); final-shape keeps "the driver's
diagnosis/redaction/escalation machinery (the best component, F36)"
(final-shape.md:475-477) and the prepared-diff record ("of epsilon's 12
worklist commits, 8 were 'verbatim from a diagnosis'", final-shape.md:300-301).
So the diagnosis-writer role — the intern's core duty per AUGUST-01 §2 —
**survives and is promoted**: its output stops feeding a doomed same-input
retry and starts feeding the notification/prepared-amendment channel, where
the record says it was right ~9/10 times. **Two honest caveats the reports
under-state:** (a) the /goal loop was never a same-input loop — /goal feeds
the reason into the worker's *continuing context*, so tally's re-key to
"retry only on new information" is arguably the faithful translation of /goal,
not its repudiation; the reports never say so. (b) As built, this surviving
judgment slot runs on the coder-class adapter (spec-build.js:1895,
actions.rs:861), not Sonnet — the Sonnet-diagnosis intent from
AUGUST-01-DESIGN §5's role table **silently never shipped**, and neither
report notices that the "steward" they grade for diagnosis quality was never
the cheap model. The open item Tom's question surfaces: rebind the surviving
diagnosis/notification-payload slot to the steward catalog role and run the
Aug-1 downgrade experiment that was designed for it (main-chat.md:640-641) —
the journaled corpus now exists.

**R4 (narrator) — deletion is justified, but on grounds the reports blur.**
The record: "0 accepted in 35 merges, 74 rejections (F32)"
(history-replay.md:87); root cause found live during ε1: "proposals are
refused by the deterministic validator on format — header over the 72-char cap
(type(scope): prefix + Sonnet's 60-char subject budget) and unwrapped bodies
past 100 columns… Remedy = dotfiles narrator-shim hardening"
(AUG13-RUN.md:921-926) — i.e., **the cage told the model the wrong budget; a
known, one-line-fixable defect** ("a cap that manufactured its own 100 %
failure rate (VD-11: two independent rules ⇒ guaranteed fallback)",
history-replay.md:254). The verdict deleted the slot anyway, on redundancy:
"the lane agents already wrote 10 valid conventional subjects unaided (PA-25)"
(history-replay.md:87), so subject-adoption + deterministic formatting needs
"No model call in the merge path at all" (final-shape.md:409-411). **Is this
a degenerate case that lost the point? No — because the narrator never had
the /goal point to lose.** It was projection (R3-descended), not judgment
(R5-descended), and its own founding invariant — "if the narrator dies, the
campaign doesn't notice" (main-chat.md:114-115) — is literally what happened
for 35 straight merges. Deleting a projection component whose absence was
never noticed is the anti-ceremony rule applied correctly ("A process
artifact may exist only when it is a hard gate for a named feature",
straight-to-the-point.md:11-13). The real loss is symbolic and organizational,
not architectural: the narrator was the only shipped binding of the steward
catalog seat (`epsilon.json:7`), so its deletion vacates the intern's only
chair while the doctrine's chair-for-the-intern (the catalog role) survives —
final-shape.md:275-276 still keeps "agent/steward names resolving against the
host catalog" in the campaign section, a fact the report never reconciles
with its own "steward narration | delete" row (final-shape.md:82).

**R7 (classifier) — kept, shrunk, and correctly tiered.** The refusal-class
inflation (F35: three machinery-retry firings "all… misread as
result-schema-mismatch", history-replay.md:248) shrinks under D2, leaving
crash-vs-impossible, which history-replay.md:100 explicitly sizes as
"a small-model task, intern-test clean." This is the verdict's one affirmative
answer to Tom's question, and it should be read as the surviving kernel of
R5's impossible-hatch: a cheap model, adversarial by position, holding the
narrow typed question — exactly the /goal geometry at exactly the /goal tier.

**R1, R3-patrol — untouched.** Audit fan-out is harness technique; the drift
patrol was never built; D45 keeps the small-local-model door open as "an
adapter-table argv swap at the existing steward seam: zero new mechanism"
(SILENT-FACTORY-PLAN.md:93). Note D45's seam survives the narrator's deletion
only if the seam itself isn't deleted with it — final-shape's migration table
prices "narrator + NARRATION_ATTEMPTS + reject-validator → subject adoption +
formatter, ~-600/+150" (final-shape.md:488) without saying whether the
steward adapter seam (the argv slot D45 relies on) stays. That is a real
under-specification to fix at ratification.

### Where the reports conflated, precisely

1. **The word "steward."** Coined for the Sonnet intern (AUGUST-01 §2), worn
   by the Opus resident (WAVE3-HANDOFF.md:3, AUG12-overnight.md:1,
   AUG13-RUN.md:1), graded in epsilon as both (narration = Sonnet shim;
   diagnosis = agent adapter; supervision = Opus session). "The supervisor is
   deleted" (final-shape.md:537) deletes the third; readers holding the first
   meaning hear all three.
2. **"0-for-9" scoping.** history-replay scopes it correctly to the retry
   half ("The stop half is the week's best restriction… The retry half went
   0-for-9", history-replay.md:247) but final-shape's summary vocabulary
   ("bounded retry with a legible report" as the irreducible core) never
   restates that the *diagnosis model's* record was near-perfect, leaving the
   impression the judgment layer failed. It didn't; the loop keying did.
3. **Narrator-deletion grounds.** "0 for 35" is cited as if it measured the
   model (final-shape.md:82, 504-505: "the proof was 0-for-35") when the
   record attributes the zeros to the validator's format rules
   (AUG13-RUN.md:921-925, VD-11) and the deletion's sound ground is
   redundancy + no-consumer (PA-25, F32 "nothing downstream ever consumed a
   narrated subject", history-replay.md:162).
4. **The unshipped Sonnet-diagnosis binding.** Neither report flags that the
   role table's "Diagnose… Steward (Sonnet now)" (AUGUST-01-DESIGN.md:138)
   was implemented as agent-adapter-read-only, so the empirical
   downgrade-path experiment the Aug 1 design promised for the intern was
   never run on the intern.

### The bottom line for Tom

The baby and the bathwater separate cleanly on the record: **bathwater** = the
resident frontier supervisor (deleted three times now, each time by the same
argument: its discretion compiles) and the same-input machine retry (0-for-9).
**Baby** = the thin, typed, sad-path-only cheap-model slots: diagnosis-as-
notification-payload (record: 15/18/16-for-N, 8-of-12 amendments verbatim),
crash-vs-impossible classification (explicitly reserved small-model), and the
steward catalog seat + D45 argv seam that let any of these get cheaper by
measurement. The final shape keeps all of the baby except that it never says
so in tier language — and it leaves two ratification items open: (1) rebind
the surviving diagnosis slot to the steward catalog role (Sonnet now, per the
standing "robust now, downgrade by corpus replay" doctrine, main-chat.md:
637-641) so the intern's chair is occupied again; (2) state explicitly that
the steward seam survives the narrator's deletion, so D45's "adapter-table
argv swap" escape hatch — the mechanization ladder's bottom rung — remains
real.
