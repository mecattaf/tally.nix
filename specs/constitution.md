# The tally constitution

Status: **v2** (2026-08-14, supersedes v1 same day — the accepted critiques
applied; article ids are stable and never renumbered, because committed bytes
cite them). Standing consumers: every sitting run per `skills/author-spec`,
the doctrine carried by `skills/assign-tally` and `skills/campaign-operator`
(which must never contradict it), and the judge's amendment proposals, which
cite article ids. Articles cite the ruling or finding that paid for them; the
citation is the argument.

## Authority

**A1 — Authority is committed bytes.** A worklist, a spec, a gate definition,
a skill: authority is the committed blob at the admitted revision, never the
working tree, never a conversation. (D77; assign-tally: "working-tree bytes
are not authority.")

**A2 — One machine authority; the decision/rendering line.** The worklist is
the only artifact the machinery admits; its schema is closed and never grows a
key for the spec layer. The spec points at tasks; the worklist schema does not
change. Write half, absolute: the machinery never writes `spec.md` or the
worklist — ratification and every spec-byte change is a human hand at a
keyboard. Read half, the bright line: no machine *decision* — admission,
dispatch, budget derivation, merge, failure classification — takes spec bytes
as input; machine *rendering* — the escalation report, the release record,
campaign status — may resolve citations for human and judge eyes. Deny-list
scope: `specs/<armed-identity>/**` of the **governing** spec only — a campaign
whose deliverable is a spec must be able to write specs it is not governed by,
or the crystallization campaign refuses itself. (D77; R3; lens-seams Seam 4;
the zeta compile.)

**A3 — Gates are the merge criterion.** A model's opinion never is. Judgment
slots (the judge) gate *retries and proposals*, never merges. (Aug-1 design;
E5's correction changed the harness verdict, not this law.)

**A4 — Main advances only through a campaign merge.** No exceptions,
including the operator; publish carries zero judgment on the record, so the
proven re-gated head fast-forwards automatically. (Merge control option B; R1.)

## Shape

**A5 — A campaign is state, not process.** Desired state is the worklist DAG;
actual state is durable completion facts; execution is short stateless
reconcile passes. (#253; AUGUST-01.)

**A6 — One lane = one agent = one worktree = one witnessed commit.** Lanes
run concurrently only where conflict domains are disjoint; the domain
declaration is the binding constraint, not an authored cap. (Caps replay: no
authored cap ever fired correctly.)

**A7 — Two surfaces.** Process lives in the journal; outcomes live on the
forge. Nothing durable is named after a registration. (AUGUST-01; E4.)

## Authoring

**A8 — The placement law.** Every mechanism added must delete operator rules.
Anything that asks the operator to tend a new artifact is forbidden — this
spec layer included: it exists to retire the day-doc sprawl and the
scratchpad ledgers, and must be judged by what it deletes. (D58.)

**A9 — The worker-context law.** A task's `goal` plus `readFirst` is the
worker's entire context; every pointer names a file existing at the authority
revision. Requirement and evidence IDs are citable only once their documents
are committed. (D68; the 48 phantom pointers.)

**A10 — Executable acceptance.** No operational requirement lives only in
prose. Ordering is dependencies, invariants are gates, ownership is path
domains, acceptance is argv. A criterion that cannot name its oracle is
`[HUMAN-ATTENDED]` by declaration, not by discovery. (assign-tally; the D13
pilot: "oracle presence, not effort, decided what could complete.")

**A11 — The ownership law.** A task owns every file its change makes false.
The authoring question is "which existing assertions does this make wrong,
and does it own them"; take the machine's enumerated list verbatim. (F22–F26;
H2 scored 0-for-4 trying to lint this textually.)

**A12 — Author against the observed tree.** Never a predicted one. The edge
census runs at the same sitting that authors the stage. (F42; the ε2 census.)

**A13 — No later, no calendar.** A spec freezes one scope; build order
exists, softer scope does not — no deferred lanes, no dated milestones.
Unauthored stages are ordered scope, not parked scope. (Agency doctrine,
adopted; it is F42's discipline stated from the other side.)

**A14 — Absence over prohibition.** An excluded surface receives no
requirement, no task, no code path — and no further words. (Agency doctrine,
adopted.)

## Verification

**A15 — Every verification artifact joins a standing gate or is deleted.** A
conformance bar not executed by a gate is not a bar; it rots silently at the
rate the code moves — the unexercised-contract failure relocated into the
test suite. The same applies to every artifact of the spec layer. (The
grind's §4 commitment, failed; VD-5, F33; `flake.nix` `--list`-only harness.)

**A16 — Dual derivation for contracts of consequence.** Contracts of
consequence get dual blind derivation; disagreements escalate as spec
defects, never absorbed. Procedure: `skills/author-spec`. (aug9-pass; the
armedManifest catch.)

**A17 — Record, don't fix.** Frozen inputs are recorded, never edited.
Procedure: `skills/author-spec`. (Agency D13; grind rule: "do not touch the
test.")

**A18 — Attribute against the deployed store path.** Before crediting or
blaming any merged commit, diff what was actually grading — the frozen-flow
rule and the stale-pin rule are one law. (VD-13, twice in one document.)

## Operation

**A19 — Human gates at boundaries only.** Ratification and stage authoring
are operator acts; nothing mid-run waits on a human. A transcription act — an
operator act whose text the system had already printed — is a defect of the
machinery, not a duty of the operator. (CA ledger; ext0's close condition.)

**A20 — The judge is adversarial by position.** Read-only, artifact-fed,
schema-forced, never the author of what it judges; its tier changes only by
corpus-replay measurement, never by impression. (Intern ruling; Aug-1
procedure.)

**A21 — Disarm is terminal.** Never failure recovery. Recovery is new input:
a steer or an amendment refreshes the epoch by derivation. (F17;
epoch-scoped-budgets.)

**A22 — The freeze/append article.** A ratified `spec.md` admits exactly one
class of in-file change: Status transitions (ratified → closed). Beside it,
exactly two artifact classes may grow: appends to `trace.json` (structural
prefix rule — old rows are a byte-stable prefix of new rows; sitting rows by
the author's hand at a sitting; release rows machine-rendered and
human-committed, machine-appended only when the release verb owns it), and
additions under `evidence/`. Nothing else. Any other diff under a ratified
`specs/<identity>/` is a defect. (The v1 self-contradiction — README v1 froze
spec.md while housing per-stage trace rows in its §7, lens-seams Seam 6; E7's
freeze genre; A21's terminal-state logic at spec altitude. Consumer: lint L17
and the sitting checklist.)

---

*Candidates for the tally-crystallization sitting, recorded and not applied
(00-INDEX accepted five critiques; these were not among them): A5–A7 moving
into tally's own crystallized spec; A16–A18 and A20–A21 shrinking to their
standing consumers. (lens-seams §Constitution critique.)*
