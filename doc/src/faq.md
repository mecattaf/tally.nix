# FAQ

## Is tally a workflow engine now?

tally schedules jobs. A flow is a deterministic JavaScript program that
materialises ordinary jobs through the same admission door; it is not a second
scheduler. The runner is itself an ordinary job, and the flow run otherwise
exists as provenance on its child rows and witnesses.

The doctrine is: **tally never originates intent**. Intent can arrive from a
declared calendar, GitHub event, event file, manual command, or an already
admitted job. Job-originated work is legitimate, but it is still constrained
by ancestry guardrails, pools, explicit evidence, and the witness ledger.

## Why not Temporal or Restate?

Use Temporal or Restate when the problem is a durable application or business
process: years-long instances, service handlers, durable timers, signals,
human approvals, externally addressable workflow state, or application-level
message delivery. Temporal models resilient workflow executions backed by an
event history and activities; Restate provides a server in front of services
with journals, durable promises, signals, and timers. Those are features, not
overhead to imitate badly.

tally's boundary is smaller and more host-shaped:

- one local coordinator owns logical contention;
- systemd transient units execute already chosen commands;
- Nix declares mechanisms and immutable flow inputs;
- SSH reaches daemonless workers;
- evidence and witnesses answer what actually ran; and
- flows are short replayable materialisers of jobs, with no durable external
  message handlers or mid-run human waits.

Choose tally when the scarce thing is a local or fleet resource—GPU time,
build slots, a mutation mutex, or a programmatic budget—and the durable output
must be auditable alongside Nix configuration. Choose a durable-execution
platform when the workflow itself is the long-lived product state. They can
also coexist: a service workflow may originate a tally job, or a tally job may
call a service, provided that boundary and its evidence are explicit.

See the official descriptions of
[Temporal workflows](https://docs.temporal.io/workflows) and
[Restate workflows](https://docs.restate.dev/tour/workflows) for their
respective execution models.

## Why not Hercules CI?

Hercules CI is Nix-native CI/CD. It evaluates repository configuration in
response to source events, builds outputs, and can run networked deployment
effects with its agent, state, locks, and secrets. Use it when that is the
product you need.

tally does not own a hosted CI control plane. Its `build-effect` producer
observes declared build effects and never invokes `nix build`; a `drv()` flow
node builds only because the flow explicitly requested that node. tally is
useful when heterogeneous shell and agent work must share logical resource
limits and produce one local evidence chain, whether or not a CI system
originated the intent.

The [Hercules CI evaluation](https://docs.hercules-ci.com/hercules-ci-agent/evaluation/)
and [effects](https://docs.hercules-ci.com/hercules-ci/effects/) documentation
describe the CI/CD boundary directly.

## Why a Unix socket instead of HTTP?

The coordinator is a host-local authority. A Unix socket gives it:

- filesystem ownership and permissions instead of a second authentication
  system;
- no listening TCP service, TLS lifecycle, or reverse proxy;
- a low-overhead multiplexed channel for CLI queries and blocked awaits; and
- one unambiguous local admission door shared by the runner and transient
  units.

Remote execution is deliberately narrower: the coordinator invokes one fixed
helper over pinned SSH. It does not expose the coordinator RPC over the
network. For remote administration, SSH to the coordinator and run the CLI
against its local socket.

There is no connect-time frame negotiation. Client and daemon must use the
same rendered `maxFrameBytes` configuration.

## Why is the producer set closed?

Producer kinds are admission semantics, not generic plugins. Each kind has a
typed Nix schema, explicit event normalisation, credentials boundary,
deduplication rules, generated unit shape, and tests. The shipped set is
exactly:

- `calendar`
- `build-effect`
- `pool-reachability`
- `gh`
- `events-dir`

An arbitrary executable pretending to be a sixth kind would bypass those
contracts. Extend at safer seams instead: use the open adapter map to describe
execution, use a flow to compose admitted work, write a bounded event into an
`events-dir`, or invoke manual enqueue. A genuinely new intake semantic
belongs in tally's typed producer registry and conformance suite.

## Why is there no mid-run human gate?

A durable human wait changes the product into an externally interactive
workflow service: it needs an addressable event, authorization, liveness and
expiry policy, replay semantics, and an operator interface. tally does not
have that surface.

Human validation is the culmination artifact—a pull request, report, reviewed
manifest, or morning queue—not an invisible suspended node. End one run with
that artifact and its gate facts. If approval should cause more work, let the
approved external state originate a new admitted run. Queue pause/resume is an
operator control over pools; it is not a durable approval primitive.

## Can a flow hold a mutex or budget lease for its whole run?

A flow may declare one typed `workloadMutex`. The generated parent runner then
co-leases that capacity-1 mutex with `flow` for its process lifetime. This is
not a run-lifetime guarantee: runner death releases the mutex, another run may
take it, and replay of the interrupted run queues behind that holder while its
already-created children remain durable. A direct manual `tally flow run`
bypasses runner admission, so a mutex flow must instead be enqueued as a parent
job holding both pools.

There is no corresponding run-wide budget lease. The former `budgetPool`
option was removed because it never created one. Individual nodes request
their own pools while they run.

Likewise, flow nodes deliberately do not supply `consumptionEstimate`, and
configured flow checking excludes `windowed-consumption` pools. Flow
contention uses priorities so more important work can intercede without an
estimate preventing the wave from completing. Manual and producer enqueue
retain the kernel's windowed-consumption mechanism. Do not infer either
capability from the option names.

## Are pools hard resource isolation?

No. The shipped pool enforcement mode is `cooperative`. Capacity, budgets,
priority, and yield are coordinator admission policy. There is no dmem,
dmemcg-booster, serving-slice, or other kernel-backed enforcement surface.
Use NixOS/systemd/container controls separately when hostile or merely
misbehaving work needs isolation.

## Does tally move files between hosts?

No. SSH transports an execution request and bounded captures, not workload
artifacts. Use explicit Git, Attic, or workload-specific transfer nodes, and
witness the receiving-side result. See
[Fleet deployment](operating/fleet-deployment.md#move-artifacts-explicitly).

## Can the NixOS module schedule flows and producers?

Not in the shipped implementation. The NixOS module renders the system daemon,
witness emitter, drain timer, and retention timer, but rejects producer,
usage-meter, and flow declarations. Use Home Manager—standalone or integrated
into NixOS—for those workload-scheduling surfaces.

Forge-native campaigns are the one exception, and they are not a producer:
`services.tally.campaignForge.enable` renders the campaign pools, the driver
adapter, one events-directory registry entry that carries no unit, and the
`tally-campaign-poll` service and timer, so a host with no user session can
execute campaigns armed with `tally campaign arm`. Declared
`services.tally.campaigns` are still rejected there, because those are driven by
a managed GitHub producer unit. See
[Campaigns on a NixOS host](flows/campaigns.md#campaigns-on-a-nixos-host).

## Is a passing witness a permanent artifact archive?

No. It proves the canonical record and the evidence observed at transition
time. Nix store evidence is rooted only within the configured retention
horizon; ordinary artifact files are owned by the workload and have no tally
garbage collector. After pruning, the witness chain can still verify even
though the referenced bytes are unavailable. See
[Retention and growth](operating/retention.md).
