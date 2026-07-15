# tally.nix — BUILD-SEQUENCE

> Ordered Rust crate/module build units. Each unit carries a scope, ordered
> dependencies, a style-transfer exemplar repo, concrete acceptance tests, and a
> definition of done. The reference implementation (the prior Bun prototype) is the
> golden-test oracle: acceptance is measured by diffing the Rust build byte-for-byte
> against it on the surviving surface — NDJSON wire frames and witness JSONL.
>
> **DOMINANT ACCEPTANCE TEST:** the Rust `tally witness verify` MUST pass (GREEN) on
> the reference implementation's `witness.jsonl` ledger
> (`test/fixtures/ledger/valid.jsonl`) and MUST fail (RED) on `tampered.jsonl`. This
> is the first gate every later unit is measured against.
>
> **Issue mapping:** BS-0 → #1, BS-1 → #2, BS-2 → #3, BS-3 → #4, BS-4 → #5,
> BS-5 → #6, BS-6 → #7, BS-7 → #8, BS-8 → #9, BS-9 → #10, BS-10 → #11, BS-11 → #12,
> BS-12 → #13, BS-13 → #14, BS-14 → #15.

## Workspace layout

`[workspace] members = [ "tally-core", "tally" ]`.

- `tally-core` — library crate: witness, attest, taskdb, lease, exec, recover, wire,
  adapters, producers, journal.
- `tally` — binary crate: clap `Opts`, `--mode {daemon,check-config}` ValueEnum,
  nested subcommands, `tallyd` argv0 dispatch.

---

### BS-0 — Repo & workspace bootstrap  (issue #1)

**Scope.** Two-crate workspace; `--mode {daemon,check-config}` plus the subcommand
skeleton; `tallyd` symlink; flake outputs; the `checkedConfig` dev shell; a
`nix run .#dev` mock daemon.

**Dependencies.** None (root unit).

**Exemplar.** attic.

**Acceptance.**
- `tally --help` prints the full verb tree.
- `nix flake check` is green.
- `--mode check-config` rejects a malformed config with a non-zero exit.

**Definition of done.** No stubs. The workspace builds, the flake checks, and the
command skeleton dispatches every declared verb. Feature-complete for this unit.

---

### BS-1 — witness crate + attestation chain  (issue #2)

**Scope.** Port the verdict record exactly: field order, `canonicalHashInput`,
`computeHash`, `prev_hash` / `seq` / `GENESIS`, the four-pass verify, `VerifyProblem`,
and `O_APPEND` + `fsync`. Add the `pool-vanished` verdict value and the `preempted`
verdict value. Add a separate `attestations.jsonl` as an independent chain with its own
append and verify, walked distinctly and unauthenticated by construction. `parent` is
NOT a witness field.

**Dependencies.** BS-0.

**Exemplar.** tally-oracle-surface-map + hercules.

**Acceptance (carries the DOMINANT test).**
- `tally witness verify` is GREEN on the reference `valid.jsonl`, RED on
  `tampered.jsonl`.
- A verdict line with no `parent` and no `kind` field still verifies (absent-field
  back-compat).
- An attestation line is reported on its own chain and is never counted toward
  canonical `gpu_seconds`.

**Definition of done.** No stubs. The verdict chain, the attestation chain, both
new verdict values, and the full verify path are implemented and pass the dominant
test. Feature-complete for this unit.

---

### BS-2 — taskdb (in-process taskchampion; a rebuildable cache)  (issue #3)

**Scope.** `Replica<SqliteStorage>` opened once at daemon start against
`$XDG_DATA_HOME/tally/taskdata` (ReadWrite, create-if-missing). The TALLY UDA
vocabulary including `parent_uuid`; the durable-row admission predicate ported exactly;
`argv_json` / `evidence_json`; batched `commit_operations`. The sqlite file is a
rebuildable cache, not a second source of truth: the commit happens OUTSIDE the
fsync-before-ack barrier, and every row is crash-safe because it reconstructs from
witness + events.

**Dependencies.** BS-1.

**Exemplar.** taskchampion-embedding.

**Acceptance.**
- A rowed enqueue is readable by a stock `task` binary (open ReadOnly to avoid a
  write race).
- A live-orchestrator-spawned job records `task_uuid: null`.
- `--parent` lands in the row and never in the witness.
- A row lost in the ack → commit window is rebuilt from witness + events on
  `recover()`.

**Definition of done.** No stubs. Rows admit, commit, read back through a stock
`task`, and rebuild after a crash in the commit window. Feature-complete for this unit.

---

### BS-3 — wire/RPC + CLI skeleton  (issue #4)

