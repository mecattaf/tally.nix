# tally.nix — NIX-SPEC (the sign-off gate)

> microvm.nix documentation caliber. Every option typed with default/description/
> example; generated units enumerated; the homeManager/nixos split drawn; the
> `enforce` enum grounded in pinned dmem facts. Wave-2 encoded the four blocker
> fixes (§2.1a remote enforce, §7a attestation ledger, §9 TALLY_* matrix, §10 core
> boundary) and unified sensors+intake into `producers.<name>` (§3). **Wave-3
> closes the devil-2 confirmability gaps:** the `budget` overload is split into typed
> `budgetGb`/`consumptionCap` (§2); producer/enqueue field ownership is de-duplicated
> with an explicit precedence (§3); the fleet confinement is mechanized via
> `servingSlice` (§2.1a); `onReturnAttest` distinguishes task-0's attestation from the
> resume enqueue (§3); the taskchampion cache is re-described as rebuildable, not
> authoritative (§1 dataDir); the remote-port default and conductorHost are settled.

---

## 0. The two modules and their split

| Module | Scope | Owns |
|---|---|---|
| `homeManagerModules.tally` | user lifecycle | daemon (`systemd --user`), CLI, pools/producers/adapters, drain, `cooperative`+`dmemcg-booster` backends, LoadCredential passthrough |
| `nixosModules.tally` (**un-stubbed**) | system scope | cgroup `Delegate=yes` on the user slice (auto when any pool `enforce="dmem"`, CD-18), `Delegate=yes` on any declared `pools.<name>.servingSlice` (worker-side confinement, R27), patched-systemd overlay for `enforce="dmem"`, `dmem.subtree_control`, and — for the system daemon — `StateDirectory=tally` + `LogsDirectory=tally` (CD-08) |

Auto-import (attic): `flake.nix` maps `./flake/*.nix` to flake-parts modules.
`crane.nix` builds one `tally` (+ `tally-static` via `pkgsStatic`). `checkedConfig`
runs `tally --mode check-config` in `runCommand` (CD-23) so a bad
pool/producer/adapter set fails `nixos-rebuild` at build time.

---

## 1. Top-level options

```nix
services.tally = {
  enable = mkEnableOption "the tally.nix impure-labor scheduler";

  package = mkOption { type = types.package; default = pkgs.tally;
    description = "The tally binary (daemon + CLI in one)."; };

  installTallydSymlink = mkOption { type = types.bool; default = true;
    description = "Install a `tallyd` argv[0] symlink for ps/journald legibility (cosmetics, R2)."; };

  # CD-08 — the DATA/STATE split is two clearly-named options (devil #11.6).
  dataDir = mkOption { type = types.path; default = "${config.xdg.dataHome}/tally";
    description = ''Durable proof + rebuildable cache: witness.jsonl (canonical
      verdict chain, R12), attestations.jsonl (advisory chain, R24), and
      taskchampion.sqlite3 (a durable but REBUILDABLE cache of pending state —
      reconstructable from witness+events; the witness chain is ledger-as-truth per
      PS#9/R13, so the sqlite file is NOT a second source of truth).'';
    example = "/home/tom/.local/share/tally"; };

  stateDir = mkOption { type = types.path; default = "${config.xdg.stateHome}/tally";
    description = ''Mutable/rebuildable runtime: lease_epoch counter, events/,
      unit-exit/, capture/. Distinct from dataDir; the system daemon uses systemd
      StateDirectory=tally instead (CD-08).'';
    example = "/home/tom/.local/state/tally"; };

  enqueue = mkOption {          # devil-2 #1 — job-originated fan-out guardrails (R28)
    type = types.submodule { options = {
      depthCap = mkOption { type = types.ints.positive; default = 3;
        description = "Max parent→child enqueue depth for a JOB-originated enqueue (R28). Jobs MAY enqueue; this bounds the chain."; };
      fanoutCap = mkOption { type = types.ints.positive; default = 64;
        description = "Max children a single parent job may enqueue (OCR-firehose bound, R28)."; };
      requireDedupKey = mkOption { type = types.bool; default = true;
        description = "Job-originated enqueues must carry a dedupKey (server-side, R28)."; };
    }; };
    default = {}; description = "Server-side guardrails on job-originated enqueue (R28). NOT a ban — jobs may enqueue jobs."; };

  lease = mkOption {           # devil #11.3 — the missing durability knob
    type = types.submodule { options = {
      remoteHeartbeatSec = mkOption { type = types.ints.positive; default = 15;
        description = "RemoteLease heartbeat cadence (host-to-host only; local leases use systemd unit-liveness, R31)."; };
      remoteReapSec = mkOption { type = types.ints.positive; default = 45;
        description = "Miss-3-beats reap timeout for a remote lease. Tunable — an aggressive value can reap a live interactive holder (CD-14)."; };
      graceSec = mkOption { type = types.ints.positive; default = 90;
        description = "Epoch-keyed grace window a worker holds a reaped-but-not-regranted lease across a coordinator switch (R30)."; };
      yieldPollSec = mkOption { type = types.ints.positive; default = 5;
        description = "Cadence a leaf polls its lease status for a cooperative-yield flag (R29)."; };
      yieldGraceSec = mkOption { type = types.ints.positive; default = 20;
        description = "After an interrupt admission, seconds a holder has to yield before HARD reclaim → verdict=preempted (R29)."; };
    }; };
    default = {}; description = "Lease timing knobs (R29/R30/R31)."; };

  patchedSystemd = mkOption {  # nixosModule; R10
    type = types.submodule { options = {
      enable = mkOption { type = types.bool; default = false; };
      pr37079Rev = mkOption { type = types.str; description = "Pinned commit on PR #37079 head (re-cut on force-push)."; };
      pr37079Hash = mkOption { type = types.str; };
    }; };
    default = {}; };
};
```

