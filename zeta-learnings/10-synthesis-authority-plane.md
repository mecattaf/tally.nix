# Synthesis — the authority plane

*The tally-adjacent product: what the spec layer is as a system, not a
markdown template. Drawn from all four threads — the seam map, the Nix
styling, and the two-model instinct consortium — under the operator's
constraints: tightly integrated, not loosely coupled; two-way, not
proof-of-work one-way; the authoring runtime is Claude Code and the policy
is "don't get in the agent's way." 2026-08-14.*

## The frame

Tally today is one plane — the **work plane**: lanes, gates, receipts,
release; desired change reconciled into merged code. The adjacent product
is the **authority plane**: desired behavior held as claims, reconciled in
both directions against the tree. It is a compiler toolchain plus a
standing reconciler, and it ships inside tally's workspace, not beside it.
It introduces no new store — all of it is git.

## The two-way valve

Every claim carries a provenance mark, and **the mark determines which side
is authoritative for that claim**:

- **DECIDE** (default, unmarked): a ruling. Code must conform. If the
  oracle fails, the code is wrong — the downward direction tally already
  mechanizes.
- **BELIEVE: path**: the author's model of the existing system. If the
  tree disagrees, the spec is wrong — the upward direction, mechanically
  checkable: identifier set-difference against the tree, path resolution,
  example executability, unchanged-behavior claims bound to already-passing
  oracles.
- **GUESS** on numerals and **HUMAN-ATTENDED** on oracle gaps complete the
  grammar: honest marks for grounding gaps and verification gaps.

