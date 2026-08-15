# ETA — the daily-driver buildout: one campaign to a usable tally

Written 2026-08-15 by the Fable meta-orchestrator session, at the operator's
instruction, with the full ext0 record in hand. **This file supersedes the
piecewise plans**: ZETA.md's operator acts A5–A9 (the zeta *task specs* in
ZETA.md remain the verbatim authoring source and are referenced, not copied),
the vestige ledger's standalone A10 packaging (its content is absorbed as
Chapter 1), and the open-items lists in `main-thread-ext0-close.md` §6 and
`AUG15-SESSION-FINDINGS.md` §3 (every item is mapped in §0 below). Standing
consumers: the Phase-0 handoff session; the eta sitting commits; the
supervising orchestrator for the whole buildout.

Eta in one line: one bootstrap gate to a fresh working tally, then one
continuous campaign — substrate, authority plane, ext1 verbs, productization,
proof — until tally is a daily-drivable, harness-orthogonal orchestrator, at
which point (and only then) v0.0.1 is minted.

---

## 0. Supersession map

| superseded item | where it lives now |
|---|---|
| ext0 interim close + release (A5) | dissolved — ext0 is disarmed in Phase 0; its nine lanes re-enter by cherry-pick under the Phase-0 gate; ext0 never "closes" |
| boundary deploy (A6) | Phase 0 pin+flash |
| zeta sitting (A7), arm (A8), close (A9) | eta sitting 0 + Chapter 2 + seam C1 |
| A10 substrate-repair act (ledger part 3) | Chapter 1, verbatim task groups |
| ext1 sitting/program | Chapter 3 |
| ext2 completion contract + judge replay | Chapter 5 |
| dotfiles deploy trap (§3.6) | Phase 0 step 6 (pin is committed, permanently) |
| memory-cap module option (§6.4 of close report) | rejected per V-1 disposition — Chapter 1 deletes the cap instead; 24 GiB drop-in is the bridge |
| acceptance-argv ↔ conflictDomains check (§3.3) | Chapter 3 task X3 |
| OOM classification (§3.2) | Chapter 1 task S2 |
| learnings-to-enforcement audit (§2.5) | eta sitting 0, opening step |
| baseline-parity law (§2.6) | substrate spec claim + Chapter 4 task D3 |
| v0.0.1 blessing (portrait appendix) | §7, unchanged in substance, ratchets restated |

Nothing on any prior list falls off; anything not named above was checked and
either absorbed into a chapter below or deliberately dropped with its
replacement named.

---

## 1. Rulings (E1–E8)

- **E1 — One campaign.** All remaining buildout work runs under a single
  identity, `eta`, armed once. Chapters are dependency braids inside one
  worklist, not separate campaigns. Later chapters are refined by worklist
  amendment on `main` before their tasks are admitted (the amendment path is
  proven — it worked all of ext0).
- **E2 — One bootstrap gate.** Phase 0 replaces ext0's unreachable chapter
  gate with a single witnessed bar: assembled tree passes
  `test/fleet-gate.sh` + final bar locally, then pin + flash. After that,
  ordinary per-merge gates and seam checkpoints carry the burden.
- **E3 — Pin-bumps at seams are cheap and sanctioned.** A seam pin-bump is:
  commit the dotfiles pin to the new rev, rebuild switch, verify. No fleet
  timer, no ceremony. The nightly deploy timer stays stopped for the whole
  buildout; deploys are deliberate seam acts.
- **E4 — Hygiene ruling.** Records live under `specs/<identity>/evidence/`
  or in git history (commit messages, tags). No new loose record files at
  repo root; no committed `.diff`/patch files — that is what git is for.
  Existing root day-docs are grandfathered until the Chapter 5 cleanup.
