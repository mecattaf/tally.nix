# Learnings VIII — The two lenses: seam map and Nix styling

*Distilled from the first ultracode workflow: two Fable agents, one reading
tally's machinery at HEAD for the spec layer's attachment points, one asked
what SDD native to Nix would be. Per the operator's framing, the second
lens's externals are Nix-facing stylistic references, nothing more — the
findings kept below are the ones that survive that demotion. 2026-08-14.*

## The governing observation, now with coordinates

The pressure justifying the spec layer is visible as strain inside one
field: `goal` is capped at 12,000 characters (`spec-build.js:1057`) and the
live worklist uses it as a spec surrogate — requirement prose, evidence
citations, and source coordinates packed into single strings. Every seam is
a place to decompress that payload into a citable layer *without the
machinery noticing*. The expected effect is itself the metric: goal lengths
should fall from ~2,000 characters toward a few hundred as requirements
move to anchors. No lint rule enforces this — the shrink is how we know the
layer is working.

## The seam rulings worth keeping

**Pointers attach at requirement granularity.** Not criterion (fragments
the why), not file (destroys the two-budget model at agency scale — spec
bytes read must track spec bytes needed). The anchor grammar is a format
obligation: `### Requirement N — name` with the anchor derived from the
number only (`#requirement-2`), never the title, so retitling breaks
nothing. `readFirst.specSections` is a free string today, rendered verbatim
into the brief, with the worker already instructed to read cited sections —
so real pointers work with zero code changes. D68 is mechanized by
co-location: the lane worktree is checked out at the admitted revision, so
a spec in the same repository cannot be a phantom pointer. And the
repository's own documentation anticipated the genre before the layer
existed — `doc/src/flows/campaigns.md` already teaches
`specs/001-crm/spec.md#customer-model`.

**The spec stays out of the epoch key.** If spec bytes keyed attempt
budgets, every typo fix and evidence commit would reset every task's budget
— a global side effect from a document the machinery never reads, and a
reintroduction of the mutable-counter races the epoch model deletes. The
derivation sitting is the filter: a spec change that matters to a task
amends that task's bytes, and the epoch refreshes through the existing
derivation. The spec influences budgets only through the worklist — the
authority chain restated as a data-flow fact. Corollary: no `specSha256`
receipt stamp; the admitted commit already pins the whole tree. The one
actionable caveat: if ext0's stamp carries only the worklist blob sha, add
the admitted *commit* — one field pinning everything beats a field per
artifact class.

**The bright line on machine access.** The draft constitution's "the
machinery never reads or writes a spec" was half right. The defensible law:
no machine *decision* — admission, dispatch, budgets, merge, failure
classification — takes spec bytes as input; machine *rendering* — the
escalation report, the release record, campaign status — may resolve
citations for human and judge eyes. This permits the two integrations worth
having (escalation reports quoting the requirement a failing task
discharges; the judge citing requirement IDs in amendment proposals — which
needs no code at all, since the judge already reads the tree in a read-only
sandbox) while keeping the write half absolute.

**Traceability moves out of spec.md.** The draft had the trace table inside
a file frozen at ratification, but trace rows are authored per stage, after
ratification — a real contradiction. Resolution: `trace.json` beside the
spec, append-only, holding the three-way join (task ↔ criterion ↔
acceptance id ↔ evidence). This also settles the md/json split by the rule
that every JSON element must name its machine join: requirements stay md
under a lintable grammar (a parallel index would be a second copy that
drifts); the trace is JSON (appended every sitting, parsed at every lint
and release); contracts are JSON with fixtures; evidence stays md.

**Ratification is an ordinary operator commit.** A commit flipping the
status line is already the strongest authenticated act in the system — the
push credential is the trust root. A signed tag or ledger line would be a
second authority mechanism tending a fact the first already carries. Stage
and amendment approvals ride the ext1 inbox as designed; ratification
deliberately does not — steer from anywhere, ratify at a keyboard. And poll
re-admission never extends to specs: a spec becomes operative at exactly
one moment, when a worklist derived from it is committed — the worklist
commit *is* the spec's admission event, filtered through the sitting where
the judgment lives.

**The deny-list entry is scoped, not blanket.** `specs/<armed-identity>/**`
for the governing spec only — because a campaign whose deliverable is a
spec (tally's own crystallization, imminently) must write `specs/tally/`
while governed by a different identity. A blanket entry would make the
first real spec-layer campaign refuse itself.

**The constitution critique, accepted.** Roughly a third of the draft
articles are procedures dressed as law (the grind checklist belongs in a
skill), restatements of what ext0 lands as tested code, or operator
doctrine already owned by the ext0 skills task. The most load-bearing
absence: the freeze/append article — after ratification, exactly three
changes are legal: status transitions in spec.md, appends to trace.json,
additions under evidence/. Keep the citation-is-the-argument device, and
sharpen it into a test: an article that cannot cite a paid-for finding is a
deletion candidate.

## What works today — no machinery changes

Commit `specs/epsilon-extension/` with spec.md, trace.json, and the five
excavation ledgers under evidence/ (this is E6, and it makes the live
worklist's citations resolvable). Write `test/spec-lint` plus one flake
check attribute — fleet-gate already runs `nix flake check`, so the spec
gets fleet-tier standing coverage with zero fleet-gate edits. Author the
next stage's pointers as real anchors. Append trace rows at sittings. Hand
release the destination section plus a rendered coverage table (release
already renders operator intent verbatim). Keep the governing spec out of
every task's conflict domains until the deny-list lands.

The priced ext-era deepenings, each deleting an operator rule: the scoped
deny-list entry (deletes the post-run "did any lane touch the spec"
glance); admission resolving `specs/**` pointers (deletes manual pointer
checking, closes the phantom-pointer class); escalation reports resolving
citations (deletes "open the spec to see what this task was for" — a
transcription act); release rendering coverage from durable facts (deletes
the hand-rendered table). Rejected, showing the placement law biting: the
spec sha stamp, spec poll-admission, commit-trailer schemes, any new
worklist key.

## What the Nix lens contributes, as styling

The repo already practices the discipline unnamed, and the spec layer
should conform to its own flake's idioms rather than import anyone else's:
`hardening-doc-drift` pins prose to code with a check; the options
documentation is rendered from declarations *with a guard that fails if a
generated page is checked in* — which is exactly how the trace/coverage
tables should work (rendered, never authored); the golden-fixture
double-pin ("Nix's rendering and Rust's reading of it cannot drift apart
silently without both pins failing") is the required shape for everything
under `contracts/`; and must-fail perturbation fixtures — used seventeen
times in the flake already — are the standing answer to the grind's rot:
the linter ships with a deliberately broken spec that must fail, from day
one.

Two ideas survive as more than style. The **oracle census**: every
criterion binds to exactly one of a check attribute, a witnessed gate argv,
or an explicit human-attended mark — zero or two bindings fail the lint —
so coverage becomes an enumeration, not a judgment; and the sandbox makes
the honesty split self-enforcing, since an oracle that can live in a check
physically cannot depend on network, operator, or ambient state. And the
**pin chain**: ratified spec sha → worklist sha → receipt → release, each
link already existing or landing in ext0, so "which spec graded this code"
becomes a two-hop lookup instead of a claim.

## Standing subordination

Both lenses now rank below the instinct pass. The seam map says where the
layer attaches; the Nix lens says how its verification edge is styled; but
the shape of the authored artifact itself — structure, granularity,
notation — is decided by what the model that will author every spec says
it does natively best, with nobody in its way. The four reports meet in
the synthesis after the consortium returns.
