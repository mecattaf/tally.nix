# tally.nix

**Contention and proof for impure labor.**

tally admits explicitly described jobs through named logical resource pools,
runs them as local or daemonless-remote systemd units, and records their
outcomes in a hash-chained witness ledger. It is for builds, agents, GPU work,
and metered calls that must share scarce capacity without losing the evidence
of what ran.

The doctrine is:

> **tally never originates intent.** Every job—including work submitted by an
> already admitted job—passes the same admission door, is bounded by pools and
> ancestry guardrails, and is witnessed.

tally schedules jobs. A flow is a deterministic, content-hashed JavaScript
program that materialises ordinary jobs through that door; it is not a second
scheduler. tally does not inspect domain output to choose the next objective,
move workload artifacts between hosts, manage secrets, or provide a
general-purpose distributed workflow service.

There are no releases yet; deployments should pin a reviewed commit. Report suspected
vulnerabilities through the private routes in the [security policy](SECURITY.md).

Read the checked book at
**[mecattaf.github.io/tally.nix](https://mecattaf.github.io/tally.nix/)**.

## A 30-second job

Import `inputs.tally.homeManagerModules.tally`, then declare one pool:

```nix
services.tally = {
  enable = true;

  # The default timer invokes host-wide Nix store GC. Enable it after choosing
  # an operating policy.
  retention.enable = false;

  pools.local-build = {
    resource = "build-slot";
    capacity = 1;
  };
};
```

After the Home Manager switch, submit one directly executed command and inspect
its proof:

```console
$ result="$(tally enqueue --pool local-build --evidence exit:0 --wait -- \
    /run/current-system/sw/bin/true)"
$ printf '%s\n' "$result" | jq .
$ tally query proof --task "$(printf '%s\n' "$result" | jq -r .task_uuid)"
```

The same job path supplies pool admission, direct argv execution, restart
recovery, evidence evaluation, and the canonical verdict. The NixOS module
uses `/run/tally/tally.sock`; the Home Manager CLI discovers its user socket
by default.

## Read next

- [Install tally](https://mecattaf.github.io/tally.nix/getting-started/install.html)
  and [run the first job](https://mecattaf.github.io/tally.nix/getting-started/first-job.html)
- [Understand jobs and admission](https://mecattaf.github.io/tally.nix/concepts/jobs-and-admission.html),
  [pools and leases](https://mecattaf.github.io/tally.nix/concepts/pools-and-leases.html),
  and [evidence and gates](https://mecattaf.github.io/tally.nix/concepts/evidence-and-gates.html)
- [Author a flow](https://mecattaf.github.io/tally.nix/flows/authoring.html)
  and [understand replay](https://mecattaf.github.io/tally.nix/flows/submission-and-replay.html)
- [Browse the generated options](https://mecattaf.github.io/tally.nix/configuration/options.html)
  and [CLI reference](https://mecattaf.github.io/tally.nix/operating/cli.html)
- [Deploy a fleet](https://mecattaf.github.io/tally.nix/operating/fleet-deployment.html),
  [operate recovery](https://mecattaf.github.io/tally.nix/operating/recovery.html),
  and [troubleshoot](https://mecattaf.github.io/tally.nix/operating/troubleshooting.html)
- [Read the FAQ](https://mecattaf.github.io/tally.nix/faq.html)
  and [project conventions](https://mecattaf.github.io/tally.nix/conventions.html)

Book source and local build instructions live in [`doc/`](doc/README.md).
The older design and campaign records in [`legacy-docs/`](legacy-docs/README.md)
remain provenance, not the current user manual.

## Platform support

tally supports `x86_64-linux` only. That is the platform exercised by the full
fleet gate and matches tally's systemd-based execution model.

Project policy: [security](SECURITY.md), [releasing](RELEASING.md),
[changelog](CHANGELOG.md), and [dependency policy](deny.toml).

## Development

```console
$ env -u TALLY_TEST_REMOTE_HOST nix develop --command cargo test --workspace
$ nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings
$ nix develop --command test/cargo-deny.sh
$ nix flake check -L
```

The flake exposes the `tally` package, NixOS and Home Manager modules, the
checked mdBook as `packages.doc`, and its documentation check as `checks.doc`.
There are intentionally no GitHub workflow files. For each exact pull-request
head, the implementing worker runs `test/fleet-gate.sh` locally and pastes its
transcript tail into the pull request. That transcript is the merge evidence;
the runner does not publish evidence or control GitHub writes or merges.