- **E5 — Token policy.**
  - Gates: deterministic, zero model tokens.
  - Lane implementors: the metered qwen plan (pi adapter), **one worker at a
    time** (two only for provably disjoint cheap tasks), low/medium effort —
    the pre-digested goal is the capability amplifier.
  - Diagnosis, judging, sittings, escalations, all authoring: never on the
    metered plan. These are the orchestrator's (Fable) and claude-code's.
  - A lane attempt is budgeted at 300–400k fresh input (measured, Aug 7).
    The orchestrator keeps a running spend ledger against the weekly window
    and dispatches under the §6 protocol — no dispatch without a spend check,
    no fan-out before calibration, hard reserve floor.
  - Crystallization (Chapter 1): spend into receipts, effort tier into the
    host catalog, metered-pool capacity=1 into the pool declaration.
- **E6 — Read the capture tail before any retry burns.** Until S2/S5 deploy,
  every empty-stderr agent fault and projection timeout gets its capture
  tail read first (`~/.local/state/tally/capture/`), and
  `systemctl --user show <unit> --property=Result` checked for `oom-kill`.
  This is the budget guard, not just a diagnostic courtesy.
- **E7 — Operator surface.** Tom is asked for exactly three kinds of act:
  ratification commits at seams, pin/flash confirmations, and the rulings
  that are his (§8). Everything else is the orchestrator's.
- **E8 — Adapter facts.** codex is unavailable until Aug 20 05:29. On the
  deployed contract, a policy-less worklist renders codex literals and dies
  at `render_policy` for **every** non-codex adapter (V-15) — so until S4
  merges *and* deploys, eta's worklist must explicitly null
  `approvalPolicy`, `sandboxPolicy`, `diagnosisSandboxPolicy` for whichever
  adapter it binds (pi and claude-code both need this). Verify at admission
  rehearsal that the rendered argv carries no codex vocabulary.

---

## 2. Phase 0 — bootstrap (separate handoff session; summary here)

Steps, in order: (1) record commit on `main` — all untracked session files
plus this file; (2) disarm ext0 (terminal); (3) cherry-pick the nine ext0
lane commits from the integration branch onto `main`, oldest first; (4) run
the bootstrap gate: `language-entry-policy` check cheap-first, then full
`test/fleet-gate.sh "$(git rev-parse HEAD)"` + `test/final-bar/run "$PWD"`;
(5) push `main`; (6) pin the new rev in dotfiles, **commit and push
dotfiles**, rebuild switch; (7) re-verify the 24 GiB daemon drop-in survived
the rebuild (recreate if not) and smoke the deployed tally
(`tally adapter smoke claude-code --assert-commit --pool campaign-agent`);
(8) write `specs/eta/evidence/phase0-report.md`, commit, push. Stop
conditions and the full runbook are in the handoff itself.

Phase 0 exit state: `main` = ext0's machinery assembled and gate-proven;
deployed pin = that rev; both timers stopped; no campaign armed; qwen window
untouched.

---

## 3. Campaign `eta` — header and admission notes

- Identity `eta`; worklist `silent-factory-worklists/eta.json`; spec home
  `specs/eta/` (spec.md authored at sitting 0, linted once Z1–Z3 land —
  same first-contact posture zeta ruled for itself).
- Header: `schemaVersion: 1`, `maxTasks: 40`, `maxParallel: 1` (E5; raised
  to 2 by amendment only for disjoint cheap pairs), gates: the four template
  gates verbatim from `epsilon-extension.json:8–37` (`driver-suite`,
  `cargo-tests`, `clippy`, `flake-build-subset`).
- `campaign.agent`: bound to the pi (qwen) adapter for lane work, with the
  three policy keys explicitly nulled (E8) until S4 deploys. claude-code
  (also policy-nulled) is the standing fallback pool when the metered window
  is the constraint. No model name in worklist bytes — occupancy stays a
  host-catalog fact.
- Full zeta-style goal texts are authored per-chapter at seam sittings by
  the orchestrator, from the named sources (Chapter 1: the vestige ledger's
  A10 package verbatim; Chapter 2: ZETA.md task specs verbatim; Chapters
  3–5: drafted at their seams against the then-observed tree). This file
  fixes structure, domains, and dependencies; it does not duplicate goals.

