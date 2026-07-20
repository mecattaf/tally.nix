# tally.nix

**Contention arbitration and verifiable execution evidence for systemd-managed work.**

tally accepts explicitly described jobs, leases the logical resource gates they need, runs them
as systemd transient units, and appends the outcome to a hash-chained witness ledger. It is useful
when builds, agents, GPU jobs, or metered API work must not overrun shared capacity.

The governing rule is:

> tally tracks **contention** and **proof**—never **content** or **control**.

tally decides whether declared work may run now. It does not inspect the work's domain output,
choose what work should happen next, or replace the program that already makes that choice.

## What tally is—and is not

tally provides:

- atomic leases over named, coordinator-owned resource pools;
- cooperative priority and optional hard reclaim after a grace period;
- direct, shell-free execution through local or daemonless remote `systemd-run`;
- named SSH executors with fail-closed reconnect and exact-unit re-adoption;
- evidence checks, deduplication, recovery, and durable outcome records;
- a closed five-kind producer registry for declared intake mechanisms;
- an open adapter map for rendering agent-specific argv and scraping captures;
- Home Manager and NixOS modules; and
- offline verification of verdict and attestation ledgers.

tally is not a workflow scheduler, general-purpose remote shell, container runtime, secrets
manager, message bus, terminal manager, or model registry. Each logical pool has one coordinator
daemon; that ownership identifies where contention is arbitrated, not where work is physically
placed. A named executor may place an admitted job on another host without moving lease ownership
or running a second tally daemon there.

## Requirements

- Nix with flakes enabled;
- Linux with systemd for real job execution;
- a user systemd manager for the Home Manager module, or a system manager for the NixOS module;
- for a remote executor, OpenSSH on the coordinator plus the same `tally` binary and a reachable
  user systemd manager on the worker;
- `gh` only when an enabled `gh` producer is configured.

Pure Rust tests use fakes where possible. The ordinary suite and both local scenarios need only
one machine.

## Quick start

Enter the development environment and build the project:

```console
$ nix develop
$ cargo test --workspace
$ cargo build --workspace
```

The flake exposes `packages.<system>.tally`, `packages.<system>.tally-witness-emit`,
`homeManagerModules.tally`, `nixosModules.tally`, adapter helpers under `lib.adapters`, and the
canonical priority table under `lib.priorityRanks`.

After enabling one of the modules below, enqueue a local command through the running daemon:

```console
$ tally --socket "$XDG_RUNTIME_DIR/tally/tally.sock" \
    enqueue --pool local --wait -- sh -c 'printf "done\n"'
```

The Home Manager socket is `$XDG_RUNTIME_DIR/tally/tally.sock`. The NixOS socket is
`/run/tally/tally.sock`.

## Home Manager module

Add the flake input and import the module in an existing Home Manager configuration:

```nix
{
  inputs.tally.url = "github:mecattaf/tally.nix";

  outputs = { home-manager, nixpkgs, tally, ... }: {
    homeConfigurations.example = home-manager.lib.homeManagerConfiguration {
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      modules = [
        tally.homeManagerModules.tally
        {
          home = {
            username = "example";
            homeDirectory = "/var/lib/example-home";
            stateVersion = "26.11";
          };

          services.tally = {
            enable = true;
            pools.local = {
              resource = "build-slot";
              capacity = 1;
              enforce = "cooperative";
            };
          };
        }
      ];
    };
  };
}
```

Home Manager writes the checked JSON configuration, starts `tally-daemon.service`, runs the
event-directory drain timer, creates declared producer and usage-meter units, and removes stale
managed units during activation. Durable data follows `XDG_DATA_HOME`; mutable state follows
`XDG_STATE_HOME`.

## NixOS module

Import the NixOS module into a host configuration:

```nix
{
  inputs.tally.url = "github:mecattaf/tally.nix";

  outputs = { nixpkgs, tally, ... }: {
    nixosConfigurations.example = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        tally.nixosModules.tally
        {
          system.stateVersion = "26.11";
          services.tally = {
            enable = true;
            pools.local = {
              resource = "build-slot";
              capacity = 1;
              enforce = "cooperative";
            };
          };
        }
      ];
    };
  };
}
```