> `conductorHost` is deliberately ABSENT (CD-09, wave-3): conductor-ness is emergent
> from which host has a rendered config with producers (§7 `tally-daemon.service`
> ConditionPathExists); per-pool reachability lives in `pools.<name>.remote.host`.

---

## 2. `pools.<name>` — the generalized resource gate

`poolSubmodule`:

| Option | Type | Default | Description |
|---|---|---|---|
| `resource` | `enum [ "vram" "build-slot" "cpu-slot" "budget" ]` | `"vram"` | Scarce axis; `enforce="dmem"` applies only to `vram`. |
| `capacity` | `ints.positive` | `1` | Single-lease day-1 (PLS_CAPACITY=1, CD-21); >1 admits co-residency ≤ `budgetGb`. |
| `budgetGb` | `nullOr ints.positive` | `null` | Co-residency VRAM budget in GB (matches frozen hm-module.nix:52 `budgetGb`). Used ONLY when `capacity>1` on a `vram` pool. *(wave-3: replaces the untyped `either int str` overload, devil-2 #4.)* |
| `predicate` | `attrTag { co-residency = {}; windowed-consumption = { windowSec; consumptionCap; }; }` | `co-residency` | CD-11: exact SPEC spelling; `windowed-consumption` carries BOTH its window and its cap (invalid states unrepresentable). |
| `enforce` | `enum [ "cooperative" "dmemcg-booster" "dmem" ]` | `"cooperative"` | §2.1. |
| `priority` | `int` | `0` | Pool priority (lower served first). |
| `remote` | `nullOr (submodule remoteSubmodule)` | `null` | CD-09: remote addressing folded HERE, not a separate `remotePools.*`. `null` = local. |
| `servingSlice` | `nullOr str` | `null` | WORKER-side (R27/devil-2 #6): the systemd slice the FOREIGN VRAM-serving process (e.g. llama-swap) runs under. The nixos module wires `Delegate=yes` on it so the worker instance owns the cgroup and can write `DeviceMemoryMax` out-of-band. Meaningful only on the host that physically serves the pool. |
| `credentials` | `attrsOf str` | `{}` | name→path LoadCredential map (R11). |

`windowed-consumption` tag options: `windowSec` (`ints.positive`, the rolling window,
e.g. `604800` = 7d) and `consumptionCap` (`ints.positive`, the spend allowed within
the window, in the resource's native unit — seconds for time budgets, an integer
count for request budgets; NO free-form duration strings). *(wave-3: the `"5h"`
string form is removed; a duration is expressed as seconds, devil-2 #4.)*

`remoteSubmodule` (colmena NodeConfig mirror): `host` (str, required), `port`
(`types.port`, default `7331` — a single concrete default; a remote pool addresses a
different host than any local broker, CD-07/devil-2 #minor), `sshUser` (nullOr str,
`null`), `extraSshOptions` (listOf str, `[]`), and — read-only, negotiated at grant
time — the advertised `enforce` backend capability token (§2.1a).

### 2.1 The `enforce` enum, grounded (R9/R10)

| Value | Mechanism | Maturity | Generated effect |
|---|---|---|---|
| `cooperative` | tally-side bookkeeping of declared `--cost` | Always available, ship first | no device-memory property; CPUWeight/MemoryMax still always stamped |
| `dmemcg-booster` | `pkgs.dmemcg-booster` sets soft `dmem.low` hints | Real on 6.14+; advisory | module packages+enables the daemon; asserts kernel ≥ 6.14 |
| `dmem` | patched systemd sets hard `dmem.max` | Frontier (PR #37079 draft) | stamps `--property=DeviceMemoryMax` (local) or worker-side on `servingSlice` (remote, §2.1a); requires patched-systemd overlay; §2.1 assertion |

```nix
assertions = [{
  assertion = pool.enforce != "dmem" || pool.remote != null ||
    (config.services.tally.patchedSystemd.enable
     && lib.versionAtLeast config.boot.kernelPackages.kernel.version "6.14");
  message = ''
    pools.${name}.enforce = "dmem" on a LOCAL pool requires kernel >= 6.14 and
    services.tally.patchedSystemd.enable. dmem is frontier (LWN #1072437). The
    binary asserts the DeviceMemoryMax D-Bus property at startup and reads back
    dmem.current rather than trusting write success (R10).
  '';
}];
```

### 2.1a Remote `enforce` is a NEGOTIATED capability, mechanized on the worker (R27, devil #1 + devil-2 #6)

For `enforce = "dmem" && remote != null` the local assertion above is SKIPPED (Nix
cannot see the broker's kernel/systemd at eval time). Instead:

1. The COORDINATOR MUST NOT stamp `DeviceMemoryMax` on its own transient unit; the
   `LeaseBackend` handshake carries the broker's advertised capability token
   `{ deviceMemoryMax: bool, dmemCurrentReadback: bool }`. The grant path REFUSES or
   downgrades a `dmem` remote grant whose broker advertises `deviceMemoryMax = false`,
   and records the effective enforce level actually applied.
2. The WORKER applies confinement to the foreign serving process concretely: the
   worker-side pool declares `servingSlice` (the slice llama-swap runs under); the
   worker's `nixosModules.tally` sets `Delegate=yes` on that slice; the worker tally
   instance — which OWNS the delegated cgroup even though it did not spawn the
   process — writes `DeviceMemoryMax`/`dmem.max` to that slice's cgroup on grant and
   reads back `dmem.current`. Enforcement lands at the appliance, where the VRAM is
   allocated (chat.md:1479). *(wave-3: this replaces the un-mechanized "worker stamps
   its own cgroup", devil-2 #6.)*

### 2.2 Worked example

```nix
# On the COORDINATOR: a remote pool, negotiated, nothing stamped locally.
services.tally.pools = {
  worker-gpu   = { resource = "vram"; capacity = 1; enforce = "dmem";
                   remote = { host = "worker-tb"; port = 7331; }; };   # negotiated (§2.1a)
  worker-build = { resource = "build-slot"; capacity = 1; };
  api-claude   = { resource = "budget";
                   predicate.windowed-consumption = { windowSec = 604800; consumptionCap = 18000; }; # 5h in seconds
                   credentials.ANTHROPIC_API_KEY = config.age.secrets.anthropic.path; };
};

# On the WORKER (worker-tb): the SAME logical pool, locally enforced on the serving slice.
services.tally.pools.worker-gpu = {
  resource = "vram"; capacity = 1; enforce = "dmem";
  servingSlice = "llama-swap.slice";                 # worker owns this delegated cgroup (§2.1a)
};
```

---

## 3. `producers.<name>` — the ONE kind-tagged registry (R21; CD-03/CD-13)

The former separate `sensors.<name>` and `intake.*` namespaces are unified here
(disko subType dispatch). `evalModules` peeks `kind` and merges the kind-specific
submodule; every kind emits an `events/` record and generates its unit.

**Field-ownership rule (wave-3, devil-2 #5):** `pool`, `priority`, and `adapter` live
ONLY on the `enqueueSubmodule` (the payload that actually leases). They are NOT on
producer-common — there is no duplication and thus no precedence question. A producer
that enqueues carries an `enqueue` (or a kind-specific enqueue field); a pure sensor
that only narrows an existing event does not.

`producerSubmodule` common options:

| Option | Type | Default | Description |
|---|---|---|---|
| `kind` | `enum [ "calendar" "build-effect" "pool-reachability" "gh" "events-dir" "r2" ]` | required | Discriminator. |
| `credentials` | `attrsOf str` | `{}` | per-producer LoadCredential (R11). |

`enqueueSubmodule` (the payload; owns pool/priority/adapter):

| Option | Type | Default | Description |
|---|---|---|---|
| `argv` | `listOf str` | `[]` | Leaf argv, exec'd directly (no shell). |
| `adapter` | `str` | `"shell"` | Adapter preset (R20). |
| `pool` | `str` | required | Target pool the enqueued job leases. |
| `priority` | `enum [ "interrupt" "high" "medium" "low" ]` | `"low"` | Reserved top tier (R15/CD-22); one canonical enum, check-config-validated. |
| `dedupKey` | `nullOr str` | `null` | `strftime`-expanded existence key (mandatory for job-originated enqueue, R28). |
| `evidence` | `listOf str` | `[]` | `artifact:<path>`/`hash:<algo>`/`exit:<code>`. |
| `noEnqueue` | `bool` | `false` | R28/devil-2 #1: when true the leaf's admission token forbids further `enqueue` (the one-hop capability carried by the advisory recovery leaf, task-0). |
| `credentials` | `attrsOf str` | `{}` | — |

Per-`kind` submodules and their enqueue requirement:

- `calendar` (enqueues): `onCalendar` (str) → `systemd.user.timer` (`Persistent=true`)
  + oneshot writing an `enqueue` payload. `enqueue` REQUIRED.
- `build-effect` (R22; enqueues): `watch` (enum `[ "gc-roots-dir" "jsonl"
  "post-build-hook" ]`, default `gc-roots-dir`), `path` (path), `onKey` (an
  `enqueueSubmodule` fired once per distinct store path, devil #11.2). REQUIRED
  `onKey`. tally NEVER invokes `nix build`; it tails a store-path-keyed stream;
  exactly-once rides existing dedup+lease (store path IS the key).
- `pool-reachability` (R16/R26; conditional): `probePool` (str), `intervalSec` (int,
  default `30`), `hysteresis` (ints.positive, default `3` — consecutive failed probes
  before a transition, R26), `onLost` (nullOr enqueueSubmodule), `onReturn` (nullOr
  enqueueSubmodule), **`onReturnAttest`** (nullOr enqueueSubmodule carrying
  `noEnqueue = true`, R16/devil-2 #3 — the ADVISORY task-0 assessor that writes an
  attestation line and never fans out; DISTINCT from `onReturn`, which if set is a
  resume enqueue). Generated as a `Restart=always` supervised probe. **The resumed
  job (task-1) is normally reached NOT by `onReturn` but by recover() re-presenting
  the durable resource-loss row on the same pool-return (R16); `onReturn` is for the
  cases where a fresh job, not a row re-presentation, is wanted.**
- `gh` (R21; enqueues): `enable`, `sources` (listOf str, enforced), `actorExclude`
  (str, `"self"`, CD-12), `pollIntervalSec` (int, `60`, CD-12), `postEvidence` (bool,
  `false` — mutation half), `enqueue` (the mapping from an issue to a job). Auth is
  ambient `gh`.
- `events-dir` (pure ingress narrower, no static enqueue): the `events/` ingress
  narrower (same `validateEnqueueParams`; enqueue params come from the event FILE,
  not static config; atomic archive to done/rejected).
- `r2` (scanner; enqueues): the R2 scanner sensor; carries an `enqueue` template for
  the OCR jobs it emits.

Worked example — reboot-recovery wiring as pure config (R16/R26; no orchestration):

```nix
services.tally.producers.worker-gpu-health = {
  kind = "pool-reachability"; probePool = "worker-gpu"; intervalSec = 30; hysteresis = 3;
  onReturnAttest = {                 # task-0: advisory assessor, ATTESTATION line (R24), noEnqueue (R28)
    adapter = "shell"; priority = "interrupt"; pool = "controller-gpu";
    argv = [ "tally-recovery-assessor" ]; dedupKey = "recovery-assess-%Y%m%d%H";
    noEnqueue = true;
  };
  # task-1 (the actual resume) is recover() re-presenting the resource-loss ROW on
  # pool-return (R16); auto-vs-eligible per CD-19. No onReturn enqueue needed here.
};
```

The coordinator-switch trigger (R17/R30) needs no producer: `nixos-rebuild switch`
restarts `tally-daemon.service`, recover() fires, local units are adopted AND remote
leases are re-adopted via bumped `lease_epoch` (R30).

---

## 4. (folded into §3) — `sensors.*` and `intake.*` no longer exist as namespaces

Superseded by the `producers.<name>` kind registry (R21). The settled ruling's
"intake.*" capability maps onto kinds `{ gh, events-dir, r2 }`; "sensors" maps onto
`pool-reachability`. Coverage is proven in the matrix.

---

## 5. `adapters.<name>` — the inverted enum with a specified capture/scrape envelope (R20/R32)

`adapterSubmodule`:

| Option | Type | Default | Description |
|---|---|---|---|
| `argv` | `listOf str` | `[]` | Invocation template. NO shell semantics; no `sh` variant (CD-20). |
| `resume` | `nullOr (listOf str)` | `null` | Reattach template using `%<captureName>%`; `null` = no resume (shell). The task-1 vehicle (R16). Supports multiple captured variables. |
| `scrape` | `attrsOf (submodule scrapeCaptureSubmodule)` | `{}` | N NAMED captures (R32), not a single regex. |
| `yieldHook` | `nullOr (listOf str)` | `null` | Optional probe the harness runs at checkpoints to observe the cooperative-yield flag (R29). |
| `env` | `attrsOf str` | `{}` | — |
| `extraConfig` | `attrsOf raw` | `{}` | Freeform escape hatch. |

`scrapeCaptureSubmodule`: `stream` (enum `[ "stdout" "stderr" ]`, default `stdout`),
`mode` (enum `[ "regex" "jsonPath" ]`, default `regex`), `pattern` (str). The
executor captures the transient unit's stdout+stderr to `capture/<uuid>.{out,err}`
(read-back-able from the unit journal by invocation id) so a DETACHED unit's output
reaches the scrape engine (devil #8). Presets: `adapters.pi`, `adapters.claude-code`
(`scrape.sessionRef.mode = "jsonPath"`), `adapters.shell` (`resume = null`).

**Envelope boundary (sign-off item, R32/devil #9):** argv-launch + N-capture-scrape
(regex or jsonPath, stdout or stderr) + templated multi-variable `resume` is IN scope.
Approval-gated / streaming-interactive harnesses (what the CUT detector handled) are
OUT of scope — such an adapter still needs code, and this is stated so "new agent = no
recompile" is a bounded, honest claim.

---

## 6. (folded into §3) — gh intake is `producers.<name> = { kind = "gh"; ... }`

See §3. Auth is the machine's ambient `gh`; tally never manages credentials.

---

## 7. Generated systemd units (enumerated)

| Unit | Type | Trigger | Condition |
|---|---|---|---|
| `tally-daemon.service` | `Type=notify`+`WatchdogSec=`, Restart=always | wantedBy default.target | `ConditionPathExists=%h/.config/tally/config.json` (the concrete "conductor-emergent" mechanism, devil #11.5 — the unit starts on any host with a rendered config; conductor-ness is emergent from which config has producers) |
| `tally-producer-<name>.{timer,service}` | oneshot | OnCalendar | `ConditionPathExists` on events dir |
| `tally-producer-<name>.service` (pool-reachability) | simple, Restart=always | daemon-launched | — |
| `tally-drain.{timer,service}` | oneshot thin socket client | OnUnitActiveSec, Persistent=true | fails non-zero if socket absent |
| `tally-job-<uuid>` | transient (systemd-run) | dispatch | deterministic name for adoption; `StandardOutput`/`StandardError` → `capture/<uuid>` (R32) |
| `<pool>.servingSlice` (worker-side) | slice with `Delegate=yes` | declarative | worker owns the foreign serving-process cgroup for dmem (§2.1a/R27) |

Hardening (atticd `serviceConfig`): CPUWeight/MemoryMax always;
ProtectSystem/RestrictAddressFamilies/UMask=0077/SystemCallFilter on the daemon;
`Type=notify`+`sd_notify`+`WatchdogSec` gives the merged process a heartbeat (R2
blast-radius).

### 7a. The two ledgers (R24, devil #2)

`witness.jsonl` (canonical verdict chain) is written ONLY by the in-process
transactional core (R31). `attestations.jsonl` (independent chain) is written by
`tally witness append`/`emit`. The exported read-only wrapper `tally-witness-emit`
(CD-24, DECISIONS Q4 pattern) is what foreign units reference in
`OnSuccess=`/`OnFailure=` — it appends an ATTESTATION line, never a verdict line, so a
foreign unit cannot forge a `pass`/`gpu_seconds`. `witness verify` walks both chains
and reports attestations as unauthenticated-by-construction; canonical metering reads
only the verdict chain.

---

## 8. Conventions contract table

| Module option | Generated artifact | systemd-run consumer | Description |
|---|---|---|---|
| `pools.<n>.enforce="dmem"` (local) | `--property=DeviceMemoryMax=<GB>` | executor spawn line | hard VRAM cap (gated, R10) |
| `pools.<n>.enforce="dmem"` + `remote` | (nothing stamped locally) | negotiated at grant (§2.1a) | worker stamps its `servingSlice` cgroup (R27) |
| `pools.<n>.servingSlice` | slice `Delegate=yes` + worker-side `dmem.max` write | worker daemon (out-of-band) | foreign-service confinement (R27/§2.1a) |
| `pools.<n>.credentials.<K>` | `--property=LoadCredential=<K>:<path>` | executor spawn line | agenix secret → `$CREDENTIALS_DIRECTORY` (R11) |
| `producers.<n>.kind="calendar"` | `tally-producer-<n>.timer` | drain → events/ | recurring enqueue |
| `producers.<n>.kind="build-effect"` | store-path-tailing sensor | events/ per drv | Hercules build→effect (R21/R22) |
| `producers.<n>.kind="pool-reachability"` | `tally-producer-<n>.service` (Restart=always) | events/ on hysteresis transition | reboot-recovery trigger (R16/R26) |
| `producers.<n>.onReturnAttest` | attestation via `tally-witness-emit` | `tally witness append` | task-0 advisory assessor (R16/R24) |
| `adapters.<n>.resume` | `%<capture>%`-templated argv | executor spawn line | task-1 resume (R16/R32) |
| `adapters.<n>.scrape.<c>` | capture/<uuid> read | scrape engine | session_ref/model capture (R32) |
| `tally-witness-emit` export | `OnSuccess=`/`OnFailure=` line | `tally witness append` | attestation chain, not verdict (R24) |

Every net-new capability terminates in a generated artifact and a consuming flag.

---

## 9. The `TALLY_*` transient-unit env matrix (devil #11.4 — was unenumerated)

Injected by the executor into every `tally-job-<uuid>`; validated at emit time
(fail loudly on a missing proof-bearing field, BS-8):

| Var | Stage | Required-when | Notes |
|---|---|---|---|
| `TALLY_JOB_ID` | spawn | always | the signal that triggers job-originated-enqueue guardrails (R28) |
| `TALLY_TASK_UUID` | spawn | rowed jobs | null for live-orchestrator-spawned |
| `TALLY_PARENT` | spawn | when `--parent` set / job-originated enqueue | provenance carrier, OFF the witness chain (R25) |
| `TALLY_POOL` | spawn | always | — |
| `TALLY_LEASE_EPOCH` | spawn | always | fencing (R30) |
| `TALLY_CLASS` | spawn | always | priority tier (interrupt/high/medium/low) |
| `TALLY_NO_ENQUEUE` | spawn | when leaf carries `noEnqueue` | the one-hop capability for the advisory recovery leaf (R28) |
| `TALLY_CREDENTIALS` | spawn | when credentials set | NAMES only, never values (R11) |
| `CREDENTIALS_DIRECTORY` | runtime | when credentials set | systemd-provided |

---

## 10. The synchronous-core boundary (R31, devil #12)

The fsync-before-ack core touches EXACTLY: admission decision, lease grant,
verdict-witness `fsync`. Explicitly OUTSIDE the ack barrier: the taskchampion
`Replica` commit (post-ack, crash-safe because the replica is a REBUILDABLE cache of
pending state reconstructable from witness+events, R13/PS#9 — NOT authoritative),
attestation appends (R24), journald emission, scrape. External `task` viewer access
is ReadOnly-enforced as write-race avoidance. This is a sign-off invariant, verified
by a slow-sqlite fault-injection test (BS-14) asserting the socket keeps accepting
under a stalled replica commit.