---

## 4. Chapters

### Chapter 1 — substrate repair (source: vestige ledger part 3 + addendum)

| id | scope | conflictDomains | deps |
|---|---|---|---|
| S1 | delete the job memory cap; limits become optional (V-1, V-12) | `crates/tally`, `crates/tally-core/src/executor`, `nix/modules` | — |
| S2 | OOM legibility: probe + classification + failure fact names the cap (V-3) | `crates/tally-core/src/executor`, `crates/tally-core/src/daemon` | S1 |
| S3 | widen excerpt peepholes as derivations; error-aware excerpting (V-4, V-5) | `crates/spec-build-driver`, `examples/flows` (+`captures.rs` via S2's domain) | S2 |
| S4 | policy defaults resolve adapter-relatively; diagnosis mechanism → workspace-write; portability matrix test (V-2, V-15) | `crates/tally-core/src/campaign_contract.rs`, `examples/flows`, `nix/lib/adapters.nix`, driver tests | — |
| S5 | adapter-terminal outcomes (quota walls name themselves) + token spend scraped into the outcome envelope (V-16 + E5 crystallization) | `nix/lib/adapters.nix`, driver classification, `test/fixtures/traces` | S4 |
| S6 | substrate-numerals flake check; finish python-driver deletion; STORE_CHECK_TIMEOUT widened with ruling; RPC 60s ruled, flake call-site patch deleted (V-6, V-7, V-13, V-14) | `flake.nix`, `nix/modules`, `drivers`, `crates/tally-core` misc | S1 |

Sitting-side at seam C1 (operator commit): `specs/substrate/spec.md` — every
retained constant as a claim with provenance; the baseline-parity law as a
claim; catalog edits (effort tiers, metered-pool capacity=1). V-17's `Sonnet`
citation in `epsilon-extension.json` is regenerated at the first sitting that
touches that file.

### Chapter 2 — authority plane (source: ZETA.md task specs, verbatim)

| id | scope | conflictDomains | deps |
|---|---|---|---|
| Z1 | `spec-lint-core` | `crates/spec-lint`, `Cargo.toml`, `Cargo.lock` | — |
| Z2 | `spec-lint-resolution` | `crates/spec-lint` | Z1 |
| Z3 | `spec-lint-flake-check` | `flake.nix` | Z2, S6 |
| Z4 | `spec-layer-skills-amend` | `skills/assign-tally`, `skills/campaign-operator`, `skills/author-spec` | Z3 |
| Z5 | `doc-anchor-regrammar` | `doc` | — |

ZETA.md compiler rulings Z1–Z7 stand unchanged. DECISION-1 (steward) and
UNKNOWN-1 drain at eta sitting 0.

**Checkpoint C1** — fleet-gate + final bar over Chapters 1+2; then seam
sitting 1 (ratify substrate + zeta specs — now machine-lintable — append
trace rows, run the learnings-to-enforcement audit if not done at sitting 0)
and **pin-bump 1**. From here: failures name themselves, the spec layer
grades every gated head, and eta amends Chapters 3–5 to cite real
`specs/**` anchors — the campaign upgrades its own governance mid-flight.

### Chapter 3 — ext1 verbs

| id | scope | deps |
|---|---|---|
| X1 | `publish-as-a-machine-stage`: content-disjoint rebase of a campaign base + re-gate of the rebased head — the wedge-class killer | C1 |
| X2 | poll re-admission: a push to the armed identity's worklist is the arming act | X1 |
| X3 | acceptance-argv ↔ conflictDomains lint (every path an acceptance argv touches is inside declared domains) — in spec-lint + admission | C1 |
| X4 | R3 scoped deny-list: `specs/<armed-identity>/**` mechanically unwritable by lanes | X3 |
| X5 | gate budgets derived from receipts (observed duration × slack); V-6's GUESS numbers retired | C1 |
| X6 | the lease | X2 |
| X7 | the inbox: typed-doubt queue as a delivery surface (E8 of the epsilon program) | X2 |

**Chapter 3 authoring amendment (operator ruling, 2026-08-15):** the
inherited "authored caps off the schema" destination
(EPSILON-EXTENSION.md:106; final-shape.md:264,:493) is amended for
`maxParallel` — **the per-worklist concurrency knob is never deleted.**
It becomes *optional* (absent ⇒ host pool capacity governs), satisfying
the zero-required-numbers doctrine, with min semantics when present:
effective width = min(worklist `maxParallel`, host pool capacity,
disjoint-domain frontier). Desired concurrency is campaign policy (e.g.
budget containment), not only a host property — the Aug 15 budget week is
the proof case. The `maxTasks`/`projectionWaitMs` dispositions are
unaffected. Evidence files stay untouched (record-don't-fix); this ruling
is the authority the C2-era sitting authors from.

**Checkpoint C2** → seam sitting 2 → **pin-bump 2**. From here a base fix
reaches a running campaign and a push arms work — the failure mode that
ended ext0 is structurally extinct.

### Chapter 4 — daily-driver productization

| id | scope | deps |
|---|---|---|
| D1 | product split: tally installable as a versioned flake app/profile, decoupled from the dotfiles fleet pin; using tally on repo X requires no fleet deploy | C2 |
| D2 | lightweight worklist path: minimal template + scaffold verb for ordinary work on non-tally repos — no spec plane, no citation apparatus required | C2 |
| D3 | baseline-parity probe: bare-vs-laned parity as a standing check in the smoke genre ("the harness does not fight the agent" becomes witnessed) | C2 |
| D4 | docs true-up for the product path (install, init, small-worklist lifecycle) | D1, D2 |

**Checkpoint C3** → seam sitting 3 → **pin-bump 3**.

### Chapter 5 — proof and v0.0.1 preparation

| id | scope | deps |
|---|---|---|
| P1 | ext2 unified completion contract: release renders coverage from durable facts | C3 |
| P2 | judge-tier corpus replay — run it, on the real spend numbers S5 has been accumulating; tier decisions become empirical | C3 |
| P3 | the Aug 3 cleanup residue: comment sweep + shim/vestige excision not already discharged by Chapter 1; root day-doc migration under E4 | C3 |
| P4 | final chapter gate over the whole campaign | P1–P3 |

---

## 5. Seam protocol

At each checkpoint (C1/C2/C3/P4): (1) checkpoint node green; (2) orchestrator
drafts the seam sitting — spec ratifications, trace rows, amendments to the
next chapter's tasks against the observed tree; (3) Tom makes the sitting
commit and confirms the pin-bump; (4) rebuild switch + the post-flash
checklist learned at Phase 0, mandatory at EVERY pin-bump: (a) the flash
re-arms declared automation — stop `tally-campaign-poll.timer` (unless a
campaign should poll) and `tally-producer-nightly-fleet-deploy.timer`,
and clear any benign `fleet-deploy.service` failed state
(`sudo systemctl reset-failed fleet-deploy.service`) — standing until a
witnessed quiescence guard lands; (b) the 24 GiB drop-in pins a full
ExecStart INCLUDING the store path — re-point it at the new deployed path
and restart the daemon (standing until S1 deploys, after which the
drop-in is deleted outright); (5) orchestrator resumes dispatch.
Hardening candidate carried from Phase 0: the #440
`launch-cwd-ordinary-completion` continue-vs-sessionRef race (flaked once
at the bootstrap gate, passed on rerun) — queue under Chapter 5 P3. The `campaign
status` view is authoritative only for the reconciled past —
`systemctl --user list-units 'tally-job-*'` is liveness (close report §3.7).

## 6. Budget plan (the qwen week) — governed by the Aug 7 lesson

**The lesson, restated as law.** On 2026-08-07, four parallel qwen workers
consumed the plan's entire weekly allowance — 1,373,257 fresh input tokens —
in ~18 minutes (766 turns; every tool output lands as fresh input; cache
reads are unmetered and irrelevant; ~300–400k fresh per lane; the failure
was discovered only after fan-out, so it cost 4× instead of 1×). Nothing
misbehaved; that is simply the autonomous-implementation regime. Interactive
intuition does not transfer and is never used for sizing here.

**Discrepancy resolved (2026-08-15, live forensics on
`~/.pi/agent/sessions/` — supersedes AUGUST-07-LEARNINGS §3's metering
interpretation).** There was never a 10M-token weekly plan. QwenCloud runs
two separate billing rails: the **free/PAYG rail** (`sk-ws-` key, standard
endpoint, ~1M-token free buckets per model) and the **Token Plan rail**
(`sk-sp-` key, base URL `token-plan.ap-southeast-1.maas.aliyuncs.com`
`/compatible-mode/v1`, 10,000 credits per 7-day window). The Aug 7 wave
ran on the **plan rail** — pi's provider config (`qwen-token-plan`,
key at `/run/agenix/qwencloud-token`, prefix `sk-sp-`) proves it, and the
lane's terminal error says it verbatim: *"Your token-plan 1-week quota has
been exhausted. The quota will reset at 08-14 10:06:00 UTC."* The paid
week WAS spent — in ~18 minutes, by four parallel lanes. The free rail has
seen exactly one 86-token manual test (Aug 6), so the free buckets are
intact. The window has since reset: a full 10,000-credit week is available
now.

**The real cost driver, corrected:** the Aug 7 record's "cache reads —
unmetered" was wrong on this rail. At cache ≈10% of the input rate, the
wave's 112.4M cache reads were ~73% of the credit burn (est. ≈9,000 of
≈12,300 credits at scaled rates, against the 10,000 cap). **Transcript
length is the cost center**: long lanes re-send everything every turn.
Binding consequences — small tasks, lane turn-review threshold (~80 turns
without a push ⇒ supervisor reviews instead of letting it grind),
implementor thinking level low/medium (output is the priciest token class,
≈6× input), and serialization so a surprise costs one lane.

