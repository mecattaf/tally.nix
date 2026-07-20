# tally.nix

**Contention arbitration and verifiable execution evidence for systemd-managed work.**

tally accepts explicitly described jobs, leases the local resources they need, runs them as
systemd transient units, and appends the outcome to a hash-chained witness ledger. It is useful
when builds, agents, GPU jobs, or metered API work must not overrun a shared machine.

The governing rule is:

> tally tracks **contention** and **proof**—never **content** or **control**.

tally decides whether declared work may run now. It does not inspect the work's domain output,
choose what work should happen next, or replace the program that already makes that choice.

## What tally is—and is not

tally provides:

- atomic leases over named local resource pools;
- cooperative priority and optional hard reclaim after a grace period;
- direct, shell-free execution through `systemd-run`;
- evidence checks, deduplication, recovery, and durable outcome records;
- a closed five-kind producer registry for declared intake mechanisms;
- an open adapter map for rendering agent-specific argv and scraping captures;
- Home Manager and NixOS modules; and
- offline verification of verdict and attestation ledgers.

tally is not a workflow scheduler, remote-execution service, container runtime, secrets manager,
message bus, terminal manager, or model registry. In particular, a pool and its transient jobs are
local to one daemon.

## Requirements

- Nix with flakes enabled;
- Linux with systemd for real job execution;
- a user systemd manager for the Home Manager module, or a system manager for the NixOS module;
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
job or none does. Pool resources are `vram`, `build-slot`, `cpu-slot`, `budget`, and `mutex`.

Two admission predicates exist:

- `co-residency` limits simultaneous holders by `capacity`; a multi-holder VRAM pool may also set
  `budgetGb`.
- `windowed-consumption` admits against `windowSec` and `consumptionCap`; it is valid only for a
  `budget` resource and may use a supervised external usage meter.

`enforce` accepts exactly `cooperative`. Priorities are `interrupt`, `high`, `medium`, and `low`,
with ranks 1000, 100, 50, and 10 respectively.

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
