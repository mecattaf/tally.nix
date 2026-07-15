# tally.nix — COVERAGE MATRIX (100%, non-negotiable)

> Every Bun feature / CLI verb / witness field → target module → `port|cut|invert|add`.
> Wave-2 rows added for the devil-1 fixes and triage bakes; wave-3 rows/edits for the
> devil-2 corrections (enqueue-guardrails, taskchampion-as-cache, servingSlice,
> onReturnAttest, budgetGb/consumptionCap).

## Witness record fields → `witness` crate (all PORT verbatim, R12)

| Field | Disposition | Note |
|---|---|---|
| task_uuid, transition_timestamp, verdict, exit_code | port | verdict enum EXTENDED (+resource-loss value, R14/CD-01; +preempted, R29/CD-01) |
| artifact_content_hash, gpu_seconds, wall_clock, attempt | port | — |
| lease_epoch, dedup_key, labor_class, trace_ref | port | lease_epoch bumped every daemon start (R30) |
| pool, charge{unit,amount,class} | port | pools generalized (R8) |
| model | port verbatim | models.dev normalization CUT (R20) |
| seq, prev_hash, hash | port VERBATIM | dominant test (R23); `--parent` NOT added here (R25) |

## CLI verbs → `cli`/`wire`

| Verb group | Disposition | Target |
|---|---|---|
| `tally enqueue` + flags | port | wire; `--parent` ADDED off-chain (R25); job-originated allowed under guardrails (R28) |
| `queue cancel/pause/resume` | port | wire |
| `witness verify` | port (dominant) | witness (walks both chains, R24) |
| `witness append`/`emit` | add | attest — writes attestation chain, NOT verdict (R24) |
| `lease acquire`/`release` | add | lease (replaces `--session`, R18) |
| `query status/log/render/standup` | port | journal read-time join |
| `session/pane/agent *` | cut | R18 |
| `daemon run/drain` | port | daemon/producers |
| `pls-wrap`, `hooks install` | cut | R5/R20 |
| `--priority interrupt` | add | reserved tier (R15/CD-22) |
| `--mode check-config` | add | build-time validator (CD-23) |

## RPC methods → `wire`

| Methods | Disposition |
|---|---|
| `queue.enqueue/cancel/pause/resume/drain/await_job/await_barrier` | port (direct RPC; barriers ride this, R25/CD-25); enqueue gains server-side job-origination guardrails (R28) |
| `session.snapshot/subscribe/wait/ack/unsubscribe`, `pane.*`, `agent.*`, `kitty.*` | cut (R18/R19) |
| lease-negotiation RPC (host-to-host, incl. advertised-enforce capability token) | add (R6/R27) |

## Subsystems → module + disposition