**Credit economics (doc-derived; treat as estimate until calibration).**
Credits scale with each model's PAYG price. From the official qwen3.6-plus
worked example: input ≈200 credits/M, cache reads ≈10% of input rate,
output ≈6× input. Scaled to qwen3.8-max's $2/$6 PAYG pricing: input
≈800/M, cache ≈80/M, output ≈4,800/M (inference, unverified). Applied to
a measured ext0-shape lane (350k fresh in, ~27M cache reads, ~115k out):
**≈3,000 credits/lane — and cache reads are the dominant term (~70%)**,
because agentic transcripts re-send every turn. Two consequences: the
weekly 10,000 credits fund roughly 3–4 ext0-shape lanes, not dozens; and
**short lanes are the strongest cost lever on the plan rail** — cache-read
volume grows superlinearly with turn count, so a task half the size costs
well under half the credits. Credit Packs ($15/mo per 20,000 credits, up
to 5, subscribers only) are the sanctioned overflow, ≈ list-price value.

**Account configuration.** The plan rail is ALREADY wired and proven:
pi provider `qwen-token-plan` → `sk-sp-` key at
`/run/agenix/qwencloud-token` → token-plan base URL, with qwen3.8-max and
deepseek-v4-flash-0731 declared. Nothing to fix there. What does NOT yet
exist is a free-rail provider: pi has no `sk-ws-` key configured, so the
"free quota" buckets are ruled playground-only (see the free-tier ruling
above) — no free-rail provider is configured or planned. The metered
budget is plan credits + optional credit packs. Plan-rail caveats: the weekly window resets on
its own schedule (observed: reset stamped 7 days after first invocation);
no programmatic usage endpoint exists and the console lags — the per-lane
ledger reconstructs from `~/.pi/agent/sessions/` usage records (method
validated against the Aug 7 numbers exactly), console checked at seams.

