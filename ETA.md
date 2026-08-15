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
    The orchestrator keeps a running spend ledger against the weekly window.
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
commit and confirms the pin-bump; (4) rebuild switch + drop-in/state
verification (until S1 deploys at pin-bump 1, re-verify the 24 GiB drop-in
after every rebuild); (5) orchestrator resumes dispatch. The `campaign
status` view is authoritative only for the reconciled past —
`systemctl --user list-units 'tally-job-*'` is liveness (close report §3.7).

## 6. Budget plan (the qwen week)

Window: the weekly metered allocation, from Phase-0 close. Estimates at
300–400k fresh input per lane attempt: Chapter 1 (6 lanes) + Chapter 2
(5 lanes) ≈ 3.3–4.4M first-attempt, ~5–6M with a 1.3× retry factor;
Chapter 3 (7 lanes) ≈ 2.1–2.8M. The window plausibly covers Chapters 1–3
**iff** masquerade-driven retries stay near zero — which is what Phase 0's
machinery and rule E6 exist to guarantee. Chapters 4–5 ride the next window,
claude-code, or codex post-Aug-20 (Tom's §8 call). The orchestrator posts
the spend ledger at every seam.

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