| Bun subsystem | Disposition | Target |
|---|---|---|
| witness hash-chain + verify + recover planner | port | witness, recover |
| attestation chain (foreign/leaf one-way arrow) | add | attest (R24) |
| TaskChampion `task export/import` shell-out | invert → in-process Replica (a rebuildable cache, PS#9/R13) | taskdb (R3) |
| pls broker (Python, HTTP) | invert → in-process lease | lease (R5); HTTP host-to-host only (R6) |
| systemd-run TransientRunner (no props) | port + extend (CPUWeight/MemoryMax/dmem, LoadCredential, capture) | exec (R7/R11/R32) |
| remote-pool enforcement | add (negotiated capability + worker servingSlice stamp, never local stamp) | exec/lease (R27) |
| evidence gate + dedup | port; verdict EXTENDED | evidence (R14/R29) |
| priority queue (100/50/10) | port + extend (interrupt tier) | lease (R15) |
| cooperative preemption (SIGUSR1-into-unit) | invert → poll-a-lease-flag yield + hard-reclaim → preempted | lease (R29) |
| job-originated enqueue | port + add guardrails (depth/fanout/dedup/actor, --parent auto-stamp); NOT a ban | wire (R28) |
| one-hop (advisory recovery leaf) | add (per-leaf `noEnqueue` capability, NOT global) | wire/exec (R28) |
| agent-state detector, session/pane model | cut | R18 |
| Seam-B pub-sub delta stream | cut | journald (R19) |
| journald TALLY_* emit | invert → native socket client (CONTESTED, CD-17 → Tom; toggle-backed) | journal |
| adapter enum {pi,claude-code,shell} | invert → Nix adapters.<name> w/ capture+scrape envelope | adapters (R20/R32) |
| models.dev normalization | cut | R20 |
| events-dir/drain/r2/gh sensors | port + UNIFY into producers kind registry | producers (R21) |
| gh intake (read-only) | port + complete (mutation, actor-exclude, sources) | producers kind=gh (R21) |
| build→effect trigger | add (Hercules parity, producer kind) | producers kind=build-effect (R22) |
| pool-reachability health probe | add (producer kind, hysteresis) | producers kind=pool-reachability (R16/R26) |
| conductor/receiver roles | cut (emergent; conductorHost DROPPED, CD-09) | R20 |
| daemon supervisor loops | port + add sd_notify/WatchdogSec + bounded core | daemon (R31) |
| barrier/wait-groups | port (direct RPC) | daemon |
| charge/labor_class/gpu_seconds metering | port (verdict chain only) | witness/evidence (R24) |

## Nix module surface → coverage

| Bun module option | Disposition | Target |
|---|---|---|
| `enable`, `package` | port | top-level |
| `role`, `conductorHost` | cut role; DROP conductorHost (subsumed by pools.<name>.remote.host, CD-09) | R20/CD-09 |
| pool remote addr | port + fold into `pools.<name>.remote` (not remotePools.*) | pools (CD-09) |
| `pools[]` | port + generalize (resource/enforce/predicate/budgetGb/servingSlice/credentials/remote) | pools (R8/R9/R11/R27/CD-11) |
| `budget` (int) | port + split into typed `budgetGb` + windowed-consumption.consumptionCap | pools (devil-2 #4) |
| `intake.gh`, `sessions`, `detector`, `installHooks` | cut/fold — gh→producers kind=gh; sessions/detector cut | producers (R18/R21) |
| `producers.<name>` (kind registry) | add — subsumes sensors + intake | producers (R21/CD-03/CD-13) |
| `enqueueSubmodule` (incl. `noEnqueue`), `buildEffect.onKey`, `pool-reachability.onReturnAttest` | add (were undefined/added wave-3) | producers (devil #11.1/2, devil-2 #3/#5) |
| `enqueue.*` guardrails (depthCap/fanoutCap/requireDedupKey) | add | top-level (R28) |
| `lease.*` timeouts | add | top-level (CD-14/R29/R30) |
| `enforce` enum + `patchedSystemd` + `pkgs.dmemcg-booster` | add | pools (R9/R10/CD-10) |
| `servingSlice` (worker-side dmem confinement) | add | pools/nixos (R27/§2.1a) |
| `credentials` LoadCredential | add | pools/producers (R11) |
| `dataDir`/`stateDir` split + StateDirectory/LogsDirectory | add (naming fixed) | top-level (CD-08) |
| `adapters.<name>` (capture+scrape envelope) | add (inverted enum) | adapters (R20/R32) |
| `tally-witness-emit` export | add (attestation) | module (R24/CD-24) |
| `nixosModules.tally` stub | invert → un-stubbed (dmem/Delegate incl. servingSlice, CD-18/R27) | nixos module |

## Reboot-recovery + wave-2/3 fleet net-new

| Piece | Disposition | Target |
|---|---|---|
| resource-loss verdict/evidence class | add | witness enum + evidence (R14) |
| `preempted` verdict | add | evidence (R29) |
| `interrupt` priority tier | add | lease (R15) |
| pool-reachability producer + hysteresis | add | producers (R16/R26) |
| recover() pool-return ROW re-presentation (task-1) | add (auto-vs-eligible → CD-19) | recover (R16) |
| task-0 advisory assessor (onReturnAttest, noEnqueue) | add | producers/attest (R16/R24/R28) |
| remote lease grace + epoch re-adoption | add | lease/recover (R30) |
| coordinator-switch = restart + local adopt + remote re-adopt | port+add | recover (R17/R30) |
| worker servingSlice cgroup stamp | add | exec/nixos (R27) |
| fleet conformance suite | add (non-oracle gate) | BS-14 |

**Coverage proof:** every surviving frozen element has an explicit disposition tracing
to a numbered ruling; every cut is a ruling, not an omission; every wave-2 fix (devil
#1-#15), every wave-3 correction (devil-2 #1-#11), and every triage-resolved decision
(CD-04..CD-25, minus the promoted CD-17) terminates in a row.
