# tally.nix

**Declare your machine's impure work in Nix: leased systemd transient units backed by a
hash-chained, independently verifiable ledger of every outcome.**

tally leases the scarce resource, spawns the work, and proves what happened — so that a job
runs only when its resource is truly free, and every verdict is evidence you can verify
offline. One Rust binary (daemon + CLI), one pure-Nix module layer.

## Declares your labor the way the ecosystem declares everything else

| Module | Declares |
|--------|----------|
| [disko](https://github.com/nix-community/disko) | your disks |
| [agenix](https://github.com/ryantm/agenix) | your secrets |
| [microvm.nix](https://github.com/astro/microvm.nix) | your VMs |
| **tally.nix** | **your machine's impure, contended, evidence-bearing work** |

The novel part: **LLM runs, GPU holds, builds, and API-budget draws become first-class
NixOS citizens** — leased against real resource ceilings, spawned as systemd transient units,
and recorded in a ledger that anyone can verify without trusting tally.

## The one law

> tally tracks **contention** and **proof** — never **content** or **control**.

It arbitrates who may use a scarce resource, and it records verifiable evidence of what
happened. It never inspects what a job produces, and it never originates or drives work.

## What tally is NOT

- **Not a scheduler.** The daemon never invents jobs; work enters through declared producers
  or explicit enqueues.
- **Not a container or effect sandbox.** Jobs are ordinary systemd transient units.
- **Not a remote-execution engine.** Jobs run coordinator-local; a remote worker holds a
  lease and a cgroup, never a job.
- **Not a message bus.** journald is the stream; the socket carries only request/response RPC.
- **Not a secrets manager.** Credentials pass through by reference via `LoadCredential`;
  tally never reads secret bytes.
- **Not a terminal/session manager.** No pane, window, or agent-state detector in the core.
- **Not a model registry.** Model identifiers are recorded verbatim, never normalized.
- **Not a coordination plane.** The only coordination surface is the resource box.

## How it's built

- One flake, two crates (`tally-core` + `tally`), shipping a single binary that is both the
  daemon and the CLI. It shells out to exactly two programs: `systemd-run` and `gh`.
- `pls` is absorbed: the lease-grantor is the unit-spawner, in one transactional path.
- TaskChampion is embedded in-process as a rebuildable cache; the hash-chained witness ledger
  is the source of truth.
- A pure-Nix module layer declares pools, producers, and adapters. This is a greenfield
  rebuild of tally.

Full detail lives in **[`docs/SPEC.md`](docs/SPEC.md)** — the authoritative product
specification (architecture, the witness ledger, leased VRAM enforcement, priority and
preemption, reboot-aware recovery, the producer registry, and declarative adapters).
