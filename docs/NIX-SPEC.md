# tally Nix interface specification

This is the implemented contract of the flake and its modules. Types, defaults, assertions, unit
names, and examples below are grounded in `flake.nix` and `nix/`.

## 0. Flake outputs

For each default flake system, tally exports:

- `packages.<system>.tally` and `packages.<system>.default`—the combined daemon and CLI;
- `packages.<system>.tally-witness-emit`—the advisory attestation wrapper;
- `apps.<system>.default` and a mock `apps.<system>.dev`;
- `devShells.<system>.default`;
- `checks.<system>.*`;
- `homeManagerModules.tally`;
- `nixosModules.tally`;
- `lib.adapters.{mkAdapter,mkScrapeCapture,presets}`;
- `lib.priorityRanks`; and
- `lib.tallyWitnessUnitHooks` with ready-to-use `OnSuccess` and `OnFailure` values.

The package installs `tally` and, by default, a `tallyd` symlink to the same executable.

## 1. Common `services.tally` options

Both modules expose the same typed option tree.

| Option | Type | Default | Meaning |
|---|---|---|---|
| `enable` | `bool` | `false` | Enable the daemon and module artifacts. |
| `package` | `package` | this flake's `tally` | Combined CLI/daemon package. |
| `installTallydSymlink` | `bool` | `true` | Include the `tallyd` argv-0 alias in the installed package. |
| `dataDir` | `path` | module-specific | Witnesses, attestations, and TaskChampion data. |
| `stateDir` | `path` | module-specific | Events, captures, exit records, epochs, and producer state. |
| `journald.native` | `bool` | `false` | Emit native journal datagrams instead of JSON stdout events. |
| `enqueue.depthCap` | positive integer | `3` | Maximum parent-to-child enqueue depth. |
| `enqueue.fanoutCap` | positive integer | `64` | Maximum accepted children for one parent. |
| `enqueue.requireDedupKey` | `bool` | `true` | Require a key for job-originated enqueue. |
| `lease.graceSec` | positive integer | `90` | Epoch-keyed restart recovery grace. |
| `lease.yieldPollSec` | positive integer | `5` | Cooperative-yield polling cadence. |
| `lease.yieldGraceSec` | positive integer | `20` | Grace before an opted-in hard reclaim. |
| `pools` | attribute set of pool submodules | `{}` | Named local resource gates. |
| `producers` | attribute set of producer submodules | `{}` | Closed five-kind intake registry. |
| `adapters` | attribute set of adapter submodules | presets | Open structured process envelopes. |

Home Manager defaults `dataDir` to `${config.xdg.dataHome}/tally` and `stateDir` to
`${config.xdg.stateHome}/tally`. NixOS defaults them to `/var/lib/tally/data` and
`/var/lib/tally/state`.

Every enabled module renders a checked JSON configuration. Nix assertions catch typed graph errors,
then the real `tally --mode check-config` parser validates the serialized result at build time.

## 2. `pools.<name>`

| Option | Type | Default | Meaning |
|---|---|---|---|
| `resource` | `vram`, `build-slot`, `cpu-slot`, `budget`, or `mutex` | `vram` | Scarce-resource axis. |
| `capacity` | positive integer | `1` | Maximum co-resident holders. |
| `budgetGb` | null or positive integer | `null` | Aggregate GB limit for a multi-holder VRAM pool. |
| `predicate.co-residency` | empty tagged submodule | selected | Counted simultaneous admission. |
| `predicate.windowed-consumption.windowSec` | positive integer | `604800` | Consumption window. |
| `predicate.windowed-consumption.consumptionCap` | positive integer | `1` | Authoritative cap in the resource's native unit. |
| `enforce` | `cooperative` | `cooperative` | Complete enforcement enum. |
| `hardPreempt` | `bool` | `false` | Reclaim a non-yielding lower-priority holder after the grace. |
| `autoResume` | null or `bool` | `null` | Override same-row return recovery; null uses the resource default. |
| `priority` | integer | `0` | Pool ordering rank; lower values are considered first. |
| `credentials` | attribute set of absolute paths | `{}` | Credential references inherited by jobs. |
| `usageMeter` | null or usage-meter submodule | `null` | Supervised external observations for a windowed budget. |

Exactly one predicate tag is selected. The default is `co-residency`.

Cross-option assertions enforce these rules:

- a mutex uses co-residency and capacity one;
- `budgetGb` is valid only for co-resident `vram` with capacity greater than one;
- windowed consumption uses `resource = "budget"`; and
- a usage meter exists only on a windowed-consumption budget.