**Scope.** NDJSON framing; the full verb tree plus `lease acquire/release` and
`witness append`; the exit-code contract (Unreachable → 3, invalid_params → 2,
not_found → 4, else → 1); `--wait` mapping a verdict to an exit code; a quote-aware
argv splitter with no shell. Job-originated enqueue is allowed under server-side
guardrails: a job bearing `TALLY_JOB_ID` may enqueue subject to `depthCap` (default 3),
`fanoutCap`, a mandatory `dedupKey`, and the gh actor rule, with `--parent`
auto-stamped. Only a leaf carrying the `noEnqueue` / `TALLY_NO_ENQUEUE` capability is
refused.

**Dependencies.** BS-2.

**Exemplar.** greetd + attic.

**Acceptance.**
- Every exit code is byte-for-byte identical to the reference implementation.
- A job without `noEnqueue` can enqueue a child (the OCR-firehose case) and it is
  auto-parented.
- A `noEnqueue` leaf's enqueue is refused.
- An enqueue exceeding `depthCap` is refused.

**Definition of done.** No stubs. Framing, verbs, exit codes, and the enqueue
guardrails are all live. Feature-complete for this unit.

---

### BS-4 — lease engine + yield channel  (issue #5)

**Scope.** Generalized pools; two predicates (co-residency; windowed-consumption with
`windowSec` + `consumptionCap`); a monotone `bumpEpoch` bumped on EVERY daemon start;
atomic co-allocation. Lease-level cooperative preemption via an explicit yield channel:
a lease-status flag, a `yieldPollSec` poll, and a `yieldGraceSec` hard-reclaim that
resolves to `preempted`. The `interrupt` priority tier lives here at rank 1000: it is
best-effort cooperative by default, with per-pool hard-preempt opt-in, bounded by
`yieldGraceSec`. `lease acquire/release`. The `LeaseBackend { admit, release,
heartbeat }` trait with `LocalLease` (systemd unit-liveness, no bespoke heartbeat) and
`RemoteLease` (HTTP, `remoteHeartbeatSec` / `remoteReapSec` / `graceSec`).

**Dependencies.** BS-3.

**Exemplar.** tally-oracle + colmena.

**Acceptance.**
- 50 children produce a 50-deep queue.
- An `interrupt` job forces a `low` holder to yield within `yieldGraceSec`, and hard-
  reclaims to `preempted` if the holder does not yield in time.
- A coordinator restart re-adopts a remote lease via the bumped epoch, and the worker
  refuses to regrant it.

**Definition of done.** No stubs. Both backends, both predicates, the yield channel,
and the rank-1000 `interrupt` tier are implemented. Feature-complete for this unit.

---

### BS-5 — executor (owns systemd-run) + capture + enforcement  (issue #6)

**Scope.** The `systemd-run --user --wait --collect --unit tally-job-<uuid>` launch
line; an `ExecStopPost` exit record; deterministic unit names; `TALLY_*` injection
(including `TALLY_NO_ENQUEUE`); `CPUWeight` / `MemoryMax` always set; `StandardOutput`
/ `StandardError` routed to `capture/<uuid>` for the scrape engine; a 127-absent
direct-spawn fallback.

**Enforcement (the load-bearing decision of this unit).** The `enforce` backend has
three settings: `cooperative`, `dmemcg-booster`, and `dmem`. **On the NixOS/AMD fleet
the target enforcement is `enforce = "dmem"`.** This is real GPU memory confinement via
a patched-systemd `DeviceMemoryMax` overlay: the executor requires `Delegate` wiring on
the unit, writes the limit, and reads back `dmem.current` to confirm the limit took.
`dmem` on a non-patched systemd MUST fail loudly rather than silently degrade.
`cooperative` is the portable fallback for stock hosts that lack the patched systemd.
Remote enforcement is never stamped locally: it is a negotiated capability, and when a
pool is remote the worker-side confinement writes `dmem.max` into the delegated cgroup
of the declared `servingSlice`. `LoadCredential` splices secrets by name only.

**Dependencies.** BS-4.

**Exemplar.** microvm.nix + attic + agenix.

**Acceptance.**
- Local `enforce = "dmem"` on a non-patched systemd fails loudly.
- A REMOTE `dmem` pool stamps nothing on the coordinator, downgrades if the broker
  does not advertise the capability, and the worker stamps its `servingSlice` cgroup.
- A credential NAME appears in the unit but its VALUE never appears in any witness,
  attestation, events, or journald record.
- Captured stdout feeds the scrape engine.

**Definition of done.** No stubs. The launch line, capture routing, credential
splicing, and all three enforcement backends — including `dmem` with the
`dmem.current` read-back and the remote worker-slice path — are implemented.
Feature-complete for this unit.

---

### BS-6 — evidence gate + dedup + verdict model  (issue #7)

