# tally.nix — Nix module reference

Authoritative, self-contained option documentation for the `tally.nix` scheduler.
Every option is typed with a default, a description, and a worked example. All
generated systemd units are enumerated. The `homeManagerModules` / `nixosModules`
split is drawn explicitly. The `enforce` enum is grounded in concrete cgroup-dmem
facts.

This fleet (coordinator + worker) runs GPU pools with `enforce = "dmem"` on a
NixOS/AMD host. See [§2.1](#21-the-enforce-enum) for the dmem setup section.

---

## 0. The two modules and their split

| Module | Scope | Owns |
|---|---|---|
| `homeManagerModules.tally` | user lifecycle | daemon (`systemd --user`), CLI, pools/producers/adapters, drain, the `cooperative` and `dmemcg-booster` backends, LoadCredential passthrough |
| `nixosModules.tally` | system scope | cgroup `Delegate=yes` on the user slice (enabled automatically when any pool sets `enforce = "dmem"`), `Delegate=yes` on any declared `pools.<name>.servingSlice` (worker-side confinement), the patched-systemd overlay for `enforce = "dmem"`, `dmem.subtree_control` wiring, and — for the system daemon — `StateDirectory=tally` and `LogsDirectory=tally` |

`nixosModules.tally` is a full module, not a stub: it is required on any host that
runs `enforce = "dmem"` pools or that physically serves a remote pool.

Auto-import: `flake.nix` maps `./flake/*.nix` to flake-parts modules. `crane.nix`
builds one `tally` binary (daemon + CLI in one), plus `tally-static` via `pkgsStatic`.
`checkedConfig` runs `tally --mode check-config` inside `runCommand` so that a bad
pool/producer/adapter set fails `nixos-rebuild` at build time rather than at runtime.

---

## 1. Top-level options

```nix
services.tally = {
  enable = mkEnableOption "the tally.nix impure-labor scheduler";

  package = mkOption {
    type = types.package;
    default = pkgs.tally;
    description = "The tally binary (daemon + CLI in one).";
  };

  installTallydSymlink = mkOption {
    type = types.bool;
    default = true;
    description = "Install a `tallyd` argv[0] symlink for ps/journald legibility.";
  };

  dataDir = mkOption {
    type = types.path;
    default = "${config.xdg.dataHome}/tally";
    description = ''
      Durable proof plus rebuildable cache:
        witness.jsonl        canonical verdict chain (ledger-as-truth)
        attestations.jsonl   advisory attestation chain
        taskchampion.sqlite3 durable but REBUILDABLE cache of pending state,
                             reconstructable from witness + events. The witness
                             chain is the single source of truth; the sqlite file
                             is never a second source of truth.
    '';
    example = "/home/tom/.local/share/tally";
  };

  stateDir = mkOption {
    type = types.path;
    default = "${config.xdg.stateHome}/tally";
    description = ''
      Mutable/rebuildable runtime state: lease_epoch counter, events/, unit-exit/,
      capture/. Distinct from dataDir. The system daemon uses systemd
      StateDirectory=tally instead of this path.
    '';
    example = "/home/tom/.local/state/tally";
  };

  journald.native = mkOption {
    type = types.bool;
    default = false;
    description = ''
      Select the structured-logging path. Both paths are built into the binary;
      this toggle selects which one is active.

        false (default): the daemon and jobs log to stdout/stderr and systemd
          captures them via StandardOutput=journal. Portable, works on any host.

        true: the daemon emits structured records directly to journald over an
          AF_UNIX SOCK_DGRAM socket (native journal protocol), so structured
          fields (TALLY_JOB_ID, TALLY_POOL, verdict, gpu_seconds) land as journal
          fields without a stdout round-trip.
    '';
    example = true;
  };

  enqueue = mkOption {
    type = types.submodule {
      options = {
        depthCap = mkOption {
          type = types.ints.positive;
          default = 3;
          description = "Max parent->child enqueue depth for a job-originated enqueue. Jobs may enqueue; this bounds the chain.";
        };
        fanoutCap = mkOption {
          type = types.ints.positive;
          default = 64;
          description = "Max children a single parent job may enqueue (bounds an OCR-firehose fan-out).";
        };
        requireDedupKey = mkOption {
          type = types.bool;
          default = true;
          description = "Job-originated enqueues must carry a dedupKey (enforced server-side).";
        };
      };
    };
    default = {};
    description = "Server-side guardrails on job-originated enqueue. Not a ban: jobs may enqueue jobs.";
  };

  lease = mkOption {
    type = types.submodule {
      options = {
        remoteHeartbeatSec = mkOption {
          type = types.ints.positive;
          default = 15;
          description = "RemoteLease heartbeat cadence (host-to-host only; local leases use systemd unit liveness).";
        };
        remoteReapSec = mkOption {
          type = types.ints.positive;
          default = 45;
          description = "Miss-3-beats reap timeout for a remote lease. An aggressive value can reap a live interactive holder.";
        };
        graceSec = mkOption {
          type = types.ints.positive;
          default = 90;
          description = "Epoch-keyed grace window a worker holds a reaped-but-not-regranted lease across a coordinator switch.";
        };
        yieldPollSec = mkOption {
          type = types.ints.positive;
          default = 5;
          description = "Cadence a leaf polls its lease status for a cooperative-yield flag.";
        };
        yieldGraceSec = mkOption {
          type = types.ints.positive;
          default = 20;
          description = "After an interrupt admission, seconds a holder has to yield cooperatively before a HARD reclaim fires and the holder's verdict becomes preempted.";
        };
      };
    };
    default = {};
    description = "Lease timing knobs.";
  };

  patchedSystemd = mkOption {
    type = types.submodule {
      options = {
        enable = mkOption {
          type = types.bool;
          default = false;
          description = "Enable the patched-systemd overlay that provides the DeviceMemoryMax unit property. Required by local `enforce = \"dmem\"` pools.";
        };
        pr37079Rev = mkOption {
          type = types.str;
          description = "Pinned commit on the systemd PR #37079 head (re-cut on force-push).";
        };
        pr37079Hash = mkOption {
          type = types.str;
          description = "Fixed-output hash for the pinned overlay source.";
        };
      };
    };
    default = {};
    description = "nixosModule: the patched-systemd overlay providing DeviceMemoryMax.";
  };
};
```

There is no `conductorHost` option. Conductor-ness is emergent: a host is a
conductor when it has a rendered config that declares producers, gated by the
`tally-daemon.service` `ConditionPathExists` (§7). Per-pool reachability lives on
`pools.<name>.remote.host`.

---

## 2. `pools.<name>` — the generalized resource gate

`poolSubmodule`:

| Option | Type | Default | Description |
|---|---|---|---|
| `resource` | `enum [ "vram" "build-slot" "cpu-slot" "budget" ]` | `"vram"` | Scarce axis. `enforce = "dmem"` applies only to `vram`. |
| `capacity` | `ints.positive` | `1` | Single-lease by default; `> 1` admits co-residency up to `budgetGb`. |
| `budgetGb` | `nullOr ints.positive` | `null` | Co-residency VRAM budget in GB. Used only when `capacity > 1` on a `vram` pool. |
| `predicate` | `attrTag { co-residency = {}; windowed-consumption = { windowSec; consumptionCap; }; }` | `co-residency` | Admission predicate. `windowed-consumption` carries both its window and its cap, so invalid states are unrepresentable. |
| `enforce` | `enum [ "cooperative" "dmemcg-booster" "dmem" ]` | `"cooperative"` | GPU-memory enforcement backend. See [§2.1](#21-the-enforce-enum). |
| `hardPreempt` | `bool` | `false` | Preemption policy for this pool. `false` (default): an admitted `interrupt`-tier job waits `yieldGraceSec` for the holder to yield cooperatively. `true`: after `yieldGraceSec` the holder is hard-reclaimed and receives verdict `preempted`. |
| `autoResume` | `bool` | `true` for resource-loss pools (`vram`, and any pool with a `remote`), else `false` | On pool-return, re-present the durable `pool-vanished` row automatically so the interrupted work resumes without a fresh enqueue. |
| `priority` | `int` | `0` | Pool priority (lower is served first). Orthogonal to the per-job priority tier. |
| `remote` | `nullOr (submodule remoteSubmodule)` | `null` | Remote addressing folded here, not a separate `remotePools.*`. `null` means the pool is local. |
| `servingSlice` | `nullOr str` | `null` | Worker-side: the systemd slice the FOREIGN VRAM-serving process (e.g. llama-swap) runs under. The nixos module wires `Delegate=yes` on it so the worker instance owns the cgroup and can write `DeviceMemoryMax` out-of-band. Meaningful only on the host that physically serves the pool. |
| `credentials` | `attrsOf str` | `{}` | name -> path LoadCredential map. |

`windowed-consumption` tag options:

- `windowSec` (`ints.positive`) — the rolling window in seconds, e.g. `604800` for 7 days.
- `consumptionCap` (`ints.positive`) — the spend allowed within the window, in the
  resource's native unit: seconds for time budgets, an integer count for request
  budgets. Durations are always expressed as seconds; there are no free-form
  duration strings.

`remoteSubmodule` (colmena NodeConfig mirror):

- `host` (`str`, required) — the serving host.
- `port` (`types.port`, default `7331`) — the broker port on the serving host.
- `sshUser` (`nullOr str`, default `null`).
- `extraSshOptions` (`listOf str`, default `[]`).
- (read-only, negotiated at grant time) the advertised `enforce` backend capability
  token, see [§2.1a](#21a-remote-enforcement).

### Priority tiers

Every enqueued job carries a priority tier (`producers.<name>` enqueue payload, §3):

| Tier | Rank | Meaning |
|---|---|---|
| `interrupt` | 1000 | Reserved top tier, above all normal work. An `interrupt` job is admitted ahead of any `high`/`medium`/`low` holder and drives the yield/preempt path governed by `yieldGraceSec` and per-pool `hardPreempt`. |
| `high` | 30 | |
| `medium` | 20 | |
| `low` | 10 | Default. |

Preemption is best-effort cooperative by default: an admitted `interrupt` job sets
the yield flag, the holder observes it (via `lease.yieldPollSec` / the adapter
`yieldHook`) and yields within `lease.yieldGraceSec`. A pool opts into hard
preemption with `hardPreempt = true`, in which case a holder that has not yielded by
the grace deadline is reclaimed and its verdict is recorded as `preempted`.

### 2.1 The `enforce` enum

`enforce` selects the GPU-memory enforcement backend for a `vram` pool.

| Value | Mechanism | Availability | Generated effect |
|---|---|---|---|
| `cooperative` | tally-side bookkeeping of declared `--cost` | Always available, stock nixpkgs | No device-memory property is stamped. `CPUWeight`/`MemoryMax` are still always stamped. Portable default. |
| `dmemcg-booster` | `pkgs.dmemcg-booster` sets soft `dmem.low` hints | Real on kernel >= 6.14; advisory only | Module packages and enables the booster daemon; asserts kernel >= 6.14. |
| `dmem` | patched systemd sets a hard `dmem.max` | Production backend on this fleet | Stamps `--property=DeviceMemoryMax=<GB>` on the local transient unit, or applies the write worker-side on `servingSlice` for a remote pool (§2.1a). Requires the patched-systemd overlay and a dmem-capable kernel. |

**This fleet runs `enforce = "dmem"` on its vram pools.** `cooperative` is documented
and kept as the portable fallback for stock nixpkgs, but the coordinator and worker
in this deployment enforce hard VRAM caps via dmem.

#### dmem production setup (NixOS / AMD host)

`dmem` places a hard device-memory ceiling on a GPU-serving process using the kernel
DMEM cgroup controller plus a systemd unit property that writes it. Two host
prerequisites are met on this fleet and are stated here as first-class setup steps:

1. **Kernel: `CONFIG_CGROUP_DMEM` enabled, amdgpu registered with the DMEM controller.**
   The DMEM cgroup controller lands the `dmem.max` / `dmem.current` files under a
   delegated cgroup; the amdgpu driver registers its VRAM region with the controller
   so those files meter real VRAM. Kernel >= 6.14.

2. **Patched-systemd overlay providing `DeviceMemoryMax`.** Stock systemd does not
   yet expose a unit property that writes `dmem.max`. Enable the overlay:

   ```nix
   services.tally.patchedSystemd = {
     enable = true;
     pr37079Rev  = "…";   # pinned commit on the systemd PR #37079 head
     pr37079Hash = "…";
   };
   ```

   With the overlay active, `systemd-run --property=DeviceMemoryMax=<bytes>` writes
   `dmem.max` on the transient unit's cgroup.

3. **cgroup delegation wiring (nixosModule).** The nixos module sets `Delegate=yes`
   on the relevant slice and enables `dmem` in that slice's `dmem.subtree_control`,
   so the tally instance owns the cgroup subtree and is permitted to write `dmem.max`
   on child units.

4. **Startup assertion and read-back.** At startup the binary asserts that the
   `DeviceMemoryMax` D-Bus property is settable, and on every grant it reads back
   `dmem.current` from the cgroup rather than trusting write success. A grant whose
   read-back does not reflect the requested ceiling fails loudly.

The build-time assertion for a local dmem pool:

```nix
assertions = [{
  assertion = pool.enforce != "dmem" || pool.remote != null ||
    (config.services.tally.patchedSystemd.enable
     && lib.versionAtLeast config.boot.kernelPackages.kernel.version "6.14");
  message = ''
    pools.${name}.enforce = "dmem" on a LOCAL pool requires kernel >= 6.14 and
    services.tally.patchedSystemd.enable. The binary asserts the DeviceMemoryMax
    D-Bus property at startup and reads back dmem.current rather than trusting
    write success.
  '';
}];
```

### 2.1a Remote enforcement

Remote enforcement (`enforce = "dmem"` with `remote != null`) is a negotiated
capability, mechanized on the worker. The local build-time assertion above is
skipped for a remote pool, because Nix cannot see the broker's kernel or systemd at
evaluation time. Instead:

1. **The coordinator stamps nothing locally.** The `LeaseBackend` handshake carries
   the broker's advertised capability token `{ deviceMemoryMax: bool,
   dmemCurrentReadback: bool }`. The grant path refuses or downgrades a `dmem`
   remote grant whose broker advertises `deviceMemoryMax = false`, and records the
   effective enforce level actually applied.

2. **The worker confines the foreign serving process concretely.** The worker-side
   pool declares `servingSlice` (the slice llama-swap runs under). The worker's
   `nixosModules.tally` sets `Delegate=yes` on that slice. The worker tally instance
   — which owns the delegated cgroup even though it did not spawn the process —
   writes `DeviceMemoryMax` / `dmem.max` to that slice's cgroup on grant and reads
   back `dmem.current`. Enforcement lands at the appliance, where the VRAM is
   actually allocated.

### 2.2 Worked example

```nix
# On the COORDINATOR: a remote vram pool, negotiated, nothing stamped locally.
services.tally.pools = {
  worker-gpu   = {
    resource = "vram"; capacity = 1; enforce = "dmem";
    hardPreempt = true;                                  # reclaim non-yielding holders
    remote = { host = "worker-tb"; port = 7331; };       # negotiated (§2.1a)
  };
  worker-build = { resource = "build-slot"; capacity = 1; };
  api-claude   = {
    resource = "budget";
    predicate.windowed-consumption = { windowSec = 604800; consumptionCap = 18000; }; # 5h expressed in seconds
    credentials.ANTHROPIC_API_KEY = config.age.secrets.anthropic.path;
  };
};