`usageMeter` has direct `argv`, `pollIntervalSec = 120`, and the sole `budgetClass =
"programmatic"`. Pool credential names must be valid systemd credential components. Source paths
must be absolute.

Priorities are shared between Rust and Nix and are checked during the flake build:

| Name | Rank |
|---|---:|
| `interrupt` | 1000 |
| `high` | 100 |
| `medium` | 50 |
| `low` | 10 |

## 3. `producers.<name>`

`kind` is required and accepts exactly `calendar`, `build-effect`, `pool-reachability`, `gh`, or
`events-dir`. Missing and unknown discriminators produce named assertion messages. Every kind also
has a `credentials` attribute set passed by reference to its generated Home Manager unit.

### `calendar`

- `onCalendar`: systemd calendar expression, default `daily`.
- `enqueue`: required enqueue payload emitted at each firing.

Home Manager generates `tally-producer-<name>.service` and `.timer`. The timer uses
`OnCalendar=<expression>` and `Persistent=true`.

### `events-dir`

- `pollIntervalSec`: positive integer, default `60`.

Home Manager generates an oneshot service and a timer with both `OnActiveSec=1s` and
`OnUnitActiveSec=<pollIntervalSec>`. The initial trigger is intentional: a recurring-only timer has
no first firing on a fresh user manager.

### `gh`

- `enable`: boolean, default `false`.
- `sources`: list containing `notifications` and/or `search`, default empty.
- `actorExclude`: nonempty string, default `self`.
- `pollIntervalSec`: positive integer, default `60`.
- `postEvidence`: boolean, default `false`.
- `enqueue`: required enqueue payload.

An enabled producer requires at least one unique source. Home Manager generates a supervised
service with `Restart=always`, no start-rate limit, and the polling interval as `RestartSec`.

### `build-effect`

- `watch`: `gc-roots-dir`, `jsonl`, or `post-build-hook`; default `gc-roots-dir`.
- `path`: absolute observed path; default `/var/empty/tally-build-effects`.
- `onKey`: required enqueue payload for a distinct store path.

Home Manager generates a supervised service with `Restart=always`, no start-rate limit, and a
five-second restart cadence.

### `pool-reachability`

- `probePool`: required configured pool name.
- `intervalSec`: positive integer, default `30`.
- `hysteresis`: positive integer, default `3`.
- `onLost`: null or enqueue payload, default null.
- `onReturn`: null or enqueue payload, default null.
- `onReturnAttest`: null or enqueue payload, default null; when present, `noEnqueue` must be true.

Only one reachability producer may own a pool. All enqueue payloads must reference known pools and
adapters. Home Manager generates a supervised service with `Restart=always`, no start-rate limit,
and `RestartSec=<intervalSec>`.

## 4. Producer enqueue payloads

The `enqueue`, `onKey`, `onLost`, `onReturn`, and `onReturnAttest` fields share one type.

| Field | Type | Default |
|---|---|---|
| `argv` | list of strings | `[]`, but must be nonempty |
| `adapter` | string | `shell` |
| `pool` | string | empty, but must name a configured pool |
| `priority` | `interrupt`, `high`, `medium`, or `low` | `low` |
| `dedupKey` | null or string | `null` |
| `evidence` | list of strings | `[]` |
| `evidenceClass` | JSON-serializable raw value | `null` |
| `manifestHash` | null or string | `null` |
| `consumptionEstimate` | null or unsigned integer | `null` |
| `runtimeMaxSec` | null or positive integer | `null` |
| `noEnqueue` | boolean | `false` |
| `credentials` | attribute set of absolute paths | `{}` |

`argv` remains an array through rendering and execution. There is no shell-string form.

## 5. `adapters.<name>`

| Option | Type | Default | Meaning |
|---|---|---|---|
| `argv` | list of strings | `[]` | Direct prefix for fresh execution. |
| `resume` | null or list of strings | `null` | Direct template using `%<captureName>%`. |
| `scrape` | attribute set of capture submodules | `{}` | Named capture extraction. |
| `yieldHook` | null or list of strings | `null` | Direct cooperative checkpoint argv. |
| `env` | attribute set of strings | `{}` | Non-reserved process environment. |
| `extraConfig` | JSON-serializable raw attribute set | `{}` | Adapter-specific data. |

A scrape selects `stream = "stdout"` or `"stderr"`, `mode = "regex"` or `"jsonPath"`, and a
nonempty `pattern`. Adapter environment names cannot begin with `TALLY_` and cannot replace
`CREDENTIALS_DIRECTORY`.

