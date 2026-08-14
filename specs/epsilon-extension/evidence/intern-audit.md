# The intern audit — what the deletion verdict actually did to the cheap-model
# layer, and where it overreached

Written 2026-08-14 against `e921cccc`, the epsilon receipts ledger
(`~/.local/state/tally/campaigns/attempt-receipts/epsilon/attempt-receipts-v1.jsonl`,
34 receipts, re-read line by line), `crates/spec-build-driver/src/actions.rs`,
`examples/flows/spec-build.js`, `nix/lib/adapters.nix`, the dotfiles narrator
shim (`~/mecattaf/dotfiles/home/tally.nix:36-65`), `AUG14-LEARNINGS.md`,
`AUGUST-01-DESIGN.md`, `~/mecattaf/notes/aug1-tally-iteration/main-chat.md`, and
the three epsilon-extension reports (`final-shape.md`, `history-replay.md`,
`ceremony-audit.md`).

**The verdict up front.** The design pass deleted the *resident frontier
supervisor* and was right to. Along the way it damaged three limbs of a
different organism — the **cheap-model judgment layer** Tom specified on Aug 1
("the supervisor is more akin to an intern… a simple router of next steps…
start with sonnet as it's robust") — and it did so on confounded evidence:

1. **The narrator was deleted on a rigged trial.** 0-for-35 is real, but 40/74
   rejections were a dotfiles shim envelope bug and 30/70 were validator rules
   the model was never shown (VD-12: "the steward is told four rules and graded
   on fifteen"). Deletion is still correct — the slot gates nothing and the
   valid subject already exists on the lane tip — but 0-for-35 must never again
   be cited as evidence that cheap-model judgment fails.
2. **The 0-for-9 retry finding was used to gut the /goal-descended
   steering-into-retry loop, and it is 9-for-9 confounded**: every one of the
   nine episodes was either a cause class the new design itself deletes
   (ownership refusals, gate-contract defects) or a checkpoint task that cannot
   self-fix by construction. The record contains **zero trials** of the class
   the "stop at 1" rule is written for.
3. **The final shape depends on a standing model slot it never names.** Its
   inbox rows say "notification + prepared amendment diff → approve"
   (final-shape.md replay rows 4, 7, 9); nobody in the document authors those
   diffs. Today that author is the operator transcribing the diagnosis. In the
   final shape it must be the diagnosis model — the intern, promoted.

The correct statement of the redesign: **the intern is not deleted; the intern
is promoted from transcriptionist-support to the standing judgment substrate**
— typed verdicts, diagnoses, prepared amendment proposals, exit classification
— while humans keep authoring/steer/approvals and the frontier stays episodic.
Sections below carry the evidence; §4 is the one-page corrected text with the
exact deltas from `final-shape.md` and `history-replay.md`.

---

## 0. The lineage, so nobody conflates the two "interns" again

Two different things share the word:

- **The "intern test"** (final-shape.md §0) grades *human* touchpoints: "every
  operator touchpoint either arrives with its full text already written and
  needs only approve/deny/escalate, or it must not exist." That test is about
  the operator, and it survives this audit untouched.
- **The intern model** is Tom's Aug 1 design, verbatim
  (`~/mecattaf/notes/aug1-tally-iteration/main-chat.md`):

  > "'constantly doubting the ai coder' is not the right stance to take here.
  > like the slash-goal situation, i have sonnet, a weaker model, supervise
  > while i have opus/fable/gpt sol with maximum thinking doing the coding
  > implementation. that means that the supervisor is more akin to an intern
  > whose sole job is to keep the super-genius researcher aware and well fed
  > with coffee — the intern is essential to the quality of the output but we
  > don't make them analyze the results of the super genius erratic professor."
  >
  > "this sonnet model becomes a simple 'router of next steps' … we keep that
  > surface deliberately thin and simple enough to eventually replace sonnet by
  > an even smaller model — but we keep sonnet now as we develop the mechanism
  > so as to not block the development with inferior model shenanigans"

  The same conversation resolved where the intern lives: routing "compiles to
  code" ("the smallest model is no model"), and "what remains for the intern is
  exactly what an intern is actually for — the professor's logistics and
  hiccups — and wave 2 already reserves the slot: **#257's diagnosis agent**."
  The /goal geometry was imported explicitly: "the typed verdict survives
  ({steering} vs {blocked} — /goal's impossible hatch), the steer-then-bounded-
  retry loop survives, but the model never grades the professor's work —
  deterministic gates and checkpoints do that, and they're code."

  `AUGUST-01-DESIGN.md` then froze it as doctrine — role table at `:138`:
  *"| Diagnose, steer, narrate | Steward (Sonnet now; smaller later, by
  measurement) | Failure paths and the publish boundary |"* — with the
  downgrade path at `:65-68`: *"Start with Sonnet while the mechanism
  stabilizes; the downgrade path is empirical, not aspirational — every
  diagnosis and narration is journaled, so a smaller candidate can be replayed
  against the Sonnet corpus and the disagreement rate measured before any
  swap."*

So the diagnosis layer **is** the intern, by named lineage (#257), and the
narrator was its second limb. The resident-supervisor deletion and the intern
are orthogonal: the supervisor was a *human/frontier transcription role*; the
intern is a *machine judgment slot*. `final-shape.md` never separates them, and
`history-replay.md` cut into the intern while aiming at the supervisor.

---

## 1. THE INVENTORY — every model-in-the-loop touchpoint

| # | touchpoint | who runs it today | record | final shape as written | my verdict |
|---|---|---|---|---|---|
| 1 | **diagnosis generation** | the campaign **agent adapter** in a read-only sandbox — *not* the steward | 18 diagnoses / 9 episodes / **zero wrong causes** | kept, silently (§5 "survives unchanged"); its retry consumer gutted by history-replay | **keep + promote**: typed verdict gains authority (gates retry, emits proposals) |
| 2 | **steering-reason-into-retry** (the /goal loop) | flow wiring: attempt-1 diagnosis → `steering.machineDiagnoses` in attempt-2's brief | 0-for-9 at attempt-2 conversion — see §2 for why that number is confounded | final-shape keeps 2-per-input; history-replay guts it to "transient faults only" | **replace both with diagnosis-gated retry** (§2) |
| 3 | **steward narrator** | Sonnet, via the dotfiles shim | 0-for-35/37; 74 rejections | **deleted** ("0 for 35… the correct subject already exists," final-shape concept table) | **delete the slot, correct the verdict** — the trial was rigged (§1c) |
| 4 | **exit classifier** (refusal/crash/impossible) | does not exist; `failureClass` (`spec-build.js:2101-2158`) is deterministic and 5-class | 3 refusals misread as machinery faults (F35) | shrunk twice: ext1's "cheap classifier" → history-replay's "crash-vs-impossible" → final-shape's self-declared envelope, no model | **merge into touchpoint 1**: the diagnosis call's typed verdict IS the classifier; refusals self-declare via the envelope |
| 5 | **prepared amendment diffs** on the notification inbox | nobody — today the operator transcribes (8 of 12 epsilon worklist commits "verbatim from a diagnosis," PA census row 5) | diagnosis text already contains the full content (receipts 16, 24, 29) | **assumed, never assigned** — the design's unnamed dependency | **assign to touchpoint 1**: diagnosis emits a structured proposal; deterministic code renders the diff |
| 6 | **escalation report composition** | deterministic (`action_escalate`, `actions.rs:2529` — assembles receipts, no model call) | PA-43 "best human-facing artifact" | kept verbatim | keep — no model needed; the model content in it is the diagnoses |
| 7 | worker lanes / authoring | frontier (Codex lanes; Fable/human authoring) | — | episodic, kept | out of intern scope — correctly untouched |

### 1a. Diagnosis: where it runs, what it costs, what the record shows

**Where.** `examples/flows/spec-build.js:3481-3495` builds the diagnosis job
from **the agent adapter**, not the steward:

```js
const diagnosisSpec = applyAgentPolicies(
  {
    argv: effective.agent.argv,
    adapter: effective.agent.adapter,
    pools: ["campaign-agent"],
    ...
    resultSchema: diagnosisResultSchema
  },
  effective.agent.diagnosisSandboxPolicy
);
```

with the sandbox defaulted at `spec-build.js:2249-2253` ("The diagnosis brief
prohibits mutation… sandboxed to match") and
`DEFAULT_AGENT_DIAGNOSIS_SANDBOX_POLICY: &str = "read-only"`
(`crates/tally-core/src/campaign_contract.rs:29`). Epsilon's worklist declares
no agent, so `default_worklist_agent()` applies —
`adapter: "codex".to_owned()`, `model: None`
(`crates/tally/src/cli/campaign.rs:167-176`) — i.e. **the 18-for-18 record was
earned by Codex at its CLI default model, fresh read-only session, not by
Sonnet.** The flow's comments still call it "the steward's diagnose slot"
(`spec-build.js:3299`, `:3507`) — a fossil of AUGUST-01's design that the
implementation drifted from: the steward got only narration; diagnosis landed
on the worker-tier adapter. Nobody ever ran the Aug-1 downgrade evaluation.

**The record.** F36 (frozen mid-run): "16 diagnoses, 8 episodes, zero wrong
causes; 14 of 16 named cause *and* correct remedy; 2 (episode 2) named the
state correctly and the remedy one escalation early." The full ledger adds the
ε2 chapter-gate pair (receipts 31-32, both correct — pardon 34 confirms "clippy
was the only failing stage" and the named fix merged): **18 of 18 correct
cause, 16 of 18 correct remedy, 0 wrong files, 0 wrong fixes**, on top of the
rolling 15-for-15 / 18-for-18 from chapters 0-2 (PA-32). The recurring
high-value behavior is naming the fix to *avoid* (receipt 24: "Do not merely
restore `Cargo.lock`; the gate will regenerate the change"). PA-32's closing
line is the whole promotion case in one sentence: **"The most reliable
component in the system has no authority."**

**Cost.** Unpriceable from the ledger — receipts carry no timestamp and no
usage (CA-3's exact gap). Countable: 18 diagnosis calls produced 18 usable
receipts; the narrator's ≥74 calls produced 0 accepted proposals. Each
diagnosis is one bounded read-only single-response session (12,000-char output
cap, schema-forced, evidence-primed). The corpus AUGUST-01 demanded for the
downgrade measurement now exists; CA-3's stamp fields make it priceable.

### 1b. The steering loop as shipped

Attempt-1 diagnosis is recorded by `action_steer`
(`actions.rs:2229`, `attempt must equal 1 or 2` at `:2265`, blocked at
`attempt == 2`, `:2357/:2394`) and rides into attempt 2's brief at
`spec-build.js:2218-2222` (`machineDiagnoses: machineDiagnoses(reconciliation,
task.id)`), under a mission that says "Treat only steering.authorizedComments
and steering.machineDiagnoses below as steering" (`:2209`). That is /goal's
"no carries a steering reason back into a bounded retry loop," verbatim
tally-shaped. §2 shows what its 0-for-9 record actually proves.

### 1c. The narrator: 0-for-35 was never a fair trial

The shim is 17 lines of shell in dotfiles (`~/mecattaf/dotfiles/home/tally.nix:47-65`):
one un-validated `claude -p --model sonnet --output-format text` call, fences
stripped with `sed`, piped through `jq -c`; a non-JSON reply (or any prose
around the object) yields an empty or invalid `TALLY_FINAL_MESSAGE=` payload.
Against it, the trial conditions:

- **40/74 rejections were the envelope** (PA-26: "the model's output never
  reached the validator as JSON… 54% is an envelope bug, not a grammar
  failure"). F32 ask 1: local validate-and-retry in the shim "alone recovers
  54%." The fix was scheduled for deploy-2 and **did not ride it** (F32: "the
  shim lives in dotfiles and is still unhardened").
- **30/70 were rules the model was never given** (VD-12, exact): the request's
  grammar block carries 4 keys; `validated_narration` +
  `validate_outcome_first` enforce 15 rules, including "body must open with a
  past-tense verb" and "leading sentence must end with a period" — nowhere in
  the prompt — and `headerMaxChars: 72` "is actively misleading: the model
  reads it as a subject budget and it is a header budget."
- **`NARRATION_ATTEMPTS = 2`** (`actions.rs:31`) with independent rules firing
  guarantees fallback (VD-11: "the budget the binding constraint rather than
  the model"; the loop itself is "otherwise well built — it feeds
  `previousRejection` forward").
- A 120-second runtime ceiling (`DEFAULT_STEWARD_RUNTIME_MAX_SEC: u64 = 120`,
  `campaign_contract.rs:31`) against the agent's 14,400.

The bitter irony: the **diagnosis path already wrote down the rule the
narration path violated** — `spec-build.js:3416-3419`: "The validator below
requires literal gate evidence, so… put its required strings in the model's
mission. **A rule disclosed only to the validator turns correct paraphrases
into silent steering loss.**" Diagnosis, with disclosed grammar and primed
evidence, went 18-for-18. Narration, with 4/15 disclosure and a broken
envelope, went 0-for-35. Same intern; one seam engineered honestly, one not.

**Verdict: delete the slot anyway** — deletion is right for final-shape's own
reason (the valid subject already exists on the lane tip 10/37 times, PA-25;
deterministic adoption + formatting needs zero model calls; the slot gates
nothing, aug-4 rule) — **but strike 0-for-35 from the evidence base about
model capability.** It is evidence about VD-11 + VD-12 + a dotfiles bug, and
E5's "keep-and-measure" was a defensible read of the same facts.

### 1d. The exit classifier: three documents, three shrinkages

CA-11 prescribed stealing /goal wholesale: "A cheap model deciding *'is this a
boundary refusal, a crash, or genuine impossibility?'* … plus a hard cap, is
the whole pardon verb, automated." EPSILON-EXTENSION ext1 carried it ("a cheap
classifier separates refusal / crash / genuinely impossible at the exit").
`history-replay.md` shrank it ("leaving crash-vs-impossible — a small-model
task, intern-test clean"), and `final-shape.md` §1b erased the model entirely
("What survives of VD-1 is only a structured final-message outcome envelope").
Each step was locally reasonable; the compound effect is that **no /goal-shaped
typed verdict survives anywhere in the final shape** — the precise overreach.

The right composition is one slot, not two: refusals self-declare through the
VD-1 envelope (the agent already knows and already names the paths, F35);
crashes are signal-level (no envelope, HEAD at base); and the residual judgment
— "genuinely impossible vs. try differently," /goal's adversarially-guarded
hatch where the worker's claim is "evidence, not proof" — is a field on the
diagnosis result the intern already sits positioned to emit.

---

## 2. THE 0-FOR-9 CONFOUND

`history-replay.md` §3: "Machine-steered retry of a deterministic failure
converted nothing, nine times, at ~30-40 min each… retry within an epoch only
for classified-transient faults; otherwise stop at 1 and notify." The fact is
verified — every diagnosis receipt in the ledger pairs `[1,2]`, no lone `[1]`
anywhere. The **inference** does not survive decomposition:

| ep | task (receipts) | actual cause | class under the new design | attempt-1 diagnosis already said |
|---|---|---|---|---|
| 1 | `chapter-gate` ε0 (1,2) | fleet-gate queries GitHub for a local head | **deleted** (local-audit arm, F28/PA-36) | "**No source fix is indicated**" — out-of-task |
| 2 | `squash-rowversion-ladder` (5,6) | Codex session death, flooding suite (F43) | **survives** — transient class | "commit all intended edits" — in-task (remedy premature, per F36) |
| 3 | `squash-rowversion-ladder` (8,9) | stale test out-of-domain → grant `663de5bc` | **deleted** (retro-certification) | "**First expand `conflictDomains`** or assign… to an authorized dependency" — out-of-task |
| 4 | `chapter-gate` ε1 (11,12) | clippy `large_enum_variant` in merged code | **deleted at authoring** (clippy in lane set, F33) | prescribes source edits a checkpoint task cannot make — out-of-task by task kind |
| 5 | `producers-config-variant-box` (13,14) | `producer_query.rs` outside boundary → grant `1324eaa4` | **deleted** (retro-certification) | "**Expand the ownership boundary** to include `producer_query.rs`" — out-of-task |
| 6 | `chapter-gate` ε1 (16,17) | 12/24 final-bar cases stale | **survives** — stale-assertion class | "**Do not retry chapter-gate unchanged; land this final-bar synchronization first**" |
| 7 | `port-fold-half` (24,25) | `Cargo.lock` outside grant → cohort grant `ef0443f8` | **deleted** (retro-certification) | "Have the operator add `Cargo.lock` to `conflictDomains`… **Retry only after** the lockfile is authorized" |
| 8 | `delete-python-driver` (28,29) | full consumer set outside boundary → grant `05aec25d` | **deleted** (retro-certification) | "commit the complete result" — in-task (the F35 refusal/crash conflation; attempt 2 named all five files) |
| 9 | `chapter-gate` ε2 (31,32) | clippy stderr macros in merged code | **deleted at authoring** (clippy in lane set) | prescribes source edits — out-of-task by task kind |

Read it off:

1. **Seven of nine episodes are cause classes the redesign itself deletes**
   (four ownership refusals — the class D2/retro-certification removes; one
   forge-native gate contract; two lint classes the ext0 gate template moves to
   authoring time). On the redesign's own terms these episodes never happen, so
   they cannot be evidence about its retry policy.
2. **Four of nine are checkpoint tasks** (`chapter-gate`), whose mission is a
   fixed command with "Do not modify the repository"
   (`spec-build.js:2230-2242`). A steered retry of a checkpoint can only
   convert if the failure was transient. Counting those four as "steered retry
   failed to convert" is counting water for failing to burn.
3. **The two residual-class episodes both converted on new information**:
   episode 2 converted after the **human steer** ("commit FIRST, verify
   second… `2>&1 | tail -30`" — the steering log CA-10 calls the single most
   valuable operator act), released by pardon 7 — **a steered retry that
   worked**, on the class (session death) that survives the redesign. And the
   pure-transient case converted with no steering at all: `port-worktrees`
   (receipt 21) took its free machinery retry and merged — which also corrects
   `history-replay.md`'s "machinery retry budget fired wrongly 3-for-3": two of
   three were misclassified refusals (`port-fold-half`), one was the mechanism
   working exactly as designed.
4. Therefore the record supports precisely: *retry with the same information
   does not convert deterministic failures; retry with new information (grant,
   amendment, steer, or a transient fault clearing) converts.* It contains
   **zero trials** of "diagnosis names an in-task code fix, steered attempt 2
   runs" — the only class where "stop at 1" and "retry with steering" give
   different answers — because every in-epoch failure epsilon produced was
   out-of-task or transient.

**And the machine already knew which was which.** In 7 of 9 episodes the
attempt-1 diagnosis explicitly named an out-of-task cause (rows 1, 3, 4, 5, 6,
7, 9 above — row 6 literally opens "Do not retry chapter-gate unchanged"). The
flow ran attempt 2 anyway, because the two-attempt budget is unconditional.
Seven of the nine second attempts were burned *against the machine's own
written advice*. The two misreads (rows 2, 8) are both the F35 signal
conflation — a refusal and a crash are the same signal — which VD-1's
structured envelope removes at the source.

**The corrected rule — diagnosis-gated retry, which is the /goal shape:**

> Attempt 1 fails → the intern diagnoses with a **typed verdict**:
> `retry` (in-task actionable fix; the steering reason rides into attempt 2 —
> the loop as built), `blocked` (out-of-task cause: authority, gate contract,
> dependency, source-fix-elsewhere; **stop at 1**, notify with the prepared
> proposal), or `transient` (machinery/session fault; free retry, existing
> budget). Hard caps unchanged: 2 attempts per input, ~10 lifetime.

Replayed against epsilon: saves 7 of 9 second attempts (the clippy and
final-bar gate cycles alone cost 74 and 61 minutes, F33), loses zero
conversions (there were none to lose), and degrades to exactly today's
behavior on the two misread episodes. It is strictly better than *both*
reports' rules: `final-shape.md`'s unconditional 2-per-input re-burns the seven
advice-defying attempts; `history-replay.md`'s transient-only rule deletes the
steering loop for a class it has never observed failing, forfeiting /goal's
core geometry ("a typed verdict from a model that didn't do the work… whose
'no' carries a steering reason back into a bounded retry loop") that CA-11 said
to steal verbatim and Tom designed the intern around. A typed verdict from a
party that is 18-for-18 on this exact judgment is the cheapest decision the
system can buy.

---

## 3. THE BABY — what must be kept for the final shape to run unattended

The final shape's standing loop has four judgment points. Who serves each, at
what tier, and what makes the judge honest:

**(1) Prepared amendment proposals — the inbox's content.** The design's
approve/deny surface is empty without them; today the "prepared diff" exists
only as diagnosis prose that a human retypes (grants glossary: "The machine can
only diagnose… **It has no verb that can act on its own conclusion. This
remains the largest single unattended-operation gap in the system.**"). Keep:
the diagnosis call, extended — `diagnosisResultSchema` gains a structured
`proposal` (amendment-task mint or gate-set fix: paths, goal text, ACs,
dependencies — content receipts 16 and 29 already contain in prose, complete);
deterministic code renders it into the worklist diff on the notification.
The lane's own structured refusal (VD-1 envelope) increasingly pre-writes the
content (F34: ε2's refusals "named the *complete* missing set on the first
refusal"), shrinking the intern's job toward packaging — the mechanization
ladder working as designed.

**(2) Exit classification.** Refusal: the worker self-declares (VD-1 envelope
— no judge needed; the refuser knows). Crash: signal-level, deterministic.
Impossible-vs-retry: the diagnosis verdict (§2), adversarial per /goal — the
worker's impossibility claim is evidence, not proof. One model slot total.

**(3) Retry-vs-stop.** The diagnosis verdict gates it (§2); deterministic
rails bound it (checkpoint tasks never get `retry`; `needs-authority` always
stops; the 2-per-input and lifetime caps are code).

**(4) Escalation/notification composition.** Already deterministic
(`action_escalate`, `actions.rs:2529`); keep verbatim (PA-43). No model.
Narration: no model (subject adoption + template, PA-25/26).

**Tier, verified against what actually ran.** The honest correction to the
assignment's premise: the record does **not** show "sonnet-grade suffices for
all of it" — the 18-for-18 diagnosis record was earned by the **worker-tier
agent adapter** (Codex default, §1a); Sonnet's only recorded outing was the
sabotaged narrator. What the record does show: the intern *function* is
harness-portable (accuracy held 15-for-15 → 18-for-18 → 18-for-18 across the
chapters-to-epsilon adapter flip, PA-32/PA-40), its inputs are artifacts rather
than transcripts, and AUGUST-01 already specifies the tier procedure: bind the
judge as a catalog role, "replay [a smaller candidate] against the … corpus and
the disagreement rate measured before any swap" (`AUGUST-01-DESIGN.md:65-68`).
The 18-receipt corpus plus archived captures now exists; run that evaluation
before any tier claim. Sonnet is the *starting* tier by ruling, not by proof.

**Adversarial-by-position, itemized** — the /goal insight is that the judge
must not be the worker, and every property is positional, none is tier:

- **Fresh session, never the author of the diff it judges** — a successor
  node, not the lane continuing.
- **Cannot write**: `diagnosisSandboxPolicy: "read-only"` by default
  (`spec-build.js:2249-2253`, `campaign_contract.rs:29`) — the Aug-1 note's
  exact demand: "judgment without write authority, enforced by sandbox policy
  rather than by asking nicely."
- **Judges artifacts, not the workman's account**: the brief is gate outputs,
  the diff, capture stderr, the task brief (`spec-build.js:3426-3472`) — never
  the worker's transcript. ("Judge the work, never the workman's account of
  it.")
- **Schema-forced and evidence-primed**: `resultSchema` + the literal
  gate-evidence substring rule derived before the model runs
  (`spec-build.js:3416-3421`).
- **Its authority is a typed verdict a deterministic layer executes** — the
  intern routes; gates still grade; merge stays mechanical ("the model never
  grades the professor's work").
- One repair to make the position match the design: **rebind the diagnosis
  slot from `effective.agent` to a steward-catalog judge role** (today Codex
  judges Codex — positionally independent but same family and worker tier;
  AUGUST-01 assigned diagnose to the steward and the implementation drifted,
  §1a). D77's principle covers it: which model answers is a host-catalog fact,
  never worklist bytes — "swapping narrators is an adapter change"
  (`~/mecattaf/dotfiles/home/tally.nix:147-149`).

---

## 4. THE CORRECTED RECOMMENDATION — the model layer, one page

**The standing substrate has three tiers, and the middle one is the intern.**
Deterministic machinery routes, schedules, merges, composes reports, formats
subjects — every judgment that compiles to code is code, and "the smallest
model is no model." **One standing model slot remains — the judge**: on every
failed attempt (and only there), a read-only, schema-forced, artifact-fed
session that (a) names cause and remedy, (b) emits the typed verdict
`retry | blocked | transient` that gates the bounded retry, and (c) when
`blocked` with an actionable authority/worklist fix, emits the structured
amendment proposal the notification renders as a ready diff. The escalation
report stays deterministic and carries the intern's diagnoses and proposal.
Humans keep exactly authoring, steer (which grants fresh input-budget),
approvals, and deploys. The frontier stays episodic: staged authoring and
summoned incident engineering. The narrator slot stays deleted; subject
adoption + deterministic formatting replace it with zero calls. The judge is a
catalog role (Sonnet first, per the standing Aug-1 ruling), rebound off the
worker adapter, downgradeable only by the corpus-replay disagreement
measurement AUGUST-01 specified — which the 18-receipt epsilon corpus finally
makes runnable. Nothing here adds a human act: the ~16-act replay of
final-shape.md §4 survives unchanged; the intern is what makes rows 4, 7, and
9 of that table ("notification + prepared diff → approve") real instead of
assumed, and it deletes most second attempts from the failure path.

**Exact deltas from `final-shape.md` as written:**

1. §3 inbox / §4 replay rows 4, 7, 9: the prepared diffs gain their author —
   the diagnosis slot's structured proposal. §5's cost table gains the line
   item (`diagnosisResultSchema` + verdict + proposal + renderer, ~+150).
2. §2 caps table, "two-attempt steering budget: keep, as the per-input
   constant" → **keep as the ceiling, gate attempt 2 on the diagnosis
   verdict** (7 of 9 epsilon second attempts ran against the machine's own
   written advice; §2).
3. §1b "What survives of VD-1 is only a structured final-message outcome
   envelope" → the envelope **plus** the impossibility hatch as a verdict field
   on the diagnosis (no new component; /goal's adversarial clause restored).
4. §4 "No model call in the merge path at all" — true and kept; add the
   explicit converse the report omits: **one model call remains in the failure
   path, by design, with a name.** The narrator-row reasoning ("0 for 35")
   gains the VD-11/VD-12/PA-26 correction so the record stops indicting the
   model for the harness.

**Exact deltas from `history-replay.md` as written:**

5. §3 / §5-delta-2 "retry within an epoch only on classified-transient faults
   (the 0-for-9 result)" → **diagnosis-gated retry** (§2). The 0-for-9 is
   confounded 9-for-9; pardon 7 is a steered retry converting; `port-worktrees`
   corrects "machinery retries fired wrongly 3-for-3" to 2-of-3.
6. D6's classifier ("crash-vs-impossible — a small-model task") → folded into
   the diagnosis slot; one slot, not two.
7. D5 (narrator deletion) — concur; correct the evidence as in (4).

**Unchanged and reaffirmed:** every structural conclusion both reports share —
input-keyed budgets, no pardons/grants/latch, retrospective certification with
the I1 deny-list, lease registration, five verbs, phone-reachable steer,
zero-resident-supervisor. This audit adds no ceremony back. It returns the one
component the record calls "still the best component" (AUG14 score table) to
the position Tom designed for it on Aug 1: **the intern was never the
supervisor, so deleting the supervisor was never a reason to delete the
intern.** The supervisor transcribed conclusions the machine had already
reached; the intern is the party that reaches them — 18-for-18 — and the
final shape runs on its verdicts.
