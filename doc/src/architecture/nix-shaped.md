# Why tally is Nix-shaped

Tally is **a sovereign durable-execution substrate for NixOS**. It borrows
Nix's way of making work explicit, but applies it where purity ends: networked
agents, repository mutation, deployment, hardware-bound programs, and other
jobs whose result cannot honestly be called a reproducible build. The boundary
is short enough to remember:

> **Nix memoizes the pure; tally proves the impure.**

That is an architectural claim, not a metaphor pasted over a queue. The same
shapes recur in the submission record, executor, lease engine, evidence gate,
and flow runner. The resemblance also has limits, and those limits matter.

## A job is an impure derivation

A Nix derivation declares a builder, an argument vector, an environment and
named outputs. Nix realises it in a controlled build environment and accepts it
only if the declared outputs exist. A tally submission makes the corresponding
facts explicit, then runs a program that may be impure and records evidence of
what happened.

| Nix mechanism | Tally mechanism | Why the pairing matters |
|---|---|---|
| derivation `builder` plus `args` | an adapter resolves a non-empty `argv`; the executor calls the program directly | Quoting is data, not a second program hidden in a shell string. The legacy `invocation` input is tokenized at admission; the resolved job still carries argv. |
| builder environment assembled by Nix | adapter environment, tally execution fields, and credential references are assembled explicitly; reserved ambient variables are removed when absent | A retry does not silently inherit whichever agent session happened to launch it. |
| store paths and derivation inputs | store-pinned flow scripts and catalogs, content-addressed briefs, manifest/script hashes, and the canonical payload hash | The durable identity names the resolved work, not just a friendly label. |
| declared output paths | evidence terms such as an artifact path, hash, valid store path, exit code, and gate manifest | Completion is a proposition to check, not a process status to trust. |
| sandbox or remote build machine | a transient systemd unit locally, or the fixed SSH remote helper on a declared executor | Execution is bounded and attributable even though the program itself may use the network or mutate a workspace. |
| successful realisation | a terminal witness verdict and its checked facts | The durable result says what was observed, including failure and uncertainty. |