The preset names are `shell`, `pi`, `claude-code`, and `codex`. Their definitions live entirely in
`nix/lib/adapters.nix`. In particular, the Codex fresh argv is frozen as:

```json
["codex", "exec", "--json", "--"]
```

Custom direct-argv integrations use `lib.adapters.mkAdapter`; custom captures use
`lib.adapters.mkScrapeCapture`.

## 6. Home Manager artifacts

When enabled, the Home Manager module:

- installs the selected package and `tally-witness-emit`;
- creates private data/state directories;
- writes `$XDG_CONFIG_HOME/tally/config.json`;
- starts `tally-daemon.service` as `Type=notify` with watchdog and restart;
- owns `$XDG_RUNTIME_DIR/tally/tally.sock`;
- creates `tally-drain.service` and `.timer`;
- creates producer and usage-meter services/timers described above;
- creates `tally-witness-emit@.service`; and
- runs `tally-clean-removed-producers.service` during activation.

The drain timer uses both `OnActiveSec=1s` and `OnUnitActiveSec=5s`. The stock-host VM check boots a
fresh system and proves this timer and an `events-dir` producer timer fire without a manual service
start.

The daemon service is hardened with a private runtime directory, `UMask=0077`, CPU and memory
limits, `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`, explicit writable paths, Unix-only
address families, and a system-service syscall filter.

## 7. NixOS artifacts

When enabled, the NixOS module:

- installs the selected package and `tally-witness-emit` system-wide;
- writes `/etc/tally/config.json`;
- creates `/run/tally`, `/var/lib/tally`, and `/var/log/tally` through systemd directory options;
- starts hardened `tally-daemon.service` as `Type=notify` with watchdog and restart; and
- creates `tally-witness-emit@.service`.

The NixOS module does not generate the Home Manager producer timers or supervised producer units.
Its socket is `/run/tally/tally.sock`.

## 8. Credentials, transient jobs, and witnesses

Pool, producer, and enqueue credentials render as `LoadCredential=<name>:<absolute-source>`.
Generated metadata contains credential names only. The process reads values through systemd's
`CREDENTIALS_DIRECTORY`.

Transient `tally-job-<uuid>.service` units receive the applicable variables from this matrix:

| Variable | Meaning |
|---|---|
| `TALLY_JOB_ID` | Unique execution identity. |
| `TALLY_TASK_UUID` | Durable row identity when one exists. |
| `TALLY_PARENT` | Job-originated enqueue provenance. |
| `TALLY_POOL` | Granted pool. |
| `TALLY_LEASE_EPOCH` | Restart fence. |
| `TALLY_ATTEMPT` | Attempt number. |
| `TALLY_CLASS` | Priority tier. |
| `TALLY_NO_ENQUEUE` | One-hop enqueue capability removal. |
| `TALLY_CREDENTIALS` | JSON list of credential names. |
| `TALLY_SOCKET` | Socket used by cooperative hooks. |
| `TALLY_YIELD_HOOK` | Serialized direct hook argv when configured. |

Optional proof-bearing variables are explicitly unset when absent.

`lib.tallyWitnessUnitHooks` expands to:

```nix
{
  OnSuccess = [ "tally-witness-emit@success:%n.service" ];
  OnFailure = [ "tally-witness-emit@failure:%n.service" ];
}
```

These hooks append advisory attestations, never canonical verdicts.

## 9. Minimal configurations

Home Manager module fragment:

```nix
{
  imports = [ inputs.tally.homeManagerModules.tally ];

  services.tally = {
    enable = true;
    pools.local = {
      resource = "build-slot";
      capacity = 1;
      enforce = "cooperative";
    };
  };
}
```

NixOS module fragment:

```nix
{
  imports = [ inputs.tally.nixosModules.tally ];

  services.tally = {
    enable = true;
    pools.local = {
      resource = "build-slot";
      capacity = 1;
      enforce = "cooperative";
    };
  };
}
```

## 10. Deferred—not implemented

The following names describe one previously considered direction. They are not module options,
runtime variants, flake overlays, generated units, or compatibility placeholders:

- alternative enforcement values such as `enforce = "dmem"` or `"dmemcg-booster"`;
- a patched-systemd overlay, device-memory controller properties, or automatic `Delegate=` wiring;
- `servingSlice` and worker-side serving slices;
- `remote`, `remoteSubmodule`, `remoteHeartbeatSec`, and `remoteReapSec`; and
- cross-machine lease ownership or re-adoption.

Configuration containing those fields or values is rejected. The implemented resource surface is
local and `enforce` accepts exactly `cooperative`. This section documents scope only; it does not
reserve an enum value or declare future compatibility.