One token turns the spec from a downstream document into a reconciled
surface. Crystallization (tally's own final spec, an overnight or two away)
is authoring where nearly everything starts BELIEVE and the falsity pass
promotes claims to DECIDE. Derivation (agency) is authoring where nearly
everything is DECIDE and the census adds the BELIEVE inventory of the
substrate. Same format, both directions.

## The nouns

The *spec* — a claim registry per campaign identity: outcome, vocabulary,
rulings (including every ambiguity resolved while authoring — silent
respecification gets a mandatory home), numbered claims in flat declarative
arrow form (`2.1 receipt epoch ≠ current input → receipt does not count`),
unchanged behavior, forbidden list near the end (recency wins in every
reader), unknowns with actions, stages as build order. Sections omittable
with a one-line reason — a mandatory empty section is filled with plausible
fabrication, per both models' own testimony. Anchors derived from numbers
only, retitle-safe. Frozen at ratification.

The *trace* — `trace.json`, the append-only three-way join (claim ↔ task ↔
acceptance id ↔ evidence), written downward at sittings, completed upward
at release with receipts and merged shas. The meeting point of the two
directions; the product's ledger the way receipts are tally's.

The *contracts* — byte oracles in the double-pin shape the flake already
practices: fixture + producer check + consumer test, never editable to make
progress. The *evidence* — committed ledgers the claims cite, which is what
makes citation resolvable at the authority revision. The *ratification* —
an ordinary operator commit pinning a sha: the `flake lock` of the layer,
made at a keyboard, never from a phone, never by machinery.

## The verbs

Each priced by the placement law — admitted only when it deletes an
operator rule:

- **`tally spec lint`** — the enforcement engine. One Rust parser in the
  workspace, shared by every other verb. Surfaced three ways with one
  implementation: a flake check attribute (fleet-gate already runs `nix
  flake check`, so the spec gets fleet-tier standing coverage for free), a
  campaign gate argv, and the sitting's rehearsal step. Checks both
  directions in one pass. Rule set merges three sources: structural
  (grammar, ID uniqueness, claim↔oracle bidirectional coverage, vocabulary
  drift, trace resolution), model-facing as self-specified by the
  consortium (unsourced numerals, out-of-context identifiers, hedge
  lexicon, e.g./etc. banned, compound claims split), and the contract lint
  the agency pilot demanded (cross-schema resolvability, fixture
  producibility). Ships with a must-fail perturbation fixture from day one.
  Deletes: the manual analyze pass, pointer-checking by eye, the
  hand-maintained trace table.
- **`tally spec diff`** — the boundary review artifact, plan-shaped:
  claims added/removed/reworded at claim granularity, unchanged-behavior
  touched, and a prediction of which tasks' epochs the derived amendment
  would refresh. Ratification reviews a rendered delta of law, not a git
  diff of prose. Deletes: re-reading the whole spec at every boundary.
- **`tally spec census`** — the oracle census: every claim binds to
  exactly one of check attribute, witnessed gate argv, or HUMAN-ATTENDED;
  zero or two bindings is a defect. Coverage becomes an enumeration.
- **`tally spec coverage`** — renders claim ↔ task ↔ receipt ↔ merged sha
  from the trace joined with durable completion facts; release consumes it.
  Deletes: the hand-rendered close-out table.
- **`tally spec questions`** — drains the typed-doubt queue (GUESS,
  BLOCKING unknowns, DECISION-n) to the inbox. Answers arrive as
  steers/commits, never as edits to the artifact. Doubt becomes a queue of
  small explicit questions instead of a fluent lie in paragraph four —
  zero transcription acts at spec altitude.

Deliberately absent: `spec admit` (a spec becomes operative only through a
derived worklist commit — the sitting is the filter), `spec generate`
(authoring is the model's act), any archive verb.

## The two runtimes

**The deterministic runtime** is tally's: parser, lint, diff, census,
coverage — pure functions of committed bytes, living in derivations and
gates, never containing a model call.

**The authoring runtime is Claude Code itself.** The product does not build
an authoring tool; it equips the model already trusted with authorship.
Three pieces: the claim-registry format, which the consortium showed is the
model's native output shape — the format is the accommodation; the linter
as the model's feedback loop — "don't hallucinate" instructions cannot
work, mechanical catching can, so the loop is author → lint → fix; and the
typed-question protocol — the model marks its doubt instead of resolving
ambiguity silently, the lint blocks derivation while marks are outstanding,
the inbox carries them to the operator. The same move that turned ultracode
workflows into tally's flows: crystallize what the model does well into
house mechanism, and put the enforcement in the harness, not the prompt.

Between the runtimes sits the one human-attended operation: the
**sitting** — the compile step from spec to worklist stage. Inputs: the
observed tree, the ratified spec, drained questions. Output: one commit —
worklist stage, trace rows, census report under evidence/. Witnessed the
way everything is witnessed: the lint bites its output on the next gated
head. Post-ext1 the same commit is the arming act.

## The loop, standing

Downward, continuous: gates decide merges against oracles bound to DECIDE
claims — unchanged. Upward, continuous: every gated head re-runs the lint,
so the moment the tree moves under a BELIEVE claim, the head fails — the
code falsifies the spec through the same ladder the spec disciplines the
code; drift is a build failure the day it starts. Upward, bulk:
crystallization runs as a campaign whose deliverable is the spec, census
lanes emitting BELIEVE claims, falsity passes promoting them — governed by
a different identity, which is why the deny-list is scoped per governing
spec. Tally's own final specification is the first run of this mode, and
how the mechanism proves itself before agency.

## What this buys over Kiro and spec-kit, stated plainly

The honest premise first: both competitors contributed real conventions —
Kiro's EARS discipline, traceability, and unchanged-behavior clauses;
spec-kit's directory shape, wired constitution, and converge stance — and
the house format keeps them. The value claim is not that they are wrong.
It is that **both stop exactly where the machine should start.** Five
differences, each categorical rather than incremental:

**1. Enforcement is executable; theirs is exhortative.** Spec-kit enforces
its format through template prose and agent instructions; its analyze pass
is a model reading documents — and on agency's real corpus it reported
zero findings while three contract-vs-contract defects waited. Kiro's
analysis is likewise a model's opinion inside a proprietary binary. Here,
enforcement is a deterministic linter inside the gate ladder: a malformed
spec cannot merge, a stale claim fails the build, and the linter proves it
can bite via its own must-fail fixture. Verification moves from opinion to
derivation.

**2. Two-way authority; theirs is one-way.** Both competitors flow spec →
code only. Spec-kit's converge and Kiro's sync are agents re-reading and
re-mapping — advisory, model-driven, on demand. Neither has any mechanism
by which the code mechanically falsifies the spec. The BELIEVE mark plus
the lint make upward reconciliation continuous and deterministic, and
crystallization — code → spec as a witnessed campaign — exists in neither
product even as a concept.

**3. Proof, not checkboxes.** Their tasks.md completion state is a
checkbox an agent flips: self-reported, unevidenced. Here, completion is a
witnessed receipt chain, and the trace joins claim → task → receipt →
merged sha, rendered at release. "Requirements discharged" is a join over
durable facts. Neither competitor has an execution machine at all — they
are authoring workflows; this is authoring, witnessed execution, and proof
in one citable lineage. That is the deepest difference, and it is why the
layer must be tally-adjacent rather than standalone.

**4. Doubt has syntax and it blocks.** Both competitors emit uniformly
confident prose — precisely the failure mode both authoring models
testified to under introspection (fluency rises as grounding falls).
Spec-kit's `[NEEDS CLARIFICATION]` is the nearest analogue and it is
advisory decoration. Here, GUESS, BELIEVE, HUMAN-ATTENDED, and open
DECISION items are grammar, and the lint refuses derivation while any are
outstanding. The authoring model's worst failure mode is converted into a
typed queue on the operator's inbox.

**5. The format was elicited from the author, not imposed on it.** Both
competitors hand the model a mandatory template — and mandatory sections
produce fabricated filler, per the models' own testimony. This format is
the shape two frontier models independently said they produce at highest
fidelity: a claim registry with stable IDs, omittable sections, flat
declarative lines. "Don't get in the agent's way" applied as a design
method, which neither competitor attempted — Kiro is bolted to an IDE the
model cannot feel, spec-kit to scripts the model merely obeys.

In one sentence: Kiro and spec-kit structure what an agent *writes*; the
authority plane governs what a factory *proves* — and it is the only one
of the three whose spec can be wrong in a way a build notices.

## Delivery

Small and staged: `crates/spec-lint`, one flake check attribute,
`skills/author-spec`, the format doc and constitution slimmed to lintable
law, `specs/epsilon-extension/` as instance one — landable as a spec-layer
campaign whose worklist is itself the first consumer of the format. The
diff, census, coverage, and questions verbs ride later stages, each
admitted when it can delete an operator rule. Agency needs exactly two
additions when it thaws: the dialect bridge (the sitting reads its
existing spec-kit corpus — a read problem, not a migration) and the
substrate mechanics (cached fork builds, VM oracles, the modifying-delta
gate), both already designed.

The endgame ties back to the oldest note in this thread: spec → worklist →
receipts → release becomes a single citable lineage in one repository —
precisely the data a unified front-end renders. The swimlane shows what
ran; the claim it discharges is the why. The authority plane is what makes
tally legible, not just autonomous.
