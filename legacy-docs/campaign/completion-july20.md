# COMPLETION HANDOFF — Waves 12 and 13 (2026-07-20)

You are the sole writer for the remainder of this build. This file plus
`docs/CODEX-HANDOFF.md` are your handoff. Where this file and any `docs/` file disagree,
**this file wins** — it exists specifically to correct frozen prose that instructs you to
build forbidden scope.

The context fence in `docs/CODEX-HANDOFF.md` §0 still applies in full: do not read GitHub
issues, and do not read `~/mecattaf` plans, notes, or steering documents. Everything you
need has been transcribed here.

**Resume check, every session:** `git log --oneline`, `git status`, `WAVES-STATE.md`, then
`docs/CODEX-HANDOFF.md` §1 and §3, then this file. Assume no conversational context
survived.

---

## 1. Where the build stands

Committed and DONE through Wave 11.5:

| Wave | Unit | Commit |
|---|---|---|
| 0–10 | BS-0 … BS-10 | through `306b037` |
| 11 | BS-11 producers registry | `63af547` |
| 11.5 | remove `r2` from scope and specs | `27860bc` |

190 ordinary tests pass; six live-system tests are ignored in the ordinary suite. The
working tree carries exactly four expected untracked files: `docs/CODEX-HANDOFF.md`,
`docs/PRE-BUILD-ADDENDUM.md`, `wave-11-5.md`, `wave-log.jsonl`. Leave all four untracked.

Remaining: **Wave 11.6** (below — new, small), **Wave 12** (BS-12, heaviest), **Wave 13**
(three scenarios). There is no Wave 14. Full BS-13 and the rest of BS-14 are deferred and
ruled; do not resurrect them.

---

## 2. SCOPE CORRECTION — read this before opening BUILD-SEQUENCE.md

`docs/BUILD-SEQUENCE.md` and `docs/NIX-SPEC.md` were written before the scope cut. They
instruct you to build three subsystems that the scope law forbids. `docs/CODEX-HANDOFF.md`
§1 already overrides them —

> "the initial module accepts only `enforce = cooperative`. dmem-related values and all
> other deferred fields are rejected as unknown/invalid, with no placeholder branches."

— and §0 states the closed overrides "take precedence over conflicting older prose in the
references." This section names the conflicting prose explicitly so you cannot transcribe
it by accident.

### Struck from BS-12. Do NOT build any of it.

- **`enforce = "dmem"`** — the enum accepts `cooperative` only. Not `dmemcg-booster`,
  not `dmem`. The Rust side already rejects `dmem` outright.
- **`servingSlice`** — no option, no `Delegate=yes` wiring, no worker-side cgroup write.
- **The patched-systemd overlay** — no `DeviceMemoryMax`, no overlay, no
  `dmem.subtree_control`.
- **`remote` / `remoteSubmodule` / `remoteHeartbeatSec` / `remoteReapSec`** — RemoteLease
  and cross-host re-adoption are deferred. No remote pool addressing.

Their **absence is correct**. Do not add placeholder branches, feature flags, reserved
enum slots, or "rejected for now" stubs. Rejecting an unknown option because it was never
declared is the correct mechanism — not declaring it and then refusing it.

### Specific contaminated locations, superseded

| Location | What it says | Correct action |
|---|---|---|
| `BUILD-SEQUENCE.md:342` | scope includes `servingSlice` | omit the option entirely |
| `BUILD-SEQUENCE.md:344-345` | auto `Delegate=yes` for `enforce = dmem` and any `servingSlice` | build no `Delegate` wiring |
| `BUILD-SEQUENCE.md:346` | "the patched-systemd overlay" | build no overlay |
| `BUILD-SEQUENCE.md:356` | acceptance: "A worker `servingSlice` gets `Delegate=yes`" | **struck** — see corrected acceptance in §4 |
| `NIX-SPEC.md:18-19` | nixosModule owns `Delegate=yes`, overlay, `dmem.subtree_control` | nixosModule owns `StateDirectory`/`LogsDirectory` and the system daemon only |
| `NIX-SPEC.md:157` | `patchedSystemd` option | omit |
| `NIX-SPEC.md:188, 192` | `resource`/`enforce` enums including dmem | `enforce` = `[ "cooperative" ]` only |
| `NIX-SPEC.md:196-197` | `remote`, `servingSlice` options | omit both |
| `NIX-SPEC.md:208-215` | `remoteSubmodule` | omit |
| `NIX-SPEC.md:241-303` | §2.1 dmem production setup, §2.1a remote enforcement | out of scope in full |
| `NIX-SPEC.md:497-499` | §6 conventions rows 1–3 (dmem local, dmem remote, servingSlice) | **must generate nothing** |