The argv claim is literal in the shipped executor: both the production systemd
path and the standalone/test direct fallback construct a process from an argv
array. The daemon disables that fallback and requires durable systemd ownership.
The built-in adapter called `shell` adds no shell by itself. If a caller wants
shell language, it must say so in argv, for example `["/bin/sh", "-c", "..."]`.
The [resolved payload fields](https://github.com/mecattaf/tally.nix/blob/4c85563a3899369f1aa4905f44e9806e424593f1/crates/tally-core/src/wire.rs#L375-L427)
and [direct process launch](https://github.com/mecattaf/tally.nix/blob/4c85563a3899369f1aa4905f44e9806e424593f1/crates/tally-core/src/executor.rs#L2879-L2920)
are the executable contract.

Tally's `clean-exit-no-artifact` verdict is the sharpest independently
rediscovered isomorphism. A process can exit zero while failing to produce a
declared artifact or valid store path. The
[evidence gate](https://github.com/mecattaf/tally.nix/blob/4c85563a3899369f1aa4905f44e9806e424593f1/crates/tally-core/src/evidence.rs#L378-L517)
records that as a distinct failure, just as Nix rejects a builder that returns
success without producing its declared output, such as `$out`. Exit zero proves
only that the program chose zero.

The analogy must not be stretched into a reproducibility claim. Tally does not
hash the internet, discover undeclared files read by an arbitrary program, or
pretend two agent answers are byte-identical builds. Its payload hash commits to
the submission facts it knows. Its witness commits to the observations it made.
That is proof of execution and evidence, not proof that an impure world could be
recreated.

## Memoization and coalescing

Nix has long avoided doing the same work twice. A valid store result can be
substituted instead of rebuilt, and concurrent clients asking the daemon for the
same derivation share one realisation rather than launching duplicate builders.
Tally has the corresponding two paths, with an extra integrity check required by
impure artifacts:

This is at least a fifteen-year-old precedent, not a new scheduler trick. The
[Nix 0.10 user guide](https://releases.nixos.org/nix/nix-0.10/manual/index.html)
already documented that concurrent requests for the same derivation take a build
lock: one process builds while the others wait or realise different derivations.

1. A full submission combines `dedupKey` with a hash of its canonical resolved
   payload. If exactly one live job has both values, a new caller receives
   `attached` and waits on that job. Same key with different payload is a loud
   conflict.
2. If the governing terminal witness is a pass with the same payload hash,
   tally re-checks the declared evidence. Artifact bytes are hashed again and
   declared store paths are checked for validity before the disposition is
   `reused`.
3. A governing terminal failure is returned as `terminal`; it is not cosmetically
   turned into a pass. Drift, a missing artifact, or an invalid store path rejects
   reuse and admits fresh work with the rejection reason recorded.

So `dedupKey → reused` mirrors binary-cache substitution, while rehashing is the
integrity step an impure artifact needs. `dedupKey + payloadHash → attached`
mirrors nix-daemon build coalescing. Tally's
[live attach check](https://github.com/mecattaf/tally.nix/blob/4c85563a3899369f1aa4905f44e9806e424593f1/crates/tally-core/src/daemon.rs#L4596-L4672)
and [terminal evidence probe](https://github.com/mecattaf/tally.nix/blob/4c85563a3899369f1aa4905f44e9806e424593f1/crates/tally-core/src/evidence.rs#L698-L708)
implement the split.

This matters to tally-flow's history. Durable-execution systems led to the design
requirement that replayed calls collapse onto prior or in-flight work. Only then
did the implementation campaign notice that the local daemon already contained
the same central kernel shape for standalone jobs. The flow runner did not need a
second memo store or a private scheduler; it needed to derive stable submissions
and use the daemon correctly.

## Pools are build machines grown up

Nix remote builders combine placement and capacity hints. A machine advertises a
system type, supported features, a maximum job count, and a speed factor; the
daemon chooses a suitable place to realise a derivation. The current
[remote-build documentation](https://nix.dev/manual/nix/2.34/advanced-topics/distributed-builds)
also makes the coordinator/SSH shape explicit.

Tally separates two questions that Nix's build-machine record puts close
together:

- **May this work run now?** Named pools answer through atomic, coordinator-side
  leases. Capacity and resource predicates model co-residency or rolling
  consumption. Priority aging, cooperative yield, and optional hard reclamation
  determine who gets scarce capacity.
- **Where should it run?** An executor answers placement. With the local executor,
  the coordinator starts a transient systemd unit. With an SSH executor, it sends
  a fixed request to a remote helper. The remote machine does not own the lease
  queue, and it does not receive the coordinator's tally socket.

The closest mappings are therefore:

| `nix.buildMachines` concern | Tally counterpart |
|---|---|
| `system` and `supportedFeatures`/`requiredSystemFeatures` | explicit executor plus named pools selected by the submission |
| `maxJobs` | pool `capacity`, potentially composed across several pools atomically |
| speed and suitability ordering | explicit priority class, FIFO/flow fairness within rank, and one-rank aging |
| remote SSH build | SSH executor with host-key pinning and a fixed remote helper |
| daemon-held build lock | coordinator-held renewable lease, including across remote execution |

This is a family resemblance, not a hidden build-machine algorithm. Tally does
not probe advertised features or dynamically score executors by speed. A
submission names its pools and, when remote placement is wanted, its executor;
priority orders work rather than ranking hosts.

Pools can model a resource ordinary CI queues usually leave implicit: a renewable
five-hour subscription window. For example, an operator can declare a budget
pool whose rolling `windowSec` is `18000` and whose `consumptionCap` is expressed
in the resource's native unit. A job must supply `consumptionEstimate`; admission
records that estimate as the window debit. It ages out with the window; completion
does not replace it with measured usage. This is a capability of the shipped
`windowed-consumption` predicate, not a predeclared five-hour pool. The module
defaults to a seven-day window.

There are two current flow limits worth stating without designing their answers.
Declaratively generated flow runners are always submitted to exactly `["flow"]`.
The [producer renderer](https://github.com/mecattaf/tally.nix/blob/4c85563a3899369f1aa4905f44e9806e424593f1/nix/modules/common.nix#L2169-L2212)
and [separate assertion](https://github.com/mecattaf/tally.nix/blob/4c85563a3899369f1aa4905f44e9806e424593f1/nix/modules/common.nix#L2302-L2312)
make the boundary visible: `budgetPool` is checked only for existence; despite
the option's stale description, it adds no runner pool and creates no lease.
There is consequently no sanctioned mechanism today for a flow to hold a
workload mutex or budget lease for its whole run
([open question #107](https://github.com/mecattaf/tally.nix/issues/107)).
Separately, a generic job can supply `consumptionEstimate`, but a flow node cannot;
that means a node cannot currently enter a `windowed-consumption` pool
([open question #116](https://github.com/mecattaf/tally.nix/issues/116)). Neither
gap is papered over here with a prospective API.

## Pure evaluation in JavaScript clothes

Nix evaluation and tally-flow share a reason for determinism: the same program
and the same known inputs must rediscover the same work. Flow scripts therefore
have no imports, filesystem or network API. Their `meta` export must be a pure
JSON-compatible literal. Static linting and runtime hardening reject `Date`,
`Math.random`, `WeakRef`, `FinalizationRegistry`, `eval`, and `Function`. Boa runs
with loop and recursion limits, and the only effectful doors are the small host
API: `job`, `drv`, adapter sugar, catalog selection, and witnessed logging.

But the dialect is deliberately not Nix.

- A workflow is an **effect sequence**. Replay identity includes the flow run,
  script hash, and monotonically derived node ordinal. Eager JavaScript control
  flow makes “the Nth `job()` call” an observable event. Nix laziness could skip,
  duplicate the discovery context of, or reorder such calls as values are forced.
- JavaScript promises give the runner a concurrency vocabulary. The custom Boa
  job executor submits work to the daemon, which remains the scheduler, and
  observes results in witnessed order. A lazy evaluator has no equivalent
  in-language concurrency contract for effect calls.
- Waiting is explicit and priced. Nix's
  [import-from-derivation](https://nix.dev/manual/nix/2.26/language/import-from-derivation)
  pauses sequential evaluation until a store object is realised, which serialises
  discovery and is commonly disabled. Tally-flow makes that boundary intentional:
  a runner waits in the low-cost `flow` CPU-slot pool while child jobs compete for
  their actual resource pools.

“Tally-flow is IFD on purpose” is useful shorthand, but not an identity. The flow
runner is itself an ordinary, capacity-consuming tally job, and its children are
durable daemon submissions rather than Nix derivations discovered by evaluation.
The default flow pool has capacity eight, not infinite or literally free.

## Generations govern automation

For a declaratively configured scheduled flow—one with `onCalendar` set—the Nix
module turns the script and optional catalog into store paths in the producer's
literal argv. The runner records hashes of the exact script bytes, serialized
arguments, and exact optional catalog bytes on every node. A later invocation of
the same run with different identity fails as `script-changed-mid-run`,
`args-changed-mid-run`, or `catalog-changed-mid-run`. Together these mechanisms
give orchestration a generation:

- the active NixOS or Home Manager generation selects the flow script;
- rollback selects the previous store-pinned script along with the rest of the
  system configuration;
- the witness binds each run to the script, arguments, and catalog content that
  derived its submissions;
- behavior can be bisected by switching known generations rather than reconstructing
  a mutable CI configuration from a service database.

The store-path guarantee is narrower than “every invocation is immutable.”
`tally flow run ./local-script.js` remains a valid manual command and accepts an
ordinary path.
Store pinning and system rollback govern scheduled producers rendered from
`services.tally.flows`; the CLI does not silently copy arbitrary scripts into
the store. Hash pinning still prevents one existing run from changing its
script, arguments, or catalog midway.

## Isolation by composition

The executor boundary forms a practical three-tier ladder:

1. A local transient systemd unit provides lifecycle ownership and the selected
   hardening profile.
2. An SSH executor places the same resolved job on a standing NixOS worker while
   the coordinator retains admission and lease authority.
3. When a hardware boundary is required, run that SSH worker inside a declarative
   [microvm.nix guest](https://microvm-nix.github.io/microvm.nix/declarative.html).
   Give the standing guest an SSH endpoint, register it as an ordinary tally SSH
   executor, and give that executor its own pool.

Tier 3 is composition, permanently—not a hidden `microvm` executor kind, dialect
surface, or adapter tier. microvm.nix owns guest creation and boot; tally sees a
normal remote host. This pays guest boot and standing-memory cost even while idle,
rather than creating an ephemeral VM per job. In return, the existing remote
contract stays intact: strict host-key checking, coordinator-side leases,
fail-closed transport, the standard cross-host evidence reply, and no forwarded
tally socket from which the guest could enqueue children.

That final separation is the recurring design rule. Nix owns pure construction
and system generations. Callers and jobs originate intent; tally itself never
does. Job-originated work returns through the same bounded, witnessed admission
as every other submission. Tally owns controlled execution and the durable proof
of what the impure program actually did.