**Scope.** Verdict synthesis ported exactly; `combineArtifactHashes`;
dedup-by-existence; `countsTowardCanonicalGpuSeconds`. The `pool-vanished` verdict
(set only after hysteresis) and the `preempted` verdict are both excluded from
canonical `gpu_seconds` and are eligible for retry (auto vs manual per the retry
policy).

**Dependencies.** BS-5, BS-1.

**Exemplar.** tally-oracle-surface-map.

**Acceptance.**
- Multi-artifact dedup hits are detected.
- A job whose remote pool vanishes (after hysteresis) records the `pool-vanished`
  verdict, distinct from `failed`.
- A yielded holder records `preempted`.

**Definition of done.** No stubs. Verdict synthesis, dedup, and both new verdict
classifications with their retry eligibility are live. Feature-complete for this unit.

---

### BS-7 — recovery planner + pool-return re-present  (issue #8)

**Scope.** `planRecovery` is PURE: local facts in, plan out; the owning unit is always
coordinator-local. The five invariants ported exactly with `witness_lsn := witness
seq`. Because the replica is a rebuildable cache, `recover()` REBUILDS rows from witness
+ events rather than trusting the sqlite. Unit adoption via `systemctl --user show`.
`autoResume`: on a `pool-return` event, re-present the durable `pool-vanished` row
itself — this is re-presentation of the existing row, not origination of a new one — at
`attempt + 1` / `labor_class = recovered`, auto vs eligible gated by the retry policy.
A separate advisory attestation is emitted via `onReturnAttest`. Coordinator-switch =
daemon-restart + local adoption + remote re-adoption.

**Dependencies.** BS-6, BS-2.

**Exemplar.** tally-oracle-surface-map.

**Acceptance.**
- The planner is pure (facts in → plan out, no I/O).
- A `pool-vanished` row re-presents via the adapter `resume()` on `pool-return` at
  `attempt + 1` / `labor_class = recovered`.
- An external ReadWrite mutation to the replica cannot silently drive a double-run
  (ReadOnly is enforced).
- A row lost in the ack → commit window is rebuilt.

**Definition of done.** No stubs. The pure planner, the five invariants, row rebuild,
and `autoResume` re-presentation are all implemented. Feature-complete for this unit.

---

### BS-8 — journald emit + read-time join  (issue #9)

**Scope.** A native AF_UNIX `SOCK_DGRAM` journal client implemented behind a
`journal.native` toggle, with a `StandardOutput=journal` stdout-fallback path that is
buildable when the toggle is off. `TALLY_*` stage-gated validation at emit time. The
`query status/log/render/standup` read-time joins. `journalctl -t tally -o json -f` is
the delta stream.

**Dependencies.** BS-5.

**Exemplar.** tally-oracle-surface-map.

**Acceptance.**
- Every `TALLY_EVENT` is emitted with its required fields.
- `standup` buckets match the reference implementation.
- The native-vs-stdout emission path is a config toggle, not a hard-coded assumption —
  both paths build and run.

**Definition of done.** No stubs. Both emission paths, emit-time validation, and all
read-time joins are implemented. Feature-complete for this unit.

---

### BS-9 — daemon loop + supervisor + barrier  (issue #10)

**Scope.** A current-thread tokio `select!` loop; `Rc<Context: RwLock>`; a
supervised-task wrapper for producers; `sd_notify` + `WatchdogSec`. The
fsync-before-ack core is enumerated and bounded: admission + lease + verdict-fsync
ONLY; the replica commit and the attestation appends are OUTSIDE the barrier. The
`BarrierTracker` is a direct RPC.

**Dependencies.** BS-3, BS-4, BS-7.

**Exemplar.** greetd + sd-notify.

**Acceptance.**
- A panicking producer restarts alone without taking down the loop.
- A stalled replica commit does NOT stall the socket (verified by fault injection).
- A late `--wait` resolves immediately.

**Definition of done.** No stubs. The loop, the supervisor, the bounded fsync core,
and the barrier are all live. Feature-complete for this unit.

---

### BS-10 — adapters engine + nix presets + capture/scrape envelope  (issue #11)

**Scope.** The adapter engine: argv-template, `%<capture>%` multi-variable resume, an
N-named scrape with regex + jsonPath + stream selectors. No `sh` variant. Presets for
pi, claude-code, and shell; the model recorded verbatim; the `yieldHook`.

**Dependencies.** BS-5, BS-8.

**Exemplar.** disko + niri-flake.

**Acceptance.**
- A new adapter defined in pure nix dispatches with no recompile, within the
  documented envelope.
- A claude-code jsonPath capture yields a `session_ref`.
- The envelope boundary (streaming vs approval-gated) is documented as out of scope.