### Two further defects in BS-12's written acceptance

1. `BUILD-SEQUENCE.md:358` says "Every option in §1–§10". **`NIX-SPEC.md` has §0–§9.**
   Read it as §0–§9, minus everything struck above.
2. `BUILD-SEQUENCE.md:354` says "Every conventions row terminates in a generated
   artifact." This is **unsatisfiable as written** — rows 1–3 are deferred surface.
   Corrected in §4.

### The rule to carry

The producer enum has exactly five kinds: `calendar`, `build-effect`,
`pool-reachability`, `gh`, `events-dir`. External object-store intake is **permanently**
out (Wave 11.5), subsumed by `events-dir`. The two Rust rejection tests
(`crates/tally-core/src/producers.rs`, `crates/tally-core/src/config.rs`) must remain
unchanged in behavior. Do not add a sixth kind or a reserved slot.

---

## 3. Wave 11.6 — complete the E1/E2 admission path

**Do this before Wave 12.** It is small, it is a closed ruling, and it must land before
BS-12 types the enqueue submodule — afterwards it becomes a module-schema change too.

**What is wrong.** `evidence_class` (E1) and `manifest_hash` (E2) are ruled IN
(`docs/CODEX-HANDOFF.md` §1) and are plumbed through `RowSeed`, `WitnessRecord`, the UDA
list, recovery, and query. But they are hardcoded `None` at the sole admission site
(`crates/tally-core/src/daemon.rs:697-698`) and do not exist on `EnqueuePayload`
(`crates/tally-core/src/wire.rs:331`), which is `deny_unknown_fields`. **No caller can
set either field by any route.** The ledger can carry them; nothing can populate them.

**Scope.**
- Add `evidence_class: Option<Value>` and `manifest_hash: Option<String>` to
  `EnqueuePayload`, both `#[serde(default)]`.
- Thread them to the existing `RowSeed` slots at `daemon.rs:697-698`, replacing the
  hardcoded `None`s.
- Add the corresponding `tally enqueue` CLI flags in `crates/tally/src/main.rs`.
- Both remain **opaque pass-through**. tally never interprets, validates the shape of, or
  branches on either value.

**Invariants that must not move.** Absent keys stay absent, so the witness oracle hashes
stay byte-identical. Present values enter the canonical hash input in the order
`evidence_class`, `manifest_hash`, immediately before `seq`. Verify with the golden-oracle
gate: `valid.jsonl` GREEN, `tampered.jsonl` RED.

**Acceptance.**
- An enqueue carrying both fields witnesses both verbatim and surfaces them in `query`.
- An enqueue carrying neither produces a record byte-identical to today's.
- The events-dir producer can carry both through the ordinary narrower.
- Golden-oracle gate unchanged.

Commit as `Wave 11.6: enqueue admission for evidence_class and manifest_hash`.

---

## 4. Wave 12 — BS-12, the Nix module layer

The heaviest wave. `nix/modules/nixos.nix` and `nix/modules/home-manager.nix` are 19-line
stubs today — this is greenfield.

### Scope (after the §2 corrections)

- **Home-Manager module** — user lifecycle: daemon under `systemd --user`, CLI, pools,
  producers, adapters, drain, `cooperative` enforcement, `LoadCredential` passthrough.
- **NixOS module, fully un-stubbed** — system scope: `StateDirectory=tally` and
  `LogsDirectory=tally` for the system daemon. Nothing else. All cgroup delegation and
  overlay surface is struck.
- **Every option in `NIX-SPEC.md` §0–§9 typed with a default and an example**, minus the
  struck surface. Includes the enqueue submodule (`noEnqueue`, `buildEffect.onKey`,
  `pool-reachability.onReturnAttest`), the `lease.*` and `enqueue.*` guardrail timeouts,
  and the `budgetGb` / `consumptionCap` split.
- **`foldl'` into `systemd.user.{services,timers}`** — generating
  `tally-producer-<n>.timer` for `calendar` kinds and `tally-producer-<n>.service` with
  `Restart=always` for `pool-reachability` kinds. These units invoke the
  `__producer-dispatch` CLI contract established in Wave 11; read it from
  `crates/tally/src/main.rs` rather than inventing an interface.
- **`checkedConfig` build-time validator** (`NIX-SPEC.md` §9) — fails `nixos-rebuild` on
  bad config.
- **The optional usage-meter feeder** — pool submodule, direct-exec `argv`, positive
  `pollIntervalSec` (default 120), single allowed `budgetClass = programmatic`, supervised
  unit receiving `TALLY_METER_*` env. Valid meter data may clamp headroom **downward only**
  and can never grant capacity.
