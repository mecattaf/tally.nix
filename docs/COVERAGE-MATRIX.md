# tally.nix — COVERAGE MATRIX (100%, non-negotiable)

> This document maps every feature of the reference implementation — the prior Bun
> prototype, which is the golden-test oracle — to its target Rust module, with a
> disposition. It is a port map, not a proposal: each surviving element has exactly
> one row.
>
> **Dispositions:** `PORT` (carried into Rust as-is or extended) · `INVERT` (same
> behavior, re-expressed — e.g. a shell-out becomes in-process, an enum becomes
> declarative Nix) · `CUT` (dropped by design) · `ADD` (net-new surface with no Bun
> antecedent).

## Witness record fields → `witness` crate (all PORT)

| Field | Disposition | Note |
|---|---|---|
| task_uuid, transition_timestamp, verdict, exit_code | PORT | verdict enum extended: +resource-loss, +preempted, +pool-vanished |
| artifact_content_hash, gpu_seconds, wall_clock, attempt | PORT | — |
| lease_epoch, dedup_key, labor_class, trace_ref | PORT | lease_epoch bumped on every daemon start |
| pool, charge{unit,amount,class} | PORT | pools generalized (see Nix surface) |
| model | PORT | verbatim; no models.dev normalization |
| seq, prev_hash, hash | PORT | verbatim hash chain; `--parent` is recorded off-chain, not hashed |

## CLI verbs → `cli` / `wire`

| Verb group | Disposition | Target |
|---|---|---|
| `tally enqueue` + flags | PORT | wire; `--parent` recorded off-chain; job-originated enqueue allowed under guardrails |
| `queue cancel/pause/resume` | PORT | wire |
| `witness verify` | PORT | witness (walks both the verdict and attestation chains) |
| `witness append` / `emit` | ADD | attest — writes the attestation chain, never the verdict chain |
| `lease acquire` / `release` | ADD | lease (replaces `--session`) |
| `query status/log/render/standup` | PORT | journal, read-time join |
| `session/pane/agent *` | CUT | detector complex removed |
| `daemon run/drain` | PORT | daemon/producers |
| `pls-wrap`, `hooks install` | CUT | pls absorbed; hooks removed |
| `--priority interrupt` | ADD | reserved interrupt tier |
| `--mode check-config` | ADD | build-time validator |

## RPC methods → `wire`

| Methods | Disposition |
|---|---|
| `queue.enqueue/cancel/pause/resume/drain/await_job/await_barrier` | PORT (direct RPC; barriers ride this); enqueue gains server-side job-origination guardrails |
| `session.snapshot/subscribe/wait/ack/unsubscribe`, `pane.*`, `agent.*`, `kitty.*` | CUT |
| lease-negotiation RPC (host-to-host, incl. advertised-enforce capability token) | ADD |

## Subsystems → module + disposition

| Bun subsystem | Disposition | Target |
|---|---|---|
| witness hash-chain + verify + recover planner | PORT | witness, recover |
| attestation chain (foreign/leaf one-way arrow) | ADD | attest |
| TaskChampion `task export/import` shell-out | INVERT → in-process Replica (embedded, rebuildable cache) | taskdb |
| pls broker (Python, HTTP) | INVERT → absorbed into in-process lease; HTTP host-to-host only | lease |
| systemd-run TransientRunner (no props) | PORT + extend (CPUWeight/MemoryMax/dmem, LoadCredential, capture) | exec |
| remote-pool enforcement | ADD (negotiated capability + worker servingSlice stamp, never a local stamp) | exec/lease |
| evidence gate + dedup | PORT; verdict enum extended | evidence |
| priority queue (100/50/10) | PORT + extend (interrupt tier) | lease |
| cooperative preemption (SIGUSR1-into-unit) | INVERT → poll-a-lease-flag yield + hard-reclaim → preempted verdict | lease |
| job-originated enqueue | PORT + guardrails (depth/fanout/dedup/actor, `--parent` auto-stamp) | wire |
| one-hop (advisory recovery leaf) | ADD (per-leaf `noEnqueue` capability, not a global switch) | wire/exec |
| agent-state detector, session/pane model | CUT | — |
| Seam-B pub-sub delta stream | CUT | journald |
| journald TALLY_* emit | INVERT → native socket client (toggle-backed) | journal |
| adapter enum {pi,claude-code,shell} | INVERT → declarative Nix `adapters.<name>` w/ capture+scrape envelope | adapters |
| models.dev normalization | CUT | — |
| events-dir/drain/r2/gh sensors | PORT + unify into the producers kind registry | producers |
| gh intake (read-only) | PORT + complete (mutation, actor-exclude, sources) | producers kind=gh |
| build→effect trigger | ADD (Hercules parity, producer kind) | producers kind=build-effect |
| pool-reachability health probe | ADD (producer kind, hysteresis) | producers kind=pool-reachability |
| conductor/receiver roles | CUT (emergent; conductorHost dropped) | — |
| daemon supervisor loops | PORT + add sd_notify/WatchdogSec + bounded core | daemon |
| barrier / wait-groups | PORT (direct RPC) | daemon |
| charge/labor_class/gpu_seconds metering | PORT (verdict chain only) | witness/evidence |