**Dispatch protocol (binding on the orchestrator):**
1. **Metering verification first.** Before any lane dispatches, read the
   plan's quota surface (pi usage records / provider dashboard) and record:
   what is metered (fresh input vs input+output vs everything), the true
   weekly number, and the reset time. Written into the ledger before lane 1.
2. **Calibration lane.** S1 dispatches SOLO as the first qwen lane. At its
   close, reconstruct actual burn from pi's usage records (the Aug 7 method)
   and compute: burn per lane, projected chapter cost, lanes-per-window.
   The rest of the schedule is derived from that number, not from estimates.
3. **One worker at a time — no exceptions this week.** Serialization is the
   containment: any surprise costs one lane, never four. (E5's "two for
   disjoint cheap pairs" is suspended until calibration and Tom's explicit
   ok.) Enforcement is committed worklist bytes, not discipline:
   `eta.json` `maxParallel: 1`, raised only by worklist amendment after
   calibration. **Operator ruling (2026-08-15): the flow's declared
   concurrency knob (`maxParallel`) is kept permanently** — the
   final-shape note about deleting it in favor of pool capacity is
   overruled; host pool capacity remains the ceiling above it, and V-8's
   only change is making that default's derivation visible.
4. **Spend check before every dispatch.** Remaining allowance is read before
   each lane starts. A lane never dispatches into a window that cannot fund
   it plus one retry (~2× lane cost).