- **`tally-witness-emit` export** — the `OnSuccess=` / `OnFailure=` attestation line.
- **Priority ranks** `interrupt=1000, high=100, medium=50, low=10` in Nix, matching code.

### Corrected acceptance

- A bad pool set fails `nixos-rebuild`.
- **Every in-scope conventions row terminates in a generated artifact.** Rows 1–3 of
  `NIX-SPEC.md` §6 (dmem local, dmem remote, `servingSlice`) are deferred surface and
  **must generate nothing** — their absence is the correct result.
- A stock host activates at `enforce = cooperative`.
- ~~A worker `servingSlice` gets `Delegate=yes`~~ — **struck**.
- No option outside the corrected surface is declared, and `enforce` accepts only
  `cooperative`.

### Design guidance — earned, follow it

**Use per-kind `types.submodule` with real `assertions`, not a flat option namespace with
lazy `throw`.** microvm.nix dispatches its multi-backend config with flat options plus
`throw` buried in the runner builder, and its own maintainers filed that as a mistake
(their issue #338) precisely because `throw` stops at the first error. Your acceptance
criterion is "a bad pool set fails `nixos-rebuild`" — with `assertions` the user sees all
errors at once; with `throw` they fix them one rebuild at a time. Model
`producers.<name>` and `adapters.<name>` as per-kind submodules.

**Keep `checkedConfig` as a real derivation invoking the Rust validator.** microvm.nix
validates purely with assertions and no build-time checker, and for them that is right —
they have one validator. You have two (`checkedConfig` in Nix, `config.rs` in Rust) and
they can drift. A derivation that runs the actual Rust config parser at build time is the
mechanism that prevents drift. Use assertions for what Nix expresses cheaply, and keep the
derivation for parity with the binary.

**Prefer eval-time absence to runtime branching.** microvm.nix's contract: compute
everything at eval time, bake it into files inside the derivation, and let generic systemd
units gate on file presence (`ConditionPathExists`). If a capability is not configured,
the script does not exist, the unit is an inert no-op. Apply this to per-producer and
per-adapter units — no runtime "is this enabled" branching.

**Keep adapter argv as structured data.** Do not assemble CLI strings in nested Nix. The
executor never introduces a shell string; the module must not either.

**Known upstream bug to avoid inheriting:** `nixos-rebuild switch` does not stop a
template-unit instance when its entry is removed from config (microvm.nix issue #508,
open). Your `foldl'` into `systemd.user.*` inherits this by default. Provide an explicit
stop/cleanup path for removed producers.

### Suggested internal ordering

One wave, one commit, but build it in two stages with a clean seam at the validator:

1. Options schema + `checkedConfig` validator + Home-Manager module.
2. systemd unit generation (jobs, producers, meter feeder) + `tally-witness-emit` export
   + NixOS module.

If a usage window kills the session between the two, git state plus `WAVES-STATE.md` is
enough to resume at stage 2.

### Carried obligations — close these during Wave 12

Recorded at `WAVES-STATE.md:269`, transcribed here so you do not have to hunt:

1. `/home/tom/mecattaf/dotfiles/flake.nix` points its tally input at
   `github:mecattaf/tally`, but this repo's origin is `mecattaf/tally.nix`. Reconcile the
   input.
2. That repo's `home/tally.nix` still sets `conductorHost`, which is **cut** (subsumed by
   pool addressing — and pool addressing itself is deferred). Update it against the final
   module. Do **not** teach the new module the obsolete field.

Also note: the BS-10 codex adapter preset has a frozen argv prefix
`["codex","exec","--json","--"]`, so anything after `--` is read as prompt text. Callers
needing `-C` / `-p` / `--output-schema` must declare additional adapters. BS-10 made
adapters an open map dispatching without a recompile, so this is a pure-Nix concern — do
not modify the frozen preset. Make **declaring extra adapters ergonomic** in the module
surface.

Commit as `BS-12: nix module layer`.

---

## 5. Wave 13 — the three scenarios

Exactly three fault-injected scenarios, as scripted pass/fail assertions:

1. **fanout-guardrail** — an N-child firehose with `depthCap` / `fanoutCap` enforced.
2. **slow-sqlite** — the socket keeps accepting; a row lost in the ack→commit window
   rebuilds.
3. **pool-vanished/return** — worker reboot produces a verdict and re-presents the exact
   durable row.

