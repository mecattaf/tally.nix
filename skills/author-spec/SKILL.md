---
name: author-spec
description: Author, lint, and ratify a campaign spec under the house claim
  grammar, then sit the derivation from ratified spec to worklist stage. Use
  when creating or amending specs/<identity>/, resolving a spec's typed
  questions, ratifying at a boundary, or running a stage-boundary sitting.
---

# Author a spec and sit its derivation

Author against the observed tree, never a predicted one. Write claims, not
chapters. Doubt gets syntax, never tone. Ratify at a keyboard. The format is
`specs/README.md`; the laws are `specs/constitution.md`; this skill is the
procedure. Use `assign-tally` for the worklist the sitting derives and
`campaign-operator` after arming.

## Author the claims

Read the real files before writing one line; a claim about the tree you did
not read this session is fiction. Write Outcome first — eight sentences or
stop and surface that you do not understand the task. Declare every term once
in Vocabulary; flag created identifiers `(NEW)`. Record every ambiguity you
resolve as a Rulings row — an empty Rulings section is itself a finding.
Write one claim per line, `condition → observable`, exactly one oracle
binding (`[check:]`, `[gate:]`, or `[HUMAN-ATTENDED]`). Mark what you read
`BELIEVE:path`; leave what you rule unmarked; suffix every numeral `(given)`
or `(GUESS)`. Omit a section by writing `Omitted: <reason>.` under its
heading — never fill it. Forbidden goes last, verb-first, one prohibition per
line.

Then loop: run

    nix develop --command cargo run -p spec-lint -- specs/<identity> \
      --worklist silent-factory-worklists/<identity>.json --repo-root . \
      --mode sitting

fix, rerun until blocking defects are zero. Do not argue with the linter in
prose; change the bytes. For contracts of consequence, run the grind (A16):
implementation plan and conformance bar derived blind from the spec as the
single intent source; the bar frozen and read-only; converge by collision;
disagreements escalate as spec defects, never absorbed; the bar shown to bite
before it is trusted. Two measured limits are authoring rules: what both
derivations inherit is invisible to the method, and unlit territory pays its
debt serially — budget for the tail.

## Run the falsity pass

Hand the numbered claim lines — nothing else — to a fresh reader (operator,
or a fresh model instance with no loyalty to the prose) with one question:
"Which of these statements about my codebase are false?" Demote refuted
DECIDE lines, fix or delete refuted BELIEVE lines, and record every
correction as a Rulings row. Run one contradiction pass the same way: "do any
two numbered statements conflict?" Nothing about style; only facts.

## Ask, never guess

Emit doubt as typed lines: `DECISION-n <question>? proposed: <answer> (GUESS)`
for forks and unsourced constants; `UNKNOWN-n [BLOCKING] <statement> — <action>`
for what you could not determine. Answers arrive as typed replies, steers, or
commits — never as line edits to the artifact; regenerate the affected lines
from the answer and flip `(GUESS)` to `(given)`. Ratification happens only at
zero outstanding doubt: the operator, at a keyboard, commits the Status line
flip in an ordinary commit. No machinery hand ever touches the file. After
ratification the file freezes per constitution A22 — Status transitions,
trace appends, evidence additions, nothing else.

## Sit the derivation

At each stage boundary, in order: run the edge census against the observed
tree (per `assign-tally`), plus the oracle census over the spec — enumerate
every claim's binding, and name the attending moment for every
`[HUMAN-ATTENDED]` claim in the census report. Drain the questions queue.
Author or amend the worklist stage per `assign-tally` with `readFirst`
pointing at real number-derived anchors (`specs/<identity>/spec.md#r2`) and
goals citing claim ids and evidence ids instead of restating them — spec
churn reaches budgets only through these precise task amendments; the sitting
is the filter that keeps the spec out of the epoch key. Append
`kind: "sitting"` rows to `specs/<identity>/trace.json` joining claim → task
→ acceptance ids. Write the census report to
`specs/<identity>/evidence/census-<stage>.md` — record deviations, never
patch frozen inputs. Rerun the sitting-mode lint and the admission rehearsal
(every gate preflight argv verbatim, in a pristine worktree). Commit once —
worklist stage, trace rows, census report — and push. Pre-ext1, ring the
doorbell with `tally campaign arm`; post-ext1 the push is the arming act and
only the campaign's first `run` stays deliberate.

At the close boundary, render completion with
`spec-lint --coverage specs/<identity>`, review, commit the release rows, and
hand the table plus the spec's Outcome section to `tally campaign release` as
the operator intent. Walk away; the next gated head is the witness.