5. **Hard reserve floor: 15%** of the window is never dispatched against —
   it exists so diagnosis-driven reruns and the odd oversized lane don't
   dead-end the week the way Aug 7 and Aug 14–15 both did (both runs ended
   on exhausted quota).
6. **No retry without cause.** A failed qwen lane is never redispatched
   until the capture tail and unit result are read (E6) and the orchestrator
   names the cause. Terminal conditions (quota, OOM) stop the ladder — a
   retry against a wall is the single most expensive no-op in the record.
7. **Qwen never evaluates.** Diagnosis, judging, verdicts, and all
   read-everything roles stay off the metered plan, categorically (the
   evaluator re-reads the whole surface every round — worst possible
   token-flow shape for metered capacity).
8. **Ledger cadence: per lane, not per seam.** After every lane: spent,
   remaining, lanes-left-at-current-burn, posted where Tom sees it.
9. **Small lanes are the cheapest optimization.** Chapter tasks are scoped
   to 1–2 conflict domains and tight readFirst sets; ingestion, not
   thinking, is what the meter charges.

**Model roster ruling (operator, 2026-08-15).** Recorded here as the
ruling's record; encoded as host-catalog fact at sitting 0 (which model
answers is a catalog fact, never worklist bytes — L16). The metered plan is
QwenCloud Token Plan Standard: 10,000 credits / rolling 7 days (clock
starts at first plan-rail invocation; ≈3,000 credits per ext0-shape lane
on qwen3.8-max, estimate pending calibration), plus per-model free quotas
on the separate free rail that auto-stop at exhaustion.
- **Tier A — lane implementors** (plan credits): qwen3.8-max (flagship;
  list $2/M in, $6/M out, 1M ctx, 131k max out), qwen3.8-2.4t-a95b
  (open-source sibling), deepseek-v4-pro-0813.
- **Tier B — bounded closed-shape utility slots ONLY, and only if a
  genuinely free, API-reachable quota separate from plan credits is ever
  proven to exist:** qwen3.7-plus, glm-5.2. Never lane implementors,
  never evaluators.
- **Excluded:** everything else in the free-tier roster (image models et
  al.).
- **Free-tier ruling (operator, 2026-08-15): the per-model "free quota"
  buckets are treated as the try-ai webchat playground allowance, NOT
  API-usable compute.** No free-first stage; the metered budget is plan
  credits + credit packs, with claude-code as overflow. Cheap
  falsification if ever wanted: create the `sk-ws-` key, make one tiny
  API call, see whether the free bucket decrements — ten minutes, zero
  risk; until then the assumption stands. Parallelism above one worker
  only after calibration, within the plan's 3–4 agent concurrency.