**Scope discipline — this wave is smaller than it looks.** The underlying behaviors are
already proven by tests written in DONE waves: fanout at
`crates/tally-core/src/wire.rs:777`
(`no_enqueue_depth_fanout_and_dedup_are_enforced`); slow-sqlite as Wave-9 acceptance;
pool-vanished/return as Wave-7 acceptance and Wave-11's
`confirmed_pool_loss_witnesses_and_return_re_presents_the_same_row`. **Do not rebuild that
coverage.**

What Wave 13 uniquely adds is (i) genuine *fault injection* rather than fake-backing, and
(ii) a *scripted, re-runnable* gate. Build a thin harness that scripts the existing
assertions against a real daemon, and add real fault injection only where it is currently
absent — principally slow-sqlite.

`pool-vanished/return` is the one scenario where a real multi-host run finds what fakes
miss. Run it on `worker-tb`, consistent with every prior wave's live-evidence pattern.

Commit as `BS-14 scenarios: fanout, slow-sqlite, pool-return`.

---

## 6. Per-wave ritual — unchanged, transcribed

For every wave below:

a. Write the wave's acceptance bullets into `WAVES-STATE.md`, then implement immediately.
   No separate research phase. Do not fetch GitHub issues.

b. Implement and test the complete unit. Intermediate commits are fine.

c. **Evidence gate** — run and record the full command plus result for each:
   - the unit's own tests;
   - `cargo test --workspace`;
   - `cargo clippy --workspace --all-targets -- -D warnings`;
   - `cargo fmt --all --check`;
   - `nix flake check -L` (and `--max-jobs 0` over `ssh://tom@worker-tb` where the wave
     builds derivations);
   - **REGRESSION, every wave:** `witness verify` → `valid.jsonl` GREEN,
     `tampered.jsonl` RED. If this ever flips, stop everything and fix it first.
   - re-run whichever of the three scenario assertions have their preconditions built;
   - no-stubs: `grep -rn "todo!\|unimplemented!\|TODO" crates/` returns nothing.

d. Adversarially inspect the wave diff against every acceptance bullet: missing behavior,
   scope creep, and tests that do not prove their claim. Fix every proven problem and
   rerun the affected evidence.

e. Commit once under the prescribed subject with evidence in the body; mark the wave DONE
   in `WAVES-STATE.md` with evidence and a self-audit paragraph. Only then does the next
   wave begin.

**Honesty law:** no wave is DONE without pasted evidence. A gate you could not run is
recorded as not run, never as passed.

**Process law:** no wave parallelism, ever. Sole writer; any review agents are read-only.

**Docs freeze:** `docs/` is frozen again. Wave 11.5 was a one-time exception and it is
closed. Do not edit any file under `docs/`, tracked or untracked, during Waves 11.6, 12,
or 13. If you find further prose that contradicts the scope law, record it in
`WAVES-STATE.md` and follow this file — do not "fix" the doc.

**Protected files:** `Cargo.lock` and `wave-log.jsonl` must remain byte-for-byte unchanged
unless the wave genuinely adds a dependency. The four untracked files stay untracked.

---

## 7. Permanently out — never build, never stub

**Deferred-not-stubbed** (absence is correct): dmem / patched-systemd / `servingSlice`;
RemoteLease / cross-host re-adoption; the full BS-13 golden-diff harness; the four BS-14
scenarios beyond the three named above (remote re-adoption, network-blip hysteresis
discrimination, dmem capability-downgrade, cooperative-yield timing).

**Never in tally at all:** external object-store intake (subsumed by `events-dir`).

**Never in tally at all — driver-layer, by the one law:** task DAGs, git worktrees, review
lifecycle states, build-failure taxonomy beyond the spec's verdict enum, G1–G4 semantics,
X1/X2 handling, and any logic that decides what work runs next. B5 account rotation is
driver-side and produces no tally code.

tally's one law is **contention and proof, never content or control.** If a proposed
feature requires tally to know what a job *means*, it belongs in the driver.

---

## 8. Definition of done for the whole build

The build is complete when:

- Wave 11.6, Wave 12, and Wave 13 are each committed with full evidence in
  `WAVES-STATE.md`.
- `nix flake check -L` passes on `worker-tb`.
- A stock host activates the Home-Manager and NixOS modules at `enforce = cooperative`
  and the daemon runs under `systemd --user`.
- A bad pool set fails `nixos-rebuild` at eval time.
- All three scenario assertions pass as scripted gates.
- The golden-oracle witness gate is GREEN/RED as always.
- `grep -rn "todo!\|unimplemented!\|TODO" crates/` returns nothing.

At that point tally.nix is feature-complete for its declared scope: a daily-drivable
NixOS/Home-Manager module, cooperative enforcement, five producer kinds, four adapter
presets, the witness ledger with crash recovery, and three fault-injection gates.

Stop there. Do not begin driver work.