## Nix module surface → coverage

| Bun module option | Disposition | Target |
|---|---|---|
| `enable`, `package` | PORT | top-level |
| `role`, `conductorHost` | CUT role; drop `conductorHost` (subsumed by `pools.<name>.remote.host`) | top-level |
| pool remote addr | PORT + fold into `pools.<name>.remote` (not `remotePools.*`) | pools |
| `pools[]` | PORT + generalize (resource/enforce/predicate/budgetGb/servingSlice/credentials/remote) | pools |
| `budget` (int) | PORT + split into typed `budgetGb` + `windowed-consumption.consumptionCap` | pools |
| `intake.gh`, `sessions`, `detector`, `installHooks` | CUT/fold — gh → producers kind=gh; sessions/detector cut | producers |
| `producers.<name>` (kind registry) | ADD — subsumes sensors + intake | producers |
| `enqueueSubmodule` (incl. `noEnqueue`), `buildEffect.onKey`, `pool-reachability.onReturnAttest` | ADD | producers |
| `enqueue.*` guardrails (depthCap/fanoutCap/requireDedupKey) | ADD | top-level |
| `lease.*` timeouts | ADD | top-level |
| `enforce` enum + `patchedSystemd` + `pkgs.dmemcg-booster` | ADD | pools; `enforce=dmem` is the target vram enforcement |
| `servingSlice` (worker-side dmem confinement) | ADD | pools/nixos |
| `credentials` LoadCredential | ADD | pools/producers |
| `dataDir` / `stateDir` split + StateDirectory/LogsDirectory | ADD | top-level |
| `adapters.<name>` (capture+scrape envelope) | ADD (inverted enum) | adapters |
| `tally-witness-emit` export | ADD (attestation) | module |
| `nixosModules.tally` stub | INVERT → un-stubbed (dmem/Delegate incl. servingSlice) | nixos module |

## Reboot-recovery + fleet net-new

| Piece | Disposition | Target |
|---|---|---|
| resource-loss verdict / evidence class | ADD | witness enum + evidence |
| `preempted` verdict | ADD | evidence |
| `pool-vanished` verdict | ADD | evidence |
| `interrupt` priority tier | ADD | lease |
| pool-reachability producer + hysteresis | ADD | producers |
| `recover()` pool-return row re-presentation | ADD | recover |
| advisory assessor (onReturnAttest, noEnqueue) | ADD | producers/attest |
| remote lease grace + epoch re-adoption | ADD | lease/recover |
| coordinator-switch = restart + local adopt + remote re-adopt | PORT + add | recover |
| worker servingSlice cgroup stamp | ADD | exec/nixos |
| fleet conformance suite | ADD (non-oracle gate) | conformance harness |

**Coverage proof:** every frozen element of the reference implementation has exactly
one row with an explicit disposition; every cut is a stated design decision, not an
omission; and every net-new verdict, tier, producer kind, and recovery piece
terminates in its own row. The verdict enum, the priority tiers, the producer kinds,
and the Nix option surface are each fully enumerated above.
