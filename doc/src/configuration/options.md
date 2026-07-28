# Options reference ⚙️

The book builds three references directly from the Nix module system. Generated pages are
created inside the `packages.doc` build and are deliberately absent from the source tree, so a
checked-in snapshot cannot drift away from the module:

| Reference | What it represents |
|---|---|
| [Shared core](core-options.md) | The common typed `services.tally.*` schema |
| [Home Manager](home-manager-options.md) | The schema evaluated with Home Manager's actual defaults |
| [NixOS](nixos-options.md) | The schema evaluated with the NixOS module's actual defaults |

Types, defaults, examples, descriptions, and declaration links on those pages come from each
`mkOption`; prose elsewhere in the book should link to the generated option anchor instead of
copying a default.

## The wrappers are not equivalent

Both wrappers expose the same option names, but Home Manager is the only complete deployment
surface. It renders the daemon, event drain, retention timer,
[all five producer kinds](home-manager-options.md#servicestallyproducers),
[usage-meter processes](home-manager-options.md#servicestallypoolsnameusagemeter), and scheduled
flow runners as systemd user units. The NixOS wrapper renders only the system daemon and witness
emitter. A NixOS configuration can therefore type check producer and flow declarations without
creating the units that drive them.

This asymmetry is literal shipped behavior, not a recommendation inferred from the design. Use
the [Home Manager reference](home-manager-options.md) for a deployed topology and the
[NixOS reference](nixos-options.md) only for the narrower system-daemon surface.

## Declarative flows

Each flow currently has twelve generated leaf options. In Home Manager, a non-empty
[`services.tally.flows`](home-manager-options.md#servicestallyflows) registry also declares two
weak-default pools:

- `flow`: `resource = "cpu-slot"`, capacity 8, cooperative enforcement, and no hard preemption;
- `build`: `resource = "build-slot"`, capacity 2, cooperative enforcement, and no hard
  preemption.

Flow scripts may not list either reserved name in `meta.pools`. The generated
[`catalog`](home-manager-options.md#servicestallyflowsnamecatalog) entry covers
selector configuration validation. Runtime wiring is documented once in the CLI reference under
[declarative runner pools](../operating/cli.md#declarative-runner-pool-and-workloadmutex)
and [catalog is flag-only](../operating/cli.md#catalog-is-flag-only).

[`workloadMutex`](home-manager-options.md#servicestallyflowsnameworkloadmutex)
adds one capacity-1 mutex to the generated parent runner pool set. It is held
for the process lifetime; manual invocation must enter through an admitted
parent carrying both `flow` and the mutex.

The generated flow entries are in the
[Home Manager options](home-manager-options.md) and the wrapper-independent shape is in the
[shared core options](core-options.md).

## Evaluation-time flow failures

With tally enabled, both wrappers make their generated configuration depend on a checked
derivation. That derivation invokes the configured tally package itself: it checks the
configuration, parses every flow, applies the static determinism lint, validates arguments
against `meta.argsSchema`, checks pool closure and reserved names, and validates a catalog when
one is configured. A failure therefore stops `nixos-rebuild` or `home-manager switch` before
activation.

The rejection fixtures pin these four diagnostics byte for byte:

**Non-literal `meta`:**

```text
tally: {"name":"FlowMetaError","code":"meta-nonliteral","message":"meta must contain only JSON-compatible literals","location":{"line":5,"column":25}}
```

**Banned global:**

```text
tally: {"name":"FlowDeterminismError","code":"determinism-violation","message":"banned global Math.random is unavailable in flow scripts","location":{"line":8,"column":1},"details":{"global":"Math.random"}}
```

**A pool used by the script but not declared in `meta.pools`:**

```text
tally: {"name":"FlowPoolError","code":"undeclared-pool","message":"pool \"worker-gpu\" is used by the script but absent from meta.pools","location":{"line":8,"column":31},"details":{"pool":"worker-gpu"}}
```

**Invalid `meta.argsSchema`:**

```text
tally: {"name":"FlowMetaError","code":"args-schema-invalid","message":"meta.argsSchema is not a valid JSON Schema: \"definitely-not-a-json-schema-type\" is not valid under any of the schemas listed in the 'anyOf' keyword","location":{"line":1,"column":21}}
```

The next closure layer uses the configured flow name and pool name in plain-text diagnostics.
For the shipped fixture, a `meta.pools` entry missing from `services.tally.pools` produces:

```text
tally flow fixture references unknown pool worker-gpu
```

Listing either implementation pool in `meta.pools` instead produces
`tally flow fixture script meta.pools must not include flow` or
`tally flow fixture script meta.pools must not include build`. These are build failures as
well; none is deferred to a scheduled run.
