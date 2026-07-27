# Versus CI and durable-execution systems

Tally overlaps two mature product categories without trying to become either
one. Nix CI systems turn repository events into evaluations and builds. Durable
execution systems turn application code into resumable workflows. Tally takes a
narrower position underneath those frontends: admit already-described work to a
NixOS fleet, execute it under scarce-resource leases, and retain a verifiable
account of the result.

The facts below were checked against primary project sources on **2026-07-27**.
That date matters: Garnix's hosted service had just shut down, and Typhon had
already been archived.

## The Nix CI axis

| System | Real overlap | What it provides that tally does not | Decisive boundary and current state |
|---|---|---|---|
| [Hercules CI](https://hercules-ci.com/) | Nix-native configuration, self-hosted agents, binary-cache integration, and effectful work after builds | A hosted project/forge frontend, GitHub statuses and diagnostics, Nix evaluation/build orchestration, cache plumbing, and a supported effects library | This is the closest system. Its [effects](https://docs.hercules-ci.com/hercules-ci/effects/) deliberately cross the purity membrane with network, secrets, state, and locking in a container-isolated environment. Tally generalises the lower execution problem—arbitrary argv, resource leases, replay attachment, and witnessed evidence—but does not replace Hercules CI as a CI product. |
| [Hydra](https://github.com/NixOS/hydra) | A NixOS-hosted coordinator, queued work, remote builders, and durable build records | Projects, jobsets, periodic evaluations, build products, a web UI, accounts, notifications, and a JSON API | Hydra is an active Nix continuous-integration and release/build-farm system. Its unit of work is the evaluated Nix job and derivation. Tally neither evaluates jobsets nor presents a build farm; its unit is an admitted impure job whose evidence must be judged. |
| [buildbot-nix](https://github.com/nix-community/buildbot-nix) | Self-hosted NixOS master/workers, parallel Nix evaluation, shared stores, remote builders, and experimental impure effects | Buildbot's mature CI frontend, GitHub/Gitea webhooks and statuses, build matrices over flake checks, authentication, UI, and worker administration | The project describes itself as under active development but generally stable and widely used. It intentionally keeps ordinary work inside the Nix sandbox and uses an experimental Hercules effect path for impurity. Tally begins at that impure boundary and has no Buildbot-compatible pipeline model. |
| [Garnix](https://garnix.io/docs/) | Historically, Nix-focused GitHub CI, caching, hosting, and actions that could run outside the build sandbox | A turnkey hosted service and repository-facing user experience | There is no hosted alternative to choose today: Garnix [shut its service and deleted hosted data on 2026-07-15](https://garnix.io/blog/shutting-down/), while publishing its code. It remains relevant design history and possible self-hosted code, not a running SaaS comparison. |
| [Typhon](https://doc.typhon-ci.org/concepts.html) | Declarative flake projects and jobsets plus networked actions for webhooks, status updates, and deployment | A forge-agnostic CI model with projects, evaluations, jobs, actions, encrypted secrets, API, and web application | Typhon's own documentation calls it an early proof of concept with missing core features, and its [repository was archived on 2025-12-31](https://github.com/typhon-ci/typhon). Its action boundary resembles impure execution, but it is not a maintained substrate to build on. |

The comparison is not “tally, but less Nix.” Hercules CI, Hydra, and
buildbot-nix do substantial work tally intentionally lacks: repository discovery,
evaluation, status reporting, dashboards, build-product presentation, and cache
operation. If the desired product is Nix CI, one of those systems is a more
complete starting point.

Tally earns a separate category only when the scarce thing is not simply a build
slot and the required result is not simply a store path: an exclusive GPU, a
repository mutation, a remote hardware action, a renewable subscription
window, or an agent response that needs a durable evidence trail. A CI service can
be a tally producer. It should not have to become tally's lease engine or witness
authority.

## The durable-execution axis

| System | Real overlap | What it provides that tally does not | Decisive boundary |
|---|---|---|---|
| [Temporal](https://docs.temporal.io/) | Deterministic workflow replay, activities as effect boundaries, durable timers, retries, attachment, and crash recovery | A general distributed workflow platform, mature SDKs, signals/queries, long-lived timers, visibility tooling, a scalable service, and hosted cloud | Temporal guarantees applications resume after process and infrastructure failures. Tally-flow borrows replay discipline, but persists witnessed job results in a local coordinator and schedules NixOS resources. It is not a Temporal service implemented in miniature. |
| [Restate](https://docs.restate.dev/guides/request-lifecycle) | A persisted invocation journal, re-execution from the beginning, and collapse of completed context operations onto recorded results | Durable services and objects, HTTP/Kafka ingress, state, timers, events, service-to-service calls, SDKs, UI, and clustered deployment | Restate places a server in front of application handlers and journals SDK context operations. Tally places a lease/evidence daemon beneath producers and runs exact argv. Restate owns application invocation semantics; tally owns fleet admission and proof. |
| [Inngest](https://www.inngest.com/docs/learn/how-functions-are-executed) | Step IDs, memoized step results, retries, re-execution of handler code, and event-driven starts | Event ingestion, function registration, SDK middleware, scheduling, rate/concurrency controls, observability, and managed or self-hosted application-workflow infrastructure | Inngest's executor re-runs ordinary function code and returns stored step results. Tally-flow similarly re-derives node submissions, but rejects replay divergence and delegates every child to the same daemon queue as standalone jobs. It has no event platform or application SDK ecosystem. |

Temporal, Restate, and Inngest are stronger choices for business processes that
need months-long sleeps, external signals, rich application state, many language
SDKs, or horizontally scaled orchestration. Tally-flow is intentionally smaller:
one embedded Boa dialect, a Unix-socket coordinator, calendar and other bounded
producer kinds, and no multi-tenant control plane. Its advantage is that a flow
node is immediately a native tally job—same pools, priorities, executors,
guardrails, evidence gate, witness chain, and failure vocabulary—rather than an
activity that must be bridged into a separate NixOS scheduler.

## The structural conclusion

Tally is not a CI system. It sits below one.

Every CI product combines a workflow frontend—repositories, events, evaluations,
statuses, UI—with an execution substrate it partly owns. Tally extracts the
substrate for one sovereign NixOS operator and makes proof a first-class output.
CI-shaped behavior is one producer class feeding it, alongside timers, reachability
probes, manual submissions, flows, and other sources of intent. The daemon never
needs to understand pull requests in order to decide that two jobs contend for
the same GPU or that an asserted artifact is absent.

The generation property sharpens the distinction. Among the systems compared
here, none makes the operator's active NixOS generation also select the exact
store-pinned orchestration script, with a content hash in every run's replay
identity. CI systems version pipeline configuration in a repository or service;
tally's declarative flows are part of the machine closure and roll back with it.

## What tally deliberately did not borrow

- **GitHub Actions YAML or its emulation surface.** Reproducing marketplace
  actions, expression syntax, runner contexts, and compatibility quirks would put
  tally on an endless emulation treadmill. A GitHub event may produce a literal
  tally submission; the daemon does not interpret Actions workflows.
- **The `services.github-runners` inversion.** In that model GitHub schedules and
  the local machine obeys. Tally keeps admission authority on the operator's
  coordinator. GitHub can originate a producer event, never grant a local lease.
- **A spec-kit-style workflow `gate` step.** Tally does have evidence gates, but
  they judge declared facts after execution. It did not adopt a generic control
  node that pauses a workflow for a checklist or human approval.
- **Safe-output policing.** Tally can validate a schema, hash bytes, and record
  provenance. It does not decide whether prose is safe, true, polite, or fit to
  publish. That domain judgment belongs to the producer or a subsequent job.
- **Multi-tenancy.** There is no tenant isolation, per-user policy plane, billing
  layer, or public submission API. The security model is a sovereign operator's
  local Unix socket, systemd credentials, declared SSH workers, and NixOS policy.

Those omissions keep the category boundary enforceable: **Nix memoizes the pure;
tally proves the impure.** A frontend may decide what should happen next. Tally's
job is to show what was admitted, what ran, what resources it held, and what the
evidence actually supports.