- **List-price anchor (sanity only, until calibration):** a measured lane
  (300–400k fresh in, ~100–140k out) ≈ $1.2–1.6 at qwen3.8-max list;
  Chapters 1–3 (~23 attempts) ≈ $30–45 at list. What fraction the weekly
  10,000 credits cover is exactly what the calibration lane measures.

**Operator directive (2026-08-15): the qwen subscription carries the FULL
buildout, Chapters 1–5, at ZERO additional spend.** No Credit Packs, no
pay-as-you-go, nothing beyond the active $51/quarter subscription.
claude-code is contingency only (adapter smoke, emergency continuation if
the plan rail is down); codex optional after Aug 20. The available
budget, netted out: 10,000 credits per 7-day window, windows recurring
until 2026-11-07 ≈ **~12 windows ≈ ~120,000 credits already paid for**.
Against the buildout's ~30–35 attempts at small-lane discipline
(~1,000–1,500 credits/lane ≈ 40–55k credits total), the subscription
covers the whole program with roughly 2× margin — the binding resource is
windows, not money. When a window exhausts, dispatch pauses until it
resets; that pause is a planned state, not an incident.

**Model routing inside the plan rail (the biggest "use it WELL" lever):**
credits scale with each model's PAYG price, so the catalog routes by task
weight — mechanical/small lanes (salvage-class, doc edits, skill amends,
fixture work) → deepseek-v4-flash-0731 (already configured in pi;
flash-class credit rate is a small fraction of max-class); standard
implementation → deepseek-v4-pro-0813; the genuinely hard lanes
(spec-lint-core, publish-as-a-machine-stage, product split) →
qwen3.8-max. The spec layer is the capability amplifier that makes the
cheaper tiers viable (pre-digested goals, verbatim contracts); S1
calibrates qwen3.8-max, and the first flash-routed lane calibrates the
cheap tier the same way.

**Sequencing (no wall-clock commitments — the dependency order and the
ledger govern, not calendar estimates):** Phase 0 → sitting 0 → the
chapters run in DAG order, continuously, one lane at a time, for as many
weekly windows as they take. The plan week starts at Chapter 1's first
dispatch. Chapter boundaries are seam events (checkpoint → sitting →
pin-bump), not dates. The buildout is complete when P4 is green and the
§7 proof ratchet holds — full completion on the qwencloud subscription is
the aim, and the ~2× credit margin says the subscription can carry it;
S1 and the first flash-routed lane convert that from estimate to
measurement, and the per-lane ledger is the only schedule authority.

## 7. Exit — the ratchets, then v0.0.1

Eta's exit criterion is the daily-driver ratchet from the v0.0.1 blessing,
verbatim in spirit: not one good session but a proven daily driver — a week
of ordinary, boring, **non-tally** work (sodimo-final class) flowing through
the D1/D2 product path as ordinary campaigns, with zero hand-performed
recoveries. Then, outside eta and unchanged from the blessing appendix: the
history cut — cleaned tree becomes v0.0.1's root commit, minted *by* tally
via a witnessed campaign, released through `tally campaign release` against
its own coverage. Evidence under `specs/**` survives the cut; forge-history
`#NNN` referents die with it — which is why the Phase-0 record commit and E4
matter now.

## 8. Open decisions (Tom's, with due dates)

1. **Seam ruling** (the one architectural decision the Aug 3 plan reserved
   to the operator) — due before P3.
2. **R4** — the one-word `forge:"local"` remote-semantics ruling — due
   before X1 is authored.
3. **DECISION-1** (steward field) and **UNKNOWN-1** — drained at eta
   sitting 0, per ZETA.md.
4. **Adapter mix after Aug 20** (codex returns; new subscription or not) —
   due at seam C2.
5. **Whether metered small-model workers become a standing pool** after this
   week's evidence (the synthesis doc's open item) — due at P2, decided on
   P2's numbers.
