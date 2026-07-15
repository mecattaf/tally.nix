# tally.nix — FINAL RULINGS

> Authoritative, numbered consolidation of the 2026-07-14 reshape deliberation
> (`notes/july26-fable-second/july14/chat.md`), amended 2026-07-15 across three
> adversarial waves (wave-1: reboot fold; wave-2: devil-1 findings + 7-lens
> decision-triage fold; wave-3: devil-2 correctness/confirmability fold). R1-R23 are
> the settled consolidation; the **Wave-2 amendments** (R24-R32) resolve devil-1 and
> bake every triage-resolved decision with its cited source; the **wave-3
> corrections** (folded IN-PLACE into R13/R14/R16/R26/R27/R28/R29/R31 and the baked
> list) fix the three blocker-class defects devil-2 found. Each ruling states what
> Bun-era behaviour it supersedes. The full stress-test log is the closing
> **Deliberation record**.

## Identity & shape

**R1 — One flake, one repo, one binary, one release.** tally.nix is a single flake
at `github.com/mecattaf/tally` whose two layers — a Rust static binary and a
pure-Nix module layer — ship together (microvm.nix precedent). *Supersedes:* the
Bun three-runtime composition (Bun daemon + Python `pls` + `task` + `gh`).

**R2 — The binary is both daemon and CLI; no client/daemon split.** `tally daemon
run` is a subcommand of the same binary; a `tallyd` argv[0] symlink +
`SyslogIdentifier=tally` is cosmetics. *Supersedes:* nothing (affirmed vs the
attic/atticd two-binary precedent, which does not apply — no trust boundary).