**Definition of done.** No stubs. The engine, all three presets, the scrape selectors,
and the yield hook are implemented. Feature-complete for this unit.

---

### BS-11 — producers registry  (issue #12)

**Scope.** The `producers.<name>` kind registry: `calendar`, `events-dir` (same
`validateEnqueueParams`, atomic archive), `gh` COMPLETED (mutation half, actor-exclude,
sources), `r2`, `build-effect` (store-path single-flight dedup), and
`pool-reachability` (hysteresis; `onLost` / `onReturn` / `onReturnAttest`). Field
ownership: `pool` / `priority` / `adapter` live on the enqueue payload only.

**Dependencies.** BS-9, BS-10.

**Exemplar.** hercules + tally-oracle.

**Acceptance.**
- An events file enqueues through the identical narrower as any other producer.
- `pool-reachability` fires `onReturnAttest` (advisory, `noEnqueue`) only after
  `hysteresis` failed probes.
- `build-effect` fires exactly once per store path.

**Definition of done.** No stubs. Every producer kind is registered and dispatches
through the shared narrower. Feature-complete for this unit.

---

### BS-12 — the Nix module layer (product surface)  (issue #13)

**Scope.** Home-manager plus a fully un-stubbed NixOS module. Every option typed with
defaults and examples, including the enqueue submodule with `noEnqueue`,
`buildEffect.onKey`, `pool-reachability.onReturnAttest`, the `lease.*` and `enqueue.*`
guardrail timeouts, the `budgetGb` / `consumptionCap` split, and `servingSlice`. A
`foldl'` into `systemd.user.{services,timers}`; the `checkedConfig` build-time
validator; auto `Delegate=yes` whenever any pool has `enforce = dmem` AND on any
declared `servingSlice`; `StateDirectory` / `LogsDirectory` for the system daemon; the
patched-systemd overlay; the `tally-witness-emit` export; the conventions table.

**Dependencies.** BS-11, BS-5.

**Exemplar.** microvm.nix + disko/niri + agenix + attic.

**Acceptance.**
- A bad pool set fails `nixos-rebuild`.
- Every conventions row terminates in a generated artifact.
- A stock host activates at `enforce = cooperative`.
- A worker `servingSlice` gets `Delegate=yes`.

**Definition of done.** No stubs. Every option in §1–§10 is typed with a default and
an example, the validator runs at build time, and the systemd wiring generates for both
the stock and the fleet host. Feature-complete for this unit.

---

### BS-13 — golden-oracle harness  (issue #14)

**Scope.** Diffs the Rust build byte-for-byte against the reference implementation on
the surviving surface only: surviving wire frames plus witness JSONL. Ports the
dev/mock and `test/e2e` fixtures. Scope boundary: this harness governs only the
surviving frozen surface; cut surface is validated by absence; the net-new fleet
surface belongs to BS-14, not here.

**Dependencies.** BS-1 through BS-12 (governs their surviving surface).

**Exemplar.** tally-oracle-surface-map.

**Acceptance.**
- Every surviving e2e fixture diffs clean against the reference.
- The DOMINANT test (`witness verify` GREEN on `valid.jsonl`, RED on `tampered.jsonl`)
  is the first gate.

**Definition of done.** No stubs. The full surviving fixture set diffs clean and the
dominant test gates the suite. Feature-complete for this unit.

---

### BS-14 — non-oracle FLEET conformance suite  (issue #15)

**Scope.** A net-new, fault-injected multi-host integration suite covering the surface
the golden oracle cannot see (no reference oracle exists for it):
- worker reboot → `pool-vanished` verdict + `pool-return` row re-presentation;
- network blip vs true vanish → hysteresis discrimination;
- coordinator switch mid-lease → remote re-adoption;
- cooperative-yield timing → a `low` holder yields within `yieldGraceSec` and resolves
  to `preempted` (a behaviour change the artifact-diff rig cannot observe);
- slow-sqlite → the socket keeps accepting and a row lost in the ack → commit window
  rebuilds;
- remote `dmem` capability-downgrade + worker `servingSlice` stamp;
- job-originated fan-out guardrails → an OCR firehose enqueues N children with
  `depthCap` / `fanoutCap` enforced.

**Dependencies.** BS-13 (runs alongside it as a separate gate).

**Exemplar.** net-new (no exemplar; fault-injected multi-host integration).

**Acceptance.**
- Each fault-injected scenario passes its behavioural assertion.
- This suite is a SEPARATE acceptance gate alongside BS-13; it does not claim oracle
  coverage for any of its scenarios.

**Definition of done.** No stubs. Every listed scenario has a fault-injection harness
and a passing behavioural assertion. Feature-complete for this unit.