# On the WORKER (worker-tb): the SAME logical pool, locally enforced on the serving slice.
services.tally.pools.worker-gpu = {
  resource = "vram"; capacity = 1; enforce = "dmem";
  servingSlice = "llama-swap.slice";                     # worker owns this delegated cgroup (§2.1a)
};
```

---

## 3. `producers.<name>` — the kind-tagged registry

`producers.<name>` is the single registry for everything that observes the world and
optionally enqueues work. Sensors and intake share this namespace. `evalModules`
peeks `kind` and merges the kind-specific submodule; every kind emits an `events/`
record and generates its unit.

**Field-ownership rule.** `pool`, `priority`, and `adapter` live only on the
`enqueueSubmodule` (the payload that actually leases). They are not on
producer-common, so there is no duplication and no precedence question. A producer
that enqueues carries an `enqueue` (or a kind-specific enqueue field); a pure sensor
that only narrows an existing event does not.

`producerSubmodule` common options:

| Option | Type | Default | Description |
|---|---|---|---|
| `kind` | `enum [ "calendar" "build-effect" "pool-reachability" "gh" "events-dir" "r2" ]` | required | Discriminator. |
| `credentials` | `attrsOf str` | `{}` | Per-producer LoadCredential map. |

`enqueueSubmodule` (the payload; owns pool/priority/adapter):

| Option | Type | Default | Description |
|---|---|---|---|
| `argv` | `listOf str` | `[]` | Leaf argv, exec'd directly (no shell). |
| `adapter` | `str` | `"shell"` | Adapter preset. |
| `pool` | `str` | required | Target pool the enqueued job leases. |
| `priority` | `enum [ "interrupt" "high" "medium" "low" ]` | `"low"` | Priority tier (§2). One canonical enum, validated at check-config. |
| `dedupKey` | `nullOr str` | `null` | `strftime`-expanded existence key. Mandatory for a job-originated enqueue. |
| `evidence` | `listOf str` | `[]` | Entries of the form `artifact:<path>` / `hash:<algo>` / `exit:<code>`. |
| `noEnqueue` | `bool` | `false` | When true the leaf's admission token forbids any further `enqueue` (the one-hop capability carried by the advisory recovery leaf, task-0). |
| `credentials` | `attrsOf str` | `{}` | Per-leaf LoadCredential map. |

Per-`kind` submodules and their enqueue requirement:

- **`calendar`** (enqueues): `onCalendar` (`str`) generates a `systemd.user.timer`
  (`Persistent=true`) plus a oneshot writing an `enqueue` payload. `enqueue` is
  required.
- **`build-effect`** (enqueues): `watch` (enum `[ "gc-roots-dir" "jsonl"
  "post-build-hook" ]`, default `gc-roots-dir`), `path` (`path`), `onKey` (an
  `enqueueSubmodule` fired once per distinct store path). `onKey` is required. tally
  never invokes `nix build`; it tails a store-path-keyed stream, and exactly-once
  rides the existing dedup + lease because the store path is the key.
- **`pool-reachability`** (conditional): `probePool` (`str`), `intervalSec` (`int`,
  default `30`), `hysteresis` (`ints.positive`, default `3` — consecutive failed
  probes before a transition), `onLost` (`nullOr enqueueSubmodule`), `onReturn`
  (`nullOr enqueueSubmodule`), `onReturnAttest` (`nullOr enqueueSubmodule` carrying
  `noEnqueue = true` — the advisory task-0 assessor that writes an attestation line
  and never fans out, distinct from `onReturn`). Generated as a `Restart=always`
  supervised probe. When the pool has `autoResume = true`, the interrupted job
  (task-1) is resumed by `recover()` re-presenting the durable resource-loss row on
  pool-return; `onReturn` is only for the cases where a fresh job — not a row
  re-presentation — is wanted.
- **`gh`** (enqueues): `enable` (`bool`), `sources` (`listOf str`, enforced),
  `actorExclude` (`str`, default `"self"`), `pollIntervalSec` (`int`, default `60`),
  `postEvidence` (`bool`, default `false` — the mutation half), `enqueue` (the
  mapping from an issue to a job). Auth is the machine's ambient `gh`.
- **`events-dir`** (pure ingress narrower, no static enqueue): the `events/` ingress
  narrower. It runs the same `validateEnqueueParams`, but the enqueue params come
  from the event file, not from static config; processed files are atomically
  archived to `done/` or `rejected/`.
- **`r2`** (scanner; enqueues): the R2 scanner. It carries an `enqueue` template for
  the OCR jobs it emits.

Worked example — reboot-recovery wiring as pure config, no orchestration:

```nix
services.tally.producers.worker-gpu-health = {
  kind = "pool-reachability"; probePool = "worker-gpu"; intervalSec = 30; hysteresis = 3;
  onReturnAttest = {                 # task-0: advisory assessor, writes an ATTESTATION line, noEnqueue
    adapter = "shell"; priority = "interrupt"; pool = "controller-gpu";
    argv = [ "tally-recovery-assessor" ]; dedupKey = "recovery-assess-%Y%m%d%H";
    noEnqueue = true;
  };
  # task-1 (the actual resume) is recover() re-presenting the resource-loss ROW on
  # pool-return, because worker-gpu has autoResume = true. No onReturn enqueue needed.
};
```

A coordinator switch needs no producer: `nixos-rebuild switch` restarts
`tally-daemon.service`, `recover()` fires, local units are adopted, and remote
leases are re-adopted via a bumped `lease_epoch`.

---

## 4. `adapters.<name>` — argv-launch with a capture/scrape envelope

`adapterSubmodule`:

| Option | Type | Default | Description |
|---|---|---|---|
| `argv` | `listOf str` | `[]` | Invocation template. No shell semantics; there is no `sh` variant. |
| `resume` | `nullOr (listOf str)` | `null` | Reattach template using `%<captureName>%`. `null` means no resume (e.g. `shell`). This is the task-1 resume vehicle and supports multiple captured variables. |
| `scrape` | `attrsOf (submodule scrapeCaptureSubmodule)` | `{}` | N named captures, not a single regex. |
| `yieldHook` | `nullOr (listOf str)` | `null` | Optional probe the harness runs at checkpoints to observe the cooperative-yield flag. |
| `env` | `attrsOf str` | `{}` | Extra environment for the adapter. |
| `extraConfig` | `attrsOf raw` | `{}` | Freeform escape hatch. |

`scrapeCaptureSubmodule`:

- `stream` (enum `[ "stdout" "stderr" ]`, default `stdout`).
- `mode` (enum `[ "regex" "jsonPath" ]`, default `regex`).
- `pattern` (`str`).

The executor captures the transient unit's stdout and stderr to
`capture/<uuid>.{out,err}` (also read-back-able from the unit journal by invocation
id) so that a detached unit's output still reaches the scrape engine.

Presets: `adapters.pi`, `adapters.claude-code` (`scrape.sessionRef.mode =
"jsonPath"`), `adapters.shell` (`resume = null`).

**Envelope boundary.** In scope: argv-launch plus N-capture scrape (regex or
jsonPath, stdout or stderr) plus a templated multi-variable `resume`.
Approval-gated / streaming-interactive harnesses are out of scope — such an adapter
still requires code, so "new agent = no recompile" is a bounded claim that holds
only within this envelope.

---

## 5. Generated systemd units

| Unit | Type | Trigger | Condition |
|---|---|---|---|
| `tally-daemon.service` | `Type=notify` + `WatchdogSec=`, `Restart=always` | `wantedBy` default.target | `ConditionPathExists=%h/.config/tally/config.json` — the unit starts on any host with a rendered config; conductor-ness is emergent from which config declares producers |
| `tally-producer-<name>.{timer,service}` | oneshot | `OnCalendar` | `ConditionPathExists` on the events dir |
| `tally-producer-<name>.service` (pool-reachability) | simple, `Restart=always` | daemon-launched | — |
| `tally-drain.{timer,service}` | oneshot thin socket client | `OnUnitActiveSec`, `Persistent=true` | fails non-zero if the socket is absent |
| `tally-job-<uuid>` | transient (`systemd-run`) | dispatch | deterministic name for adoption; `StandardOutput`/`StandardError` route to `capture/<uuid>` |
| `<pool>.servingSlice` (worker-side) | slice with `Delegate=yes` | declarative | worker owns the foreign serving-process cgroup for dmem (§2.1a) |

Hardening (`serviceConfig`): `CPUWeight` / `MemoryMax` are always stamped;
`ProtectSystem` / `RestrictAddressFamilies` / `UMask=0077` / `SystemCallFilter` on
the daemon; `Type=notify` + `sd_notify` + `WatchdogSec` give the merged process a
heartbeat to bound the blast radius of a hang.

### 5a. The two ledgers

`witness.jsonl` (canonical verdict chain) is written only by the in-process
transactional core. `attestations.jsonl` (an independent chain) is written by `tally
witness append` / `emit`. The exported read-only wrapper `tally-witness-emit` is
what foreign units reference in `OnSuccess=` / `OnFailure=`: it appends an
attestation line, never a verdict line, so a foreign unit cannot forge a `pass` or a
`gpu_seconds`. `witness verify` walks both chains and reports attestations as
unauthenticated-by-construction; canonical metering reads only the verdict chain.

---

## 6. Conventions contract table

Every net-new capability terminates in a generated artifact and a consuming flag.

| Module option | Generated artifact | systemd-run consumer | Description |
|---|---|---|---|
| `pools.<n>.enforce = "dmem"` (local) | `--property=DeviceMemoryMax=<GB>` | executor spawn line | hard VRAM cap (gated) |
| `pools.<n>.enforce = "dmem"` + `remote` | (nothing stamped locally) | negotiated at grant (§2.1a) | worker stamps its `servingSlice` cgroup |
| `pools.<n>.servingSlice` | slice `Delegate=yes` + worker-side `dmem.max` write | worker daemon (out-of-band) | foreign-service confinement |
| `pools.<n>.hardPreempt` | reclaim on grace-deadline | executor reclaim path | non-yielding holder gets verdict `preempted` |
| `pools.<n>.autoResume` | re-presented resource-loss row | recover() | pool-return resume without a fresh enqueue |
| `pools.<n>.credentials.<K>` | `--property=LoadCredential=<K>:<path>` | executor spawn line | agenix secret -> `$CREDENTIALS_DIRECTORY` |
| `producers.<n>.kind = "calendar"` | `tally-producer-<n>.timer` | drain -> events/ | recurring enqueue |
| `producers.<n>.kind = "build-effect"` | store-path-tailing sensor | events/ per drv | build -> effect |
| `producers.<n>.kind = "pool-reachability"` | `tally-producer-<n>.service` (`Restart=always`) | events/ on hysteresis transition | reboot-recovery trigger |
| `producers.<n>.onReturnAttest` | attestation via `tally-witness-emit` | `tally witness append` | task-0 advisory assessor |
| `adapters.<n>.resume` | `%<capture>%`-templated argv | executor spawn line | task-1 resume |
| `adapters.<n>.scrape.<c>` | `capture/<uuid>` read | scrape engine | session_ref / model capture |
| `journald.native = true` | AF_UNIX SOCK_DGRAM journal emitter | daemon + jobs | structured fields without a stdout round-trip |
| `tally-witness-emit` export | `OnSuccess=` / `OnFailure=` line | `tally witness append` | attestation chain, not verdict |

---

## 7. The `TALLY_*` transient-unit env matrix

Injected by the executor into every `tally-job-<uuid>` and validated at emit time
(a missing proof-bearing field fails loudly):

| Var | Stage | Required when | Notes |
|---|---|---|---|
| `TALLY_JOB_ID` | spawn | always | the signal that triggers job-originated-enqueue guardrails |
| `TALLY_TASK_UUID` | spawn | rowed jobs | null for a live-orchestrator-spawned job |
| `TALLY_PARENT` | spawn | when `--parent` is set / a job-originated enqueue | provenance carrier, off the witness chain |
| `TALLY_POOL` | spawn | always | — |
| `TALLY_LEASE_EPOCH` | spawn | always | fencing token |
| `TALLY_CLASS` | spawn | always | priority tier (`interrupt` / `high` / `medium` / `low`) |
| `TALLY_NO_ENQUEUE` | spawn | when the leaf carries `noEnqueue` | the one-hop capability for the advisory recovery leaf |
| `TALLY_CREDENTIALS` | spawn | when credentials are set | names only, never values |
| `CREDENTIALS_DIRECTORY` | runtime | when credentials are set | systemd-provided |

---

## 8. The synchronous-core boundary

The fsync-before-ack core touches exactly: the admission decision, the lease grant,
and the verdict-witness `fsync`. Explicitly outside the ack barrier:

- the taskchampion `Replica` commit (post-ack, crash-safe because the replica is a
  rebuildable cache of pending state reconstructable from witness + events, not
  authoritative);
- attestation appends;
- journald emission;
- scrape.

External `task` viewer access is ReadOnly-enforced to avoid a write race. This is a
sign-off invariant, verified by a slow-sqlite fault-injection test asserting the
socket keeps accepting under a stalled replica commit.

---

## 9. Build-time configuration check

`checkedConfig` runs `tally --mode check-config` inside `runCommand`, so the full
pool / producer / adapter / adapter-reference graph is validated at build time and a
bad config fails `nixos-rebuild` rather than surfacing at runtime. The check covers:
priority-tier enum membership, pool references from every enqueue payload, adapter
references, dmem prerequisites for local pools, and the enqueue guardrail fields.