**R3 — Rust core, justified functionally by in-process TaskChampion.** The core
embeds `taskchampion::Replica<SqliteStorage>` in-process, dissolving the FFI wall
that forced the Bun flip. The replica is a rebuildable CACHE, not a second source of
truth (R13). *Supersedes:* DECISIONS jul9 "TaskChampion access = `task
export`/`import` shell-out"; SPEC "TaskChampion access is shell-out".

**R4 — Two languages, two external shell-outs, total.** After the rewrite the binary
shells out to EXACTLY `systemd-run` and `gh`. *Supersedes:* the four-runtime Bun
composition and the version-pinned `task` binary dependency.

## The pls dissolution & the executor

**R5 — pls dissolves into the binary; the lease-grantor IS the unit-spawner.** No
broker process; queue + lease engine + witness append become one transactional path.
*Supersedes:* DECISIONS PS#5 "Box governor = pls itself"; the `pls
acquire/release/status/coalloc` shell-out.

**R6 — HTTP survives ONLY host-to-host.** Merged, "admit job" and "grant lease" are
one in-process decision; the HTTP seam moves to remote pools via a `LeaseBackend`
trait (`Local`/`Remote`), colmena `to_ssh_host()` schema-mirroring discipline.
*Supersedes:* the always-HTTP `PLS_BROKER`/`PLS_POOL_URLS` client path.

**R7 — The executor owns the `systemd-run` line.** Transient units, deterministic
names (`tally-job-<task_uuid>`), durable `ExecStopPost` exit records under
`unit-exit/`; `CPUWeight`/`MemoryMax` ALWAYS, `dmem.max` when the backend supports
it; lease and cgroup co-created. *Supersedes:* the resource-property-free
`TransientRunner`.

## Pools, enforcement, secrets

**R8 — Pools generalized to every scarce resource.** `pools.<name>` covers `vram`,
`build-slot`, `cpu-slot`, `api`/`sub:<acct>` budgets; two admission predicates
(co-residency, windowed-consumption). *Supersedes:* SPEC/DECISIONS OUV-CM r5
"generalize AFTER a second real pool exists".

**R9 — `enforce` is an enum; the enum is the stable contract.**
`pools.<name>.enforce ∈ { cooperative | dmemcg-booster | dmem }`, per-host values,
NOT a roadmap. Default `cooperative` (**cited:** july14 chat.md:1317 "a fresh flash
on stock nixpkgs still activates cleanly at enforce = \"cooperative\""; dmem recon
tally_nix_application "cooperative … always available, ship first"). *Supersedes:*
operationalizes DECISIONS PS#5's "cooperative lease".

**R10 — dmem enforcement is grounded, not overpromised.** Kernel `CONFIG_CGROUP_DMEM`
merged (6.14, amdgpu); systemd `DeviceMemoryMax` is DRAFT PR #37079 (targeting 259)
on a pinned overlay; kernel `dmem.max` had a reclaim gap (LWN #1072437). `enforce =
"dmem"` is opt-in, fails loudly on a startup assertion, and reads back `dmem.current`
rather than trusting the write. `pkgs.dmemcg-booster` is an own-overlay derivation
(**cited:** dmem recon "needs its own pkgs.dmemcg-booster derivation … not in
nixpkgs"). *Supersedes:* n/a.

**R11 — LoadCredential passthrough; tally never touches secret bytes.** Every
pool/producer/job schema carries a `credentials` name→path map spliced as
`--property=LoadCredential=<name>:<path>`. *Supersedes:* n/a.

## Witness, evidence, recovery

**R12 — Witness hash-chain ported VERBATIM; it is the product identity.** Record
shape, field insertion order, `canonicalHashInput`, the `sha256:`+hex formula,
`prev_hash`/`seq`, `GENESIS_PREV_HASH`, the four-pass verify walk, the VerifyProblem
taxonomy, `O_APPEND`+`fsync` framing — all byte-for-byte. *Supersedes:* nothing —
the one subsystem ported without a liberty.

**R13 — Recovery invariants kept verbatim as a pure planner; sqlite stays a
rebuildable cache, PS#9 ledger-as-truth PRESERVED. [CORRECTED wave-3, closes
devil-2 #2].** `recover()` stays a pure function (durable facts in → `RecoveryPlan`
out). Five invariants: witness_lsn reconciliation (**witness_lsn := the witness
`seq`**, the sole monotone truth), ACK-gated retry, lease-epoch zombie fencing,
undeleted-row re-present (never replay), bounded requeue. **wave-3 correction:**
frozen DECISIONS.md:88 PS#9 "Ledger-as-truth" is NOT superseded — the taskchampion
replica is a DERIVED, rebuildable cache of pending state, reconstructable from
witness (the terminal-transition chain) + `events/`, and is NEVER a second source of
truth. This makes R31's post-ack replica commit crash-SAFE: a crash in the
ack→commit window loses nothing durable, because the acked verdict `seq` + the
`events/` record are the truth and recover() rebuilds the row. The ReadOnly-viewer
enforcement (R31) is therefore WRITE-RACE avoidance (two writers on one sqlite file),
not authority protection. *Supersedes:* nothing (this REPLACES the wave-2
"authoritative / not-rebuildable" framing, which contradicted PS#9 and R31).

**R14 — Evidence gate + verdict synthesis ported; verdict set EXTENDED for
resource-loss.** The gate and `clean-exit-no-artifact` port verbatim; NET-NEW verdict
value for lease/broker-loss (name is the sole residue → CD-01, R26 pins its
semantics), excluded from canonical gpu_seconds, and ELIGIBLE for retry on
pool-return (whether that retry is AUTOMATIC or operator-triggered is CD-19, left to
Tom — so this ruling does not pre-decide it). *Supersedes:* the 5-value Verdict enum.

**R15 — A reserved TOP priority tier above H/M/L.** Priority gains a reserved
privileged tier above `high`=100 / `medium`=50 / `low`=10 (name, rank, and guarantee
level → CD-02, best-effort-cooperative default per R29). *Supersedes:* the
three-value `priorityRank` table.

**R16 — Reboot-aware recovery is a first-class SHAPE, not workflow-in-binary; the
task-0→task-1 seam is specified. [AMENDED wave-3, closes devil-2 #3].** tally exposes
exactly three generic primitives: the resource-loss verdict (R14), the `interrupt`
tier (R15), and a pool-reachability probe (a producer *kind*, R21). **The
task-0→task-1 seam:** on a hysteresis-confirmed pool-return, `recover()`
RE-PRESENTS the existing durable resource-loss ROW itself — this is re-presentation
of a durable row, NOT origination (**cited:** PS#9 "re-present, never replay"),
auto-vs-eligible gated by CD-19. task-0 is a SEPARATE, purely-advisory assessor: it
writes an ATTESTATION line (R24) via the pool-reachability producer's `onReturnAttest`
field, carries the per-leaf `noEnqueue` capability (R28), and never fans out. The
resumed job (task-1) is reached by the ROW re-presentation, not by task-0 enqueuing
it — so the seam needs neither daemon origination nor leaf fan-out, and the one-hop
shape holds. *Supersedes:* the daemon-boot-only scope of recover(); the wave-2
conflation of task-0's attestation with the `onReturn` enqueue.

**R17 — Coordinator-switch recovery = daemon-restart recovery + unit adoption
[AMENDED wave-2, see R30].** The nightly `nixos-rebuild switch` restarts the daemon;
recover() fires; local `--user` transient units are adopted via `systemctl --user
show`. The wave-1 claim "no net-new mechanism" holds ONLY for local units; **R30
adds the distinct cross-host lease re-adoption path** (devil #7). *Supersedes:* n/a
(affirmation of the adoption invariant).

## The cut-list

**R18 — CUT the kitty/zmx detector complex and the session model.** Detector leaves
the core (dotfiles-owned sensor if wanted); `kitty_window_id`/`pane_id` deleted;
`session_ref` survives as an opaque scraped string; `--session` binding REPLACED by
`tally lease acquire`/`release`. *Supersedes:* SPEC "Agent-state detector", CLI
§1.2/§1.3/§3.1, DECISIONS PS#15/PS#13.

**R19 — CUT the bespoke pub-sub delta stream; journald IS the stream.** `journalctl
-t tally -o json -f` is the delta stream; the socket keeps request/response RPC (CLI,
lease negotiation, barriers); Seam-B broadcast waits for a real consumer (CUBS).
*Supersedes:* CLI §2, SPEC "Seam B".

**R20 — CUT models.dev normalization; INVERT the adapter enum; drop
conductor/receiver.** Model string recorded verbatim; `{pi, claude-code, shell}`
inverted into declarative Nix `adapters.<name>`; conductor/receiver emergent.
*Supersedes:* SPEC "model is a models.dev id"; the closed `AgentKind` union; the
`role`/`conductorHost` role gate.

## Kept, and the additive surface

**R21 — KEEP the sensor/producer set; UNIFY into one kind-tagged `producers.<name>`
registry [REFINED wave-2].** KEPT: barriers/wait-groups, lease-level cooperative
preemption, charge/labor_class/gpu_seconds metering, events-dir/drain/r2/gh sensors,
the witness chain + all recovery invariants. **wave-2 nix-refine (CD-03/CD-13,
lenses L6/L7):** the former separate `sensors.<name>` and `intake.*` namespaces
collapse into ONE `producers.<name>` registry with a `kind` discriminator (disko
subType dispatch) — `kind ∈ { calendar, build-effect, pool-reachability, gh,
events-dir, r2 }`, each `kind` a per-kind submodule emitting an `events/` record.
This faithfully renders july14 chat.md:1130 ("gh intake, the calendar source, the r2
scanner … are all just sensors, peers feeding the one queue"). ADDED: build→effect
(a producer kind, **cited:** chat.md:1153/1458 "an events-schema extension (a new
producer/event kind)") and pool-reachability (R16). gh intake COMPLETED (mutation
half; actor-exclude; `sources` enforced). *Supersedes:* the read-only gh intake; my
own wave-1 split of sensors/intake as parallel namespaces.

**R22 — Hercules parity: TAKE the trigger class, REFUSE the rest in writing.** TAKEN:
build→effect + exactly-once-per-key. REFUSED in writing: the containerized effect
sandbox, the state-file API, any coordination plane that is not the box. Offline-first
only. *Supersedes:* n/a.

**R23 — New CLI surface is additive; the SURVIVING frozen surface is byte-stable
[REWORDED wave-2, devil #10].** Additive: `tally witness append` (verb; `emit`
reserved for the exported wrapper helper name per CD-04 / chat.md:1456, R24);
`--parent` provenance (R25); `tally lease acquire`/`release`; the build→effect
producer kind. **Corrected wording:** it is FALSE that "every frozen verb/flag
survives untouched" (R18 cuts `--session` and the session/pane/agent groups; R19 cuts
Seam-B). The correct invariant is: *the SURVIVING frozen surface is byte-stable, and
every cut is an enumerated ruling.* The golden-oracle (R23-era) diffs only surviving
verbs/frames; cut surface is validated by ABSENCE, not diff; the net-new fleet
surface is NOT oracle-covered (R32 / BS-14). **The dominant acceptance test: Rust
`tally witness verify` passes on the Bun-era `witness.jsonl`.** *Supersedes:* the wave-1
overclaim.

---

## Wave-2 amendments (devil-1 findings + triage fold, 2026-07-15)

**R24 — TWO ledgers: the canonical verdict chain and the attestation chain (closes
devil #2 forgery; resolves CD-15).** `tally witness append`/`emit` — the one-way
platform arrow foreign units and advisory leaves wire into `OnSuccess`/`OnFailure` —
does NOT write the canonical `witness.jsonl`. It appends to a SEPARATE
`attestations.jsonl` with its own independent hash chain. Only the in-process
transactional core may write verdict-class lines to `witness.jsonl`; those are the
sole input to canonical `charge`/`gpu_seconds`/`standup` metering. Attestation lines
are advisory/observational, excluded from canonical metering, and `witness verify`
reports them on their own chain as unauthenticated-by-construction. This closes the
forge hole (a foreign unit can no longer mint a `pass` verdict that poisons metering)
WITHOUT touching the byte-for-byte canonicalization of the verdict chain — the
dominant test is untouched. It also resolves the CD-15 tension: task-0's advisory
report is an attestation line, not an `events/` enqueue (events/ is trigger-only
ingress, **cited:** src/triggers/events-dir.ts "produces ordinary in-daemon enqueues …
never a queue"). *Supersedes:* the wave-1 single-chain `witness append`.

**R25 — `--parent` provenance lives OFF the canonical chain (closes devil #3;
protects the dominant test).** `--parent` (auto-stamped from `TALLY_JOB_ID` in
transient units, and stamped on EVERY job-originated enqueue per R28) is recorded in
the taskchampion row (`parent_uuid` UDA), the `events/` payload, and the journald
`TALLY_PARENT` field — NEVER in the witness `canonicalHashInput`. Adding a field to
the verdict record would change canonicalization and break `verify` on the Bun
ledger, so provenance is carried by the row/journal join, not the chain.
*Supersedes:* the wave-1 unplaced `--parent`.

**R26 — Execution locus is coordinator-local; resource-loss verdict is broker-side
unreachability with hysteresis (closes devil #4).** The job-owning transient unit is
ALWAYS coordinator-local (**cited:** memory tally-dispatch-worker-model — "tally runs
jobs locally (systemd-run); worker-gpu = lease token not remote-exec; OCR-on-worker
via ssh-exec bridge"); even an ssh-exec-bridge leaf is owned by a coordinator-side
transient unit that shells to the worker. The worker owns ONLY the VRAM cgroup +
lease. Therefore: (a) the resource-loss verdict is set when the broker/remote pool is
unreachable mid-hold, requiring a SUSTAINED transition (N consecutive failed probes,
hysteresis) — never a single blip — to avoid the false-positive double-run
(open_risk 6); (b) recover() stays a PURE local-facts planner because the unit it
re-presents is local; (c) the local leaf, on losing its remote backend, typically
self-exits; the lease is released and the row marked resource-loss. Dedup-by-existence
+ epoch fencing guard against double-run if a blip was misclassified. Whether the
row is auto-re-presented on pool-return is CD-19 (Tom's call), NOT pre-decided here.
*Supersedes:* the wave-1 under-specified pool-vanished locus.

**R27 — Remote `enforce` is the BROKER's contract, negotiated, never a local check;
worker-side confinement is mechanized (closes devil #1 blocker + devil-2 #6; bakes
CD-16).** For a remote pool the coordinator MUST NOT stamp `DeviceMemoryMax` on its
own transient unit — that would confine nothing while appearing to (the silent
no-op). Confinement lands on the worker's serving-process cgroup, managed by the
worker's own sovereign instance (**cited:** july14 chat.md:1479 "the confinement half
… lands on the worker's side, on the cgroup of the serving process … Enforcement
happens at the appliance, not at the client"; colmena recon "admission and
resource-creation are one atomic RPC turn" on the owning host). **wave-3
mechanization (devil-2 #6):** because the VRAM-consuming process (llama-swap) is a
long-running FOREIGN service tally did not spawn, the worker-side pool declares a
`pools.<name>.servingSlice` — the systemd slice the foreign serving process runs
under. The worker's `nixosModules.tally` wires `Delegate=yes` on that slice, so the
worker instance OWNS the delegated cgroup and writes `DeviceMemoryMax` to it
out-of-band from any transient unit. The `LeaseBackend` handshake carries the
broker's ADVERTISED enforce backend + a live `DeviceMemoryMax`-present +
`dmem.current` read-back capability token; the coordinator REFUSES or downgrades a
`dmem`-enforced remote grant whose broker does not advertise it. The `enforce="dmem"`
assertion is therefore split: for a LOCAL pool it is the local kernel/systemd startup
assertion (R10); for a remote pool it is a NEGOTIATED capability check at grant time.
*Supersedes:* the wave-1 host-local-only assertion; the wave-2 un-mechanized
"worker stamps its own cgroup".

**R28 — Jobs MAY enqueue jobs; one-hop is a per-leaf CAPABILITY, not a global
admission ban [REWRITTEN wave-3, closes devil-2 #1 blocker].** The load-bearing law
is ONLY that the DAEMON never ORIGINATES work — NOT that jobs may not enqueue. Jobs
MAY enqueue jobs (**cited:** july14 chat.md:~1148 "Relax: jobs may enqueue jobs … a
research job that discovers twelve papers SHOULD enqueue twelve OCR jobs"; chat.md:1456
"the depth/fan-out guardrails enforce server-side. No behavioral change for any
existing caller"), which is exactly the OCR-firehose workload in the settled roster.
The admission path applies SERVER-SIDE GUARDRAILS to a job-originated `enqueue`
(detected via the `TALLY_JOB_ID` the caller carries): a mandatory `dedupKey`, a
per-parent fan-out cap, a depth cap (default 3, NOT 0), and the gh actor-exclude
rule; `--parent` is auto-stamped from `TALLY_JOB_ID` (R25) so every job-originated
enqueue is audited — which is precisely what makes R25 load-bearing rather than dead.
The "must NEVER enqueue" constraint is NOT global — it is a per-adapter/per-producer
`noEnqueue` capability flag carried ONLY by the specific ADVISORY recovery leaf
(task-0, R16). Thus "one-hop" names the recovery WORKFLOW's shape (an advisory
assessor that may not fan out), not an admission ban. *Supersedes:* the wave-2
blanket rejection, which contradicted the settled shape, broke the OCR firehose, and
made R25 dead code.

**R29 — Cooperative yield has an explicit channel and bounded timeouts (closes devil
#5).** Removing the SIGUSR1-into-terminal (R18) leaves a defined channel: a leaf
learns it must yield by POLLING its lease status (an `adapters.<name>.yieldHook`
optional probe the harness runs at checkpoints), on a bounded `yieldPollSec`. On an
`interrupt`-tier admission the daemon flags the lowest-rank same-pool holder to
yield; after a bounded `yieldGraceSec` with no release the lease is HARD-reclaimed
and the job is recorded `preempted` (a SECOND net-new verdict value distinct from
`failed`; its permanent NAME is surfaced alongside resource-loss in CD-01),
re-presentable. Both timeouts are typed options (CD-14, R31 note). Because this is a
behaviour change from the Bun oracle (which the artifact-diff rig cannot validate),
it is covered by a non-oracle conformance test asserting a low holder yields within N
seconds (BS-14). *Supersedes:* the wave-1 unspecified yield mechanism.

**R30 — Cross-host lease re-adoption is a DISTINCT path across the coordinator switch
(closes devil #7).** Local unit adoption (`systemctl --user show`) has no remote
equivalent. During the nightly switch the coordinator daemon is down for seconds and
its `RemoteLease.heartbeat` lapses; without protection the worker's reaper regrants
the single-capacity lease while the coordinator's `--collect`-kept leaf still uses
VRAM. Therefore: the worker holds a reaped-but-not-regranted lease through an
epoch-keyed GRACE WINDOW; on restart the coordinator re-adopts by presenting its
BUMPED `lease_epoch` (bumped on EVERY daemon start — graceful switch included, not
only crash); the worker refuses to regrant a lease whose adopted leaf is provably
still live (colmena boot-id-style epoch fencing). *Supersedes:* R17's wave-1
"same shape" conflation for remote leases.

**R31 — The synchronous transactional core is bounded and enumerated (closes devil
#12/#13; bakes CD-14 mechanism).** The fsync-before-ack transactional core touches
EXACTLY: the admission decision, the lease grant, and the verdict-witness
`fsync`. The taskchampion `Replica` commit is OUTSIDE that barrier (post-ack) and is
CRASH-SAFE precisely because the replica is a rebuildable cache (R13/PS#9) —
reconstructable from the acked witness `seq` + `events/`, so a stall (WAL checkpoint,
or a viewer lock) degrades to queued-not-stalled, never crash-looping the merged
process under `WatchdogSec`, and never loses acked work. External `task` access is
ReadOnly-enforced (R13) as write-race avoidance. Lease liveness for LOCAL leases is
systemd unit-liveness (`unit-exit/` reconciliation + `systemctl show`) — no bespoke
heartbeat; an actual heartbeat + `remoteHeartbeatSec`/`remoteReapSec` timeouts exist
ONLY for the cross-host `RemoteLease` path (**cited:** lens L6/L7 nix-refine on
CD-14). A slow-sqlite fault-injection test asserts the socket keeps accepting.
*Supersedes:* the wave-1 "keep the core small" discipline-without-boundary.

**R32 — Adapter capture and the scrape envelope are specified and bounded (closes
devil #8/#9).** The executor sets `StandardOutput`/`StandardError` to a per-job
captured stream (a `capture/<uuid>.out` file, equivalently read back from the unit's
journal by invocation id) so a DETACHED transient unit's stdout is available to the
scrape engine — without it `scrape.sessionRef` and thus adapter `resume()` (the
task-1 vehicle, R16) cannot work. Scrape is extended beyond a single regex:
`scrape.<captureName>` supports N named captures, a `jsonPath` extraction mode for
harnesses emitting structured JSON (claude-code), and a `stream` selector
(`stdout`|`stderr`). The EXPRESSIBLE ENVELOPE is documented as a sign-off item:
argv-launch + N-capture-scrape + templated multi-variable `resume` is in scope;
approval-gated/streaming-interactive harnesses (what the CUT detector handled) are
OUT of scope and named as such, so "new agent = no recompile" is a bounded, honest
claim. *Supersedes:* the wave-1 single-regex scrape.

### Triage-baked decisions (resolved by a lens; cited, so Tom never sees them)

- **CD-04** append/emit → `tally witness append` is the verb; `emit` is the exported
  wrapper name (R24; chat.md:1456).
- **CD-05** `enforce` default = `cooperative` (R9; chat.md:1317; dmem recon).
- **CD-06** two crates `tally-core` + `tally` (attic recon tally_nix_application).
- **CD-07** remote-negotiation port is a typed `types.port` option with ONE concrete
  default `7331` (a remote pool addresses a different host than the local broker, so
  a fixed default is cleaner than reusing `plsBroker.basePort`+offset). *(wave-3:
  committed to a single default per devil-2 #minor; the basePort+offset scheme is
  local-broker-only.)*
- **CD-08** `witness.jsonl` + `attestations.jsonl` → XDG_DATA_HOME (durable proof);
  `taskchampion.sqlite3` → XDG_DATA_HOME as a durable but REBUILDABLE cache (R13,
  not authoritative); `epoch`/`events/`/`unit-exit/`/`capture/` → XDG_STATE_HOME; the
  nixos system daemon uses systemd `LogsDirectory=tally` (witness) + `StateDirectory=
  tally` (mutable) instead of hand-resolved XDG (L6/L7).
- **CD-09** remote addressing folds INTO `pools.<name>.remote` (nullable submodule),
  NOT a parallel `remotePools.<name>` namespace (colmena NodeConfig 1:1 mirroring;
  L6/L7); `role` cut and `conductorHost` DROPPED ENTIRELY — reachability now lives in
  `pools.<name>.remote.host` (chat.md:1519 "conductor-ness becomes emergent").
  *(wave-3: conductorHost removed rather than retained, per devil-2 #minor.)*
- **CD-10** `pkgs.dmemcg-booster` own-overlay derivation (dmem recon).
- **CD-11** `predicate ∈ { co-residency, windowed-consumption }` (exact SPEC OUV-CM r1
  spelling), default `co-residency`; modelled as a niri-flake `attrTag` so
  `windowed-consumption` carries its own `windowSec` AND `consumptionCap` params (L6).
- **CD-12** `pollIntervalSec = 60` (Bun DEFAULT_POLL_INTERVAL_MS); `actorExclude =
  "self"` (chat.md:1244, verbatim module sketch).
- **CD-13** build→effect is a producer `kind` (chat.md:1153/1458), not a top-level
  option; watch ∈ { gc-roots-dir, jsonl, post-build-hook } (hercules/nix-eval-jobs
  recon).
- **CD-16** worker-side cgroup stamp via `pools.<name>.servingSlice` (see R27;
  chat.md:1479).
- **CD-18** `nixosModules.tally` auto-wires `Delegate=yes` + dmem `subtree_control`
  via `lib.any (p: p.enforce == "dmem") (attrValues cfg.pools)` (microvm.nix
  capability-branch idiom; L6/L7).
- **CD-20** flat `argv` + nullable `resume` + `scrape.*` + `extraConfig`; the
  niri-flake `sh` attrTag variant is INADMISSIBLE (frozen no-shell invariant), so no
  attrTag alternation (L6 correcting L5).
- **CD-21** `capacity = 1` default, configurable `>1` for co-residency (PLS_CAPACITY=1;
  hm-module.nix:48-52).
- **CD-22** the priority tier is one canonical enum reused by clap `ValueEnum` AND the
  Nix `types.enum`, validated by `tally --mode check-config` (single source of truth;
  L6/L7).
- **CD-23** `tally --mode check-config` build-time validator (attic `atticd --mode
  check-config` + `checkedConfigFile`).
- **CD-24** exported read-only wrapper store path `tally-witness-emit` (DECISIONS Q4
  kitty-watcher export pattern); writes the ATTESTATION chain (R24).
- **CD-25** barriers/wait-groups ride direct request/response RPC — already INTERNAL
  RPC in the frozen wire (oracle recon), structurally separate from cut Seam-B
  broadcast (SPEC OUV-MH R2; chat.md:1507/1515).

> **NOTE (wave-3):** CD-17 (native journald socket) was DE-BAKED and promoted to
> `decisions_for_tom` — it self-certified a deliberately-gated frozen decision
> (DECISIONS jul9's flip-back gate) on an invented internal requirement, and the cited
> frozen record (SPEC:682-689) partly REFUTES the "only MESSAGE" premise. Tom must
> confirm or overturn the gate; it is not the architect's to bake.

---

## Deliberation record

A terse log of what was stress-tested and why the residue is small.

**Wave 0 — consolidation.** The 2026-07-14 reshape chat
(`notes/july26-fable-second/july14/chat.md`) was distilled into the settled rulings
R1-R23, each stating what Bun-era behaviour it supersedes.

**Wave 1 — reboot fold.** Reboot / coordinator-switch recovery was folded in as three
GENERIC primitives (the resource-loss verdict, the `interrupt` tier, the
pool-reachability probe) rather than a workflow baked into the binary; the recovery
workflow stays a client leaf payload.

**Wave 2 — devil-1 (15 findings) + 7-lens decision triage.** Devil-1's blocker/major
findings became R24-R32 (two-ledger forgery close, `--parent` off-chain,
coordinator-local execution locus, remote-enforce negotiation, one-hop, the
cooperative-yield channel, cross-host lease re-adoption, the bounded transactional
core, the adapter capture envelope). Decision triage: **25 candidate decisions
(CD-01..CD-25) surfaced generously.** Seven source-lenses — the frozen tally shape
(SPEC / DECISIONS / CLI / prototype source), the july14 chat, and five prior-art
recons (attic, colmena, microvm.nix/disko/niri, dmem+kernel, taskchampion) — resolved
**22 with citations** (baked above), leaving 3 for Tom (CD-01, CD-02, CD-19). The
nix-maximalist lenses (L1-L7) forced **~12 nix-refinements**: sensors+intake unified
into one `producers.<name>` kind registry; `predicate` as an attrTag carrying its own
param; remote addressing folded into `pools.<name>.remote`; a single-source priority
enum; `--mode check-config`; the StateDirectory/LogsDirectory split; auto-`Delegate`;
and the no-`sh` adapter shape.

**Wave 3 — devil-2 (11 findings; a full citation audit that passed).** Three
BLOCKERS fixed: (1) R28's blanket enqueue ban — which contradicted the settled "jobs
may enqueue jobs" OCR-firehose shape (chat.md:~1148/1456) and made R25 dead — was
RELAXED to server-side guardrails (dedupKey + fan-out cap + depth cap 3 + gh actor
rule) plus a per-leaf `noEnqueue` capability; (2) the R13/R31 taskchampion
contradiction was RECONCILED by preserving frozen PS#9 ledger-as-truth (sqlite is a
rebuildable cache, not authoritative), which makes the post-ack commit crash-safe;
(3) the task-0→task-1 seam was SPECIFIED as recover() re-presenting the durable row
itself, plus a distinct `onReturnAttest` attestation for task-0. Five MAJORS folded:
`budget` split into typed `budgetGb` / `consumptionCap`; producer/enqueue field
precedence resolved (the enqueue payload owns pool/priority/adapter); the fleet
two-halves confinement mechanized via `pools.<name>.servingSlice`; CD-17 (native
journald) — the ONE invalid triage bake — PROMOTED to Tom; the second net-new verdict
`preempted` folded into CD-01. Minors: a single concrete remote-port default (7331),
`conductorHost` dropped entirely, and "retriable-on-return" softened to defer to
CD-19.

**Residue for Tom: 4** (CD-01 the two new verdict-value names; CD-02 the `interrupt`
tier's guarantee level; CD-17 native journald vs the frozen gate; CD-19 pool-return
auto-re-present vs mark-eligible). Every one is a permanent-identity choice or a
NOT-list-boundary policy call that no source-lens and no citation could settle — which
is exactly why so few reached him.