The NixOS module writes `/etc/tally/config.json`, starts the hardened system daemon, and stores
data below `/var/lib/tally`. The Home Manager module is the module that generates producer timers
and supervised producer services.

## Resource pools

A job requests one or more named pools. Grants are atomic: either every requested pool admits the
job or none does. Repeat `--pool` on the CLI to request more than one. Pool sets are sorted
lexically before admission and retain that canonical order in durable state, events, queries, and
witnesses. Pool resources are `vram`, `build-slot`, `cpu-slot`, `budget`, and `mutex`.

The serialized field remains named `pool` for compatibility. Existing singleton configuration,
wire payloads, Taskwarrior rows, and witness records remain valid and keep their scalar encoding.
Only a multi-pool request uses the canonical array form (or a JSON array string in string-only
UDA and environment fields), so multi-pool clients require a daemon with multi-pool support.

Two admission predicates exist:

- `co-residency` limits simultaneous holders by `capacity`; a multi-holder VRAM pool may also set
  `budgetGb`.
- `windowed-consumption` admits against `windowSec` and `consumptionCap`; it is valid only for a
  `budget` resource and may use a supervised external usage meter.

`enforce` accepts exactly `cooperative`. Priorities are `interrupt`, `high`, `medium`, and `low`,
with ranks 1000, 100, 50, and 10 respectively.

## Central coordinator and remote workers

Configure a named SSH executor on the one host running the daemon, then select it per enqueue or
producer. Pools remain logical gates on the coordinator; they do not need to be local to the
machine that executes the argv.

```nix
services.tally = {
  enable = true;

  pools.worker-gpu = {
    resource = "vram";
    capacity = 1;
    hardPreempt = false;
  };

  executors.worker = {
    host = "worker.example.net";
    user = "tally-worker";
    identityFile = "/etc/tally/worker-key";
    knownHostsFile = "/etc/tally/worker-known-hosts";
    program = "/run/current-system/sw/bin/tally";
    stateDir = "/var/lib/tally-remote";
  };
};
```

```console
$ tally --socket "$XDG_RUNTIME_DIR/tally/tally.sock" enqueue \
    --pool worker-gpu --executor worker --evidence exit:0 -- \
    /run/current-system/sw/bin/worker-command 'one argv element'
```

The coordinator invokes only the fixed `tally __remote-executor` helper over SSH. Job argv travels
as bounded JSON on stdin and is passed directly to the worker's transient unit; it is never joined
into the SSH command or interpreted by an implicit shell. Host, user, port, client binary, private
key, pinned known-hosts file, worker binary, and worker state directory are explicit configuration.
The key and known-hosts paths must be readable by the coordinator service. Job credential paths
selected for remote work are worker-side paths and are passed to the worker's systemd manager by
reference.

The worker runs no tally daemon. It needs the configured binary, a usable user systemd manager,
and a private writable `stateDir`. The unit name is derived from the durable task UUID. `Ensure`,
`Probe`, and `Adopt` operations use that identity plus attempt, lease epoch, and systemd invocation
ID, so retrying after an SSH interruption cannot launch a second copy. Evidence for remote artifact
paths is evaluated on the worker; exit status, captures, evidence result, executor name, and pool
set return to the coordinator for its canonical witness.

Before creating a unit, the worker fsyncs its generation marker in `stateDir`. If that generation
later has neither a live unit nor a durable exit record, tally treats the state as an interrupted
prior launch and refuses to replay it. Keep `stateDir` on storage that survives worker restarts.

Transport ambiguity fails closed. The coordinator keeps every logical lease and retries the same
operation until the worker returns an authoritative state. On coordinator restart it probes every
durable remote row before accepting work, re-adopts an exact running invocation, and collects an
exact durable exit without replaying argv. A missing or contradictory unit, generation, invocation,
or protocol response stops recovery rather than releasing capacity or starting a replacement.

### Non-destructive thermal cooldown

A sensor on the worker can use a fixed SSH command to enqueue a coordinator-local hold. This
example gives the hold interrupt priority, waits behind any active `worker-gpu` holder, owns the
pool for 30 minutes, and then releases it:

```console
$ ssh coordinator tally --socket /run/user/1000/tally/tally.sock enqueue \
    --pool worker-gpu --priority interrupt --no-enqueue --evidence exit:0 -- \
    /run/current-system/sw/bin/sleep 1800
```

Keep `pools.worker-gpu.hardPreempt = false`. Interrupt priority puts the cooldown first in line,
but a non-hard-preempting pool never reclaims the active LLM unit, even after the yield grace. The
cooldown command deliberately omits `--executor`: the sleep only holds the coordinator's logical
gate, so no second daemon or worker process is needed for the hold itself.

## The five producer kinds

Producer names are user-defined, but every producer has exactly one of these five kinds:

| Kind | Declared observation |
|---|---|
| `calendar` | A systemd calendar firing emits one configured enqueue payload. |
| `build-effect` | A bounded path scan observes Nix store paths from a GC-roots directory, JSONL stream, or post-build hook stream. |
| `pool-reachability` | Repeated local pool observations produce hysteresis-confirmed loss and return transitions. |
| `gh` | Explicit GitHub notification/search sources are narrowed through the `gh` CLI. |
| `events-dir` | External integrations drop ordinary enqueue JSON files into the state events directory. |

All producer output passes through the same admission, deduplication, lease, evidence, and witness
path as manual enqueue. Producers do not add a second execution path.

## Adapters

Adapters are structured data, not a Rust enum. Each named adapter can define:

- a direct `argv` prefix for a fresh invocation;
- an optional direct `resume` template using `%<captureName>%` placeholders;
- named regex or RFC 9535 JSONPath scrapes from stdout or stderr;
- a direct cooperative `yieldHook` argv;
- non-reserved environment variables; and
- opaque JSON `extraConfig`.

The included presets are `shell`, `pi`, `claude-code`, and `codex`. The Codex launch prefix is
exactly `["codex", "exec", "--json", "--"]`. Custom adapters use
`tally.lib.adapters.mkAdapter` and need no Rust recompile.

Credentials are absolute source paths passed by name through systemd `LoadCredential=`. tally
records credential names but never reads or serializes their values.

## Evidence and ledgers

Evidence checks support `exit:<code>`, `artifact:<absolute-path>`, and SHA-256 checks over the
ordered artifact set. Artifact files are opened without following symlinks, bounded, hashed, and
checked after execution. The resulting verdict is appended to `witness.jsonl`; advisory records
from external unit hooks go to the separate `attestations.jsonl` chain and cannot create a
canonical job verdict.

Verify either ledger offline:

```console
$ tally witness verify /path/to/witness.jsonl
```

The TaskChampion SQLite database is a rebuildable query cache. Durable enqueue events and the
witness ledger remain authoritative across restart recovery.

## Tests and scenarios

Run the ordinary gates without a remote host:

```console
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command cargo test --workspace
$ nix develop --command cargo clippy --workspace --all-targets -- -D warnings
$ nix develop --command cargo fmt --all --check
$ nix flake check -L
```

Run the two single-machine scenarios:

```console
$ nix develop --command test/scenarios/run fanout-guardrail
$ nix develop --command test/scenarios/run slow-sqlite
```

The third scenario proves exact-row recovery after a real second machine disappears and returns:

```console
$ TALLY_TEST_REMOTE_HOST=example-host \
    nix develop --command test/scenarios/run pool-vanished/return
```

It copies the built package to the selected NixOS host, starts a transient user unit, and performs
an unattended reboot with `sudo -n systemctl reboot`. Use only a disposable test host prepared for
that action. With `TALLY_TEST_REMOTE_HOST` unset, the scenario prints a clear `SKIP` message and
exits successfully.

`TALLY_TEST_REMOTE_HOST` is also the explicit opt-in for ignored live-system Rust tests. Run those
tests on the selected NixOS host with `--ignored --nocapture`; with the variable absent they print
`SKIP` before touching systemd. `TALLY_BIN` can point the local scenarios at an already-built
binary, and `TALLY_PACKAGE` can point the multi-host scenario at an existing Nix store package.

See [the product specification](docs/SPEC.md), [the Nix interface](docs/NIX-SPEC.md), and
[the implementation map](docs/BUILD-SEQUENCE.md). Contributions are covered by
[CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT. See [LICENSE](LICENSE).
