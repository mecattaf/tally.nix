# tally.nix — BUILD-SEQUENCE

> Ordered Rust crate/module port units; each carries acceptance tests + a
> style-transfer exemplar. The port is a TRANSCRIPTION against the Bun ORACLE.
> Golden-oracle testing diffs Rust vs Bun on NDJSON wire frames + witness JSONL.
>
> **DOMINANT ACCEPTANCE TEST:** Rust `tally witness verify` GREEN on the Bun-era
> `witness.jsonl` (`test/fixtures/ledger/valid.jsonl`), RED on `tampered.jsonl`.
> Wave-2: the verdict chain is ported UNCHANGED (attestation chain and `--parent`
> live OFF it, R24/R25) precisely so this test survives. Wave-3: the taskchampion
> replica is a rebuildable cache (PS#9/R13), jobs MAY enqueue under guardrails (R28),
> and the task-0→task-1 seam is row-re-presentation (R16) — the acceptance tests below
> reflect these corrections.

## Workspace (attic exemplar)

`[workspace] members = [ "tally-core", "tally" ]` (CD-06). `tally-core` = witness,
attest, taskdb, lease, exec, recover, wire, adapters, producers, journal.
`tally` = the bin crate (clap `Opts`, `--mode {daemon,check-config}` ValueEnum,
nested subcommands, `tallyd` argv0 dispatch).

### BS-0 — Repo & workspace bootstrap
Exemplar: attic. 2-crate workspace, `--mode {daemon,check-config}` + subcommand
skeleton, `tallyd` symlink, flake outputs, `checkedConfig` shell, `nix run .#dev`
mock. Accept: `tally --help` prints the surviving verb tree; `nix flake check` green;
`--mode check-config` rejects a bad config.

### BS-1 — witness crate + attestation chain (identity; verbatim)
Exemplar: tally-oracle-surface-map + hercules. Port the verdict record VERBATIM
(field order, `canonicalHashInput`, `computeHash`, prev_hash/seq/GENESIS, four-pass
verify, VerifyProblem, O_APPEND+fsync). NET-NEW resource-loss verdict VALUE (R14,
name from CD-01) + `preempted` (R29, name also CD-01). Separate `attestations.jsonl`
independent chain (R24): its own append + verify, walked distinctly,
unauthenticated-by-construction. `--parent` NOT a witness field (R25). Accept
(DOMINANT): verify GREEN on Bun `valid.jsonl`, RED on `tampered.jsonl`; a verdict line
with NO `parent`/`kind` still verifies (absent-field back-compat proof); an
attestation line is reported on its own chain, never counted in canonical gpu_seconds.

### BS-2 — taskdb (in-process taskchampion; a rebuildable cache)
Exemplar: taskchampion-embedding. `Replica<SqliteStorage>` opened once at daemon
start against `$XDG_DATA_HOME/tally/taskdata` (ReadWrite, create-if-missing, CD-08);
TALLY UDA vocabulary incl. `parent_uuid` (R25); durable-row admission predicate
verbatim; argv_json/evidence_json; batched `commit_operations`. **The sqlite file is a
REBUILDABLE cache (PS#9/R13), NOT a second source of truth; the commit is OUTSIDE the
fsync-before-ack barrier (R31) and is crash-safe because the row reconstructs from
witness+events.** Accept: rowed enqueue readable by a stock `task` binary (ReadOnly
viewer = write-race avoidance, R13); live-orchestrator-spawned → `task_uuid:null`;
`--parent` lands in the row, never the witness; a row lost in the ack→commit window is
rebuilt from witness+events on recover().

### BS-3 — wire/RPC + CLI skeleton (surviving client contract)
Exemplar: greetd + attic. NDJSON framing; the SURVIVING verb tree + `lease
acquire/release`, `witness append`; exit-code contract (Unreachable→3,
invalid_params→2, not_found→4, else→1); `--wait` verdict→exit; quote-aware argv
splitter (no shell). Seam-B broadcast CUT (R19). **Job-originated enqueue is ALLOWED
under server-side guardrails (R28): jobs bearing `TALLY_JOB_ID` may enqueue, subject
to depthCap (default 3) + fanoutCap + mandatory dedupKey + gh actor rule, with
`--parent` auto-stamped; only a leaf carrying the `noEnqueue`/`TALLY_NO_ENQUEUE`
capability is refused.** Accept: every SURVIVING exit code byte-for-byte vs Bun; a
job WITHOUT noEnqueue can enqueue a child (OCR-firehose) and it is auto-parented; a
`noEnqueue` leaf's enqueue is refused; an enqueue exceeding depthCap is refused.

### BS-4 — lease engine (pls absorbed) + yield channel
Exemplar: tally-oracle + colmena. Generalized pools (R8); two predicates
(co-residency, windowed-consumption with windowSec+consumptionCap); monotone
`bumpEpoch` (bumped on EVERY daemon start, R30); DS4 atomic co-allocation; lease-level
cooperative preemption with the EXPLICIT yield channel (R29 — lease-status flag +
`yieldPollSec` poll + `yieldGraceSec` hard-reclaim → `preempted`); the `interrupt`
tier (R15); `lease acquire/release`; `LeaseBackend { admit, release, heartbeat }` with
`LocalLease` (systemd unit-liveness, no bespoke heartbeat, R31) + `RemoteLease` (HTTP,
`remoteHeartbeatSec`/`remoteReapSec`/`graceSec`, R30). Accept: 50-child → 50-deep
queue; an `interrupt` job forces a `low` holder to yield within `yieldGraceSec` else
hard-reclaims → `preempted`; a coordinator restart re-adopts a remote lease via bumped
epoch and the worker refuses to regrant it.

### BS-5 — executor (owns systemd-run) + capture + remote-enforce negotiation
Exemplar: microvm.nix + attic + agenix + dmem brief. The `systemd-run --user --wait
--collect --unit tally-job-<uuid>` line; ExecStopPost exit record; deterministic
names; TALLY_* injection (§9, incl. TALLY_NO_ENQUEUE); CPUWeight/MemoryMax always;
`enforce` backend (cooperative/dmemcg-booster/dmem with loud assertion + dmem.current
read-back, R10); **remote enforce NEVER stamped locally — negotiated capability
(R27/§2.1a); worker-side confinement writes `dmem.max` to the declared `servingSlice`
delegated cgroup (R27)**; LoadCredential splicing (R11); `StandardOutput/Error` →
`capture/<uuid>` for scrape (R32); 127-absent direct-spawn fallback. Accept: local
`dmem` on non-patched systemd fails loudly; a REMOTE `dmem` pool stamps nothing on the
coordinator, downgrades if the broker doesn't advertise the capability, and the worker
stamps its servingSlice cgroup; a credential NAME appears in the unit but its VALUE
never in any witness/attestation/events/journald record; captured stdout feeds the
scrape engine.

### BS-6 — evidence gate + dedup + verdict model
Exemplar: tally-oracle-surface-map. Verdict synthesis verbatim; combineArtifactHashes;
dedup-by-existence; countsTowardCanonicalGpuSeconds; NET-NEW resource-loss (R14, set
only after hysteresis, R26) + `preempted` (R29) classification, excluded from canonical
gpu_seconds, ELIGIBLE for retry (auto-vs-manual per CD-19). Accept: multi-artifact
dedup hits; a job whose remote pool vanishes (after hysteresis, R26) records the
resource-loss verdict distinct from `failed`; a yielded holder records `preempted`.

### BS-7 — recovery planner (5 invariants + pool-return trigger)
Exemplar: tally-oracle-surface-map. `planRecovery` PURE (local facts in → plan out,
R26 — the owning unit is always coordinator-local); five invariants verbatim with
`witness_lsn := witness seq` (R13/R31); **the replica is a rebuildable cache (PS#9),
so recover() REBUILDS rows from witness+events (not from an authoritative sqlite)**;
unit adoption via `systemctl --user show`; NET-NEW: re-present the durable
resource-loss ROW itself on a `pool-return` event (R16 — this is re-presentation, not
origination; task-0 is a separate advisory attestation via onReturnAttest, task-1 IS
this row re-presentation; auto-vs-eligible gated by CD-19); coordinator-switch =
daemon-restart + LOCAL adoption + REMOTE re-adoption (R30). Accept: planner pure; a
resource-loss row re-presents via adapter resume() on pool-return at
attempt+1/labor_class=recovered; an external RW mutation to the replica cannot silently
drive a double-run (ReadOnly enforced); a row lost in the ack→commit window is rebuilt.

### BS-8 — journald emit (native socket) + read-time join
Exemplar: tally-oracle-surface-map. Native AF_UNIX SOCK_DGRAM journal client (CD-17 —
CONTESTED, see decisions_for_tom; implement behind a `journal.native` toggle so a
StandardOutput=journal fallback is buildable if Tom overturns the flip-back); TALLY_*
stage-gated validation at emit time (§9); `query status/log/render/standup` read-time
joins; `journalctl -t tally -o json -f` IS the delta stream (R19). Accept: every
TALLY_EVENT emitted with its required fields; `standup` buckets match Bun; the
native-vs-stdout emission path is a config toggle, not a hard-coded assumption.

### BS-9 — daemon loop + supervisor + sd_notify + barrier + bounded core
Exemplar: greetd + sd-notify. current-thread tokio `select!`; `Rc<Context:RwLock>`;
supervised-task wrapper for producers; sd_notify+WatchdogSec; **the fsync-before-ack
core enumerated and bounded (R31): admission+lease+verdict-fsync ONLY; replica commit
and attestation appends OUTSIDE**; BarrierTracker as direct RPC (R25/CD-25). Accept: a
panicking producer restarts alone; a stalled replica commit does NOT stall the socket
(fault-injection); a late `--wait` resolves immediately.

### BS-10 — adapters engine + nix presets + capture/scrape envelope
Exemplar: disko + niri-flake. Adapter engine (argv-template, `%<capture>%`
multi-variable resume, N-named scrape with regex+jsonPath+stream selector, R32); NO
`sh` variant (CD-20); pi/claude-code/shell presets; model recorded verbatim;
`yieldHook` (R29). Accept: a new adapter in pure nix dispatches with no recompile
WITHIN the documented envelope; a claude-code jsonPath capture yields session_ref; the
envelope boundary (streaming/approval-gated) is documented as out of scope.

### BS-11 — producers registry (unified sensors+intake)
Exemplar: hercules + tally-oracle. The `producers.<name>` kind registry (R21/CD-03):
calendar, events-dir (same validateEnqueueParams, atomic archive), gh COMPLETED
(mutation half, actor-exclude, sources), r2, build-effect (store-path single-flight
dedup), pool-reachability (hysteresis, onLost/onReturn/onReturnAttest, R26). Field
ownership: pool/priority/adapter on the enqueue payload only (devil-2 #5). Accept: an
events file enqueues through the identical narrower; pool-reachability fires
onReturnAttest (advisory, noEnqueue) only after `hysteresis` failed probes;
build-effect fires exactly once per store path.

### BS-12 — the Nix module layer (the product surface / sign-off gate)
Exemplar: microvm.nix + disko/niri + agenix + attic. hm + un-stubbed nixos; all §1-§10
options typed with defaults/examples (incl. enqueueSubmodule w/ noEnqueue,
buildEffect.onKey, pool-reachability.onReturnAttest, lease.* + enqueue.* guardrail
timeouts, budgetGb/consumptionCap split, servingSlice); foldl' →
systemd.user.{services,timers}; `checkedConfig` build-time validator; auto
`Delegate=yes` when any pool enforce=dmem (CD-18) AND on any declared servingSlice
(R27); StateDirectory/LogsDirectory for the system daemon (CD-08); patched-systemd
overlay; `tally-witness-emit` export (R24/CD-24); conventions table. Accept: a bad
pool set fails `nixos-rebuild`; every conventions row terminates in a generated
artifact; a stock host activates at enforce=cooperative; a worker servingSlice gets
Delegate=yes.

### BS-13 — golden-oracle harness (governs BS-1..BS-12 SURVIVING surface only)
Exemplar: tally-oracle-surface-map. Diffs Rust vs Bun byte-for-byte on SURVIVING wire
frames + witness JSONL; ports dev/mock + test/e2e fixtures. **Scoped (devil #10/#14):
it governs only the surviving frozen surface; cut surface validated by absence; the
net-new fleet surface is BS-14's job, not this one.** Accept: every surviving e2e
fixture diffs clean; the DOMINANT test is the first gate.

### BS-14 — non-oracle FLEET conformance suite (net-new; no Bun oracle exists)
Exemplar: (net-new) — fault-injected multi-host integration. Covers the surface the
oracle is blind to (devil #14, open_risk 10): worker reboot (resource-loss verdict +
pool-return ROW re-presentation), network blip vs true vanish (hysteresis
discrimination, R26), coordinator switch mid-lease (remote re-adoption, R30),
cooperative-yield timing (a low holder yields within `yieldGraceSec` → `preempted`,
R29 — a BEHAVIOUR change the artifact-diff rig cannot see), slow-sqlite (socket keeps
accepting; a row lost in the ack→commit window rebuilds, R31), remote-`dmem`
capability-downgrade + worker servingSlice stamp (R27), and job-originated fan-out
guardrails (OCR firehose enqueues N children; depthCap/fanoutCap enforced, R28).
Accept: each fault-injected scenario passes its behavioural assertion; this suite is a
SEPARATE acceptance gate alongside BS-13, and the plan does NOT claim oracle coverage
for any of it.
