# Intro

tally is a contention arbiter and a proof recorder for work that Nix cannot make
pure.

Give it an explicit job—argv, resource pools, priority, evidence, and optional
provenance—and one coordinator will:

1. validate and durably admit the description;
2. atomically lease every named logical resource it needs;
3. run the argv in a deterministic systemd transient unit, locally or through a
   daemonless SSH executor; and
4. judge the declared evidence and append the outcome to a hash-chained witness
   ledger before reporting completion.

That is the product boundary:

> tally tracks **contention** and **proof**—never **content** or **control**.

tally does not inspect what an agent wrote, decide which issue should be worked
next, or move artifacts between machines. A caller, a declared producer, or a
flow script supplies that intent. tally decides whether the resulting jobs may
run now and records what actually happened.

## Flows do not change the arbiter

The older description “tally is not a workflow scheduler” became false when
flows shipped. The narrower rule that replaced it is:

> **tally never originates intent.** Job-originated work enters through the same
> bounded, pooled, witnessed admission path as every other job.

A flow is a deterministic JavaScript program that materializes ordinary tally
jobs. The runner itself is an ordinary job, and its nodes contend in the same
lease engine as calendar work, direct enqueues, and interrupt-priority work.
There is no second scheduler hidden in the flow runtime.

## Who it is for

tally is useful when several impure workloads share something that can be
overcommitted or must be accounted for: GPU lanes, build slots, a mutex, CPU
capacity, a metered subscription window, or a remote worker. It is especially
suited to NixOS fleets where the mechanism, policy, scripts, and executable can
be pinned in one generation while execution remains necessarily impure.

Use a plain systemd service or timer when one command runs on one host, nothing
else contends with it, and systemd's exit status and journal are sufficient
evidence. tally earns its extra machinery only when admission, cross-workload
fairness, remote re-adoption, semantic gates, deduplication, or independently
verifiable history matter.

There are two ways in. [Install tally](getting-started/install.md) deploys the
complete Home Manager surface — pools, producers, meters, declarative flows —
and the pages after it take the shortest path to a witnessed
[job](getting-started/first-job.md) and a witnessed
[flow](getting-started/first-flow.md). [Install tally as a
product](getting-started/product.md) is the single-machine path: a
profile-installed CLI and a fleet-free daemon, for running campaigns on your
own repositories without deploying anything to anyone. The
[Concepts](concepts/jobs-and-admission.md) section then explains each mechanism
in isolation.
