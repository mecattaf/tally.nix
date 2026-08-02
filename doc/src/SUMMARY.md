# Summary

[Intro](introduction.md)

# Getting started

- [Install tally](getting-started/install.md)
- [Run your first job](getting-started/first-job.md)
- [Run your first flow](getting-started/first-flow.md)

# Concepts

- [Jobs and admission](concepts/jobs-and-admission.md)
- [Pools and leases](concepts/pools-and-leases.md)
- [Priorities and preemption](concepts/priorities-and-preemption.md)
- [Evidence and gates](concepts/evidence-and-gates.md)
- [The witness ledger](concepts/witness-ledger.md)
- [Producers](concepts/producers.md)
- [Adapters](concepts/adapters.md)
- [Executors](concepts/executors.md)

# Flows

- [Authoring a flow](flows/authoring.md)
- [The dialect](flows/dialect.md)
- [Host API reference](flows/host-api.md)
- [Submission identity and replay](flows/submission-and-replay.md)
- [Campaigns](flows/campaigns.md)
- [git-ai squash fidelity](flows/git-ai-squash-fidelity.md)
- [Pooled-review cookbook](flows/pooled-review.md)
- [Two more cookbook recipes](flows/cookbook.md)
- [Cross-host handoff](flows/cross-host-handoff.md)

# Declarative configuration

- [Pools, executors, producers, and adapters](configuration/mechanisms.md)
- [`services.tally.flows`](configuration/flows.md)
- [Hardening presets](configuration/hardening.md)
- [Options reference ⚙️](configuration/options.md)
  - [Shared core options ⚙️](configuration/core-options.md)
  - [Home Manager options ⚙️](configuration/home-manager-options.md)
  - [NixOS options ⚙️](configuration/nixos-options.md)

# Operating tally

- [CLI reference](operating/cli.md)
- [Query and observability](operating/observability.md)
- [Recovery and restarts](operating/recovery.md)
- [Retention and growth](operating/retention.md)
- [Fleet deployment](operating/fleet-deployment.md)
- [Troubleshooting](operating/troubleshooting.md)

# Reference

- [RPC protocol contract](reference/rpc-protocol.md)
- [Witness format and verification](reference/witness-format.md)
- [Exit codes and error taxonomy](reference/errors.md)

# Architecture and rationale

- [Why tally is Nix-shaped](architecture/nix-shaped.md)
- [Versus CI and durable-execution systems](architecture/comparisons.md)
- [Design lineage](architecture/lineage.md)

---

[FAQ](faq.md)

[Conventions](conventions.md)
