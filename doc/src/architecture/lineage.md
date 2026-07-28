# Design lineage

Tally's design did not arrive as one borrowed architecture. Nine focused
[transfer briefs](https://github.com/mecattaf/tally.nix/tree/4c85563a3899369f1aa4905f44e9806e424593f1/legacy-docs/transfer)
were used as an adversarial library: identify one mechanism worth lifting, name
the surrounding assumptions, and write down what must stay behind. The resulting
code is often narrower than its donor.

This page records lineage, not authority. The briefs are point-in-time research
notes. Current behavior is defined by the shipped code and the user/reference
chapters.

## The embedded evaluator

### Boa: host-owned determinism

[Boa](https://github.com/boa-dev/boa) supplied the enabling mechanism for
tally-flow: a pure-Rust ECMAScript engine whose promise job queue and host hooks
can be owned by the embedder. Tally pins `boa_engine` and `boa_parser` 0.21.1,
installs its own `JobExecutor`, registers a small host API, surfaces unhandled
promise rejection, and hardens the context against replay-breaking globals.

The engine's general runtime was not imported. There is no Boa module loader,
`fetch`, timer API, console package, or “first future to finish wins” event loop.
Those conveniences would reintroduce ambient effects or wall-clock completion
order. The host decides when daemon results become observable.

[rquickjs](https://github.com/DelSkayn/rquickjs) was researched as the fallback.
Its async integration and intrinsic controls proved the alternative was viable,
but it brought a bundled C engine and weaker serde ergonomics. It is documented
negative lineage, not a dependency or a dormant backend slot.

### workflows.js: authoring shape, not runtime policy

The workflows.js brief donated the small JavaScript authoring shape: literal
`meta`, ordinary control flow, `job()` as the primitive, adapter sugar, and the
`parallel`/`pipeline` vocabulary. It also demonstrated that an orchestration
script can be a disposable, reviewable artifact while workers do the external
I/O.

Tally did not copy its LLM-only `agent()` primitive, null-collapsing errors,
private concurrency queue, mutable session journal, model knobs, or five-hour
process-lifetime assumption. Child calls are admitted eagerly by the tally daemon;
they name adapters and pools; terminal failures remain failures. Resource windows
belong to coordinator pools, never an in-process budget object.

## Durable execution without the platform

Several systems independently support the same central observation: workflow code
can be re-executed if completed effects collapse onto durable results.

- [Temporal](https://docs.temporal.io/) contributed ordered replay matching and
  a distinct nondeterminism failure. Tally's version is stricter at the job
  boundary: an ordinal is checked against the canonical payload hash, and any
  mismatch fails the run instead of attaching to similar-looking work. Temporal's
  history service, task queues, SDK negotiation, patch markers, and multi-tenant
  deployment were not copied.
- [Azure Durable Functions' determinism constraints](https://learn.microsoft.com/azure/azure-functions/durable/durable-functions-code-constraints)
  reinforced the clock/random/ambient-I/O bans. Its whole-orchestration version
  branches were not adopted; declarative tally flows instead use a store-pinned
  script and optional catalog plus run-level script, argument, and catalog
  hashes. An in-progress run rejects changes to any of those inputs.
- [Inngest](https://www.inngest.com/docs/learn/how-functions-are-executed) supplied
  concrete step-result memoization and the useful separation between a duplicate
  call-site identity and a racing invocation. Tally did not copy automatic ID
  suffixes or warn-and-continue “graceful determinism.” A collision or payload
  divergence is an error.
- [Cloudflare Workflows](https://developers.cloudflare.com/workflows/) reinforced
  the rule that durable state should be made from step returns rather than ambient
  mutable variables. Tally did not import its retry vocabulary or instance-fatal
  error semantics wholesale.
- [Obelisk](https://github.com/obeli-sk/obelisk) demonstrated a deterministic job
  executor that refuses nondeterministic work kinds. Tally borrowed that narrow
  stance, not Obelisk's WASM Component Model, WIT boundary, or execution-log
  architecture.

The result is intentionally not a miniature durable-execution service. The local
witness and task database were already present; flow replay became a disciplined
client of that kernel. In-flight attach and terminal reuse therefore work the same
for a flow node and a standalone job.

## Operator prior art

The pi-appliance brief captured the operator's own production patterns before
they could be idealised away: small named resource pools, roster-driven model
selection, map/validate/reduce, explicit quorum, one repair attempt, and dissent
that survives aggregation. Its blocking supervisor also supplied the “cheap
parent, expensive children” shape used by tally-flow.

The transfer was structural, not a hidden compatibility mode. Catalog selection
is witnessed before inference. Pool and ensemble policy stays in the caller.
Reducers consume compact validated results rather than shared worker state. Tally
does not parse the old shell supervisor, and a selector count does not override
the capacity of the pools its selected members require.

## NixOS module and isolation style

[microvm.nix](https://github.com/microvm-nix/microvm.nix) donated option-tree and
assertion idioms: `attrsOf` submodules for named instances, cross-field checks,
and a closed dispatch surface. [srvos](https://github.com/nix-community/srvos)
donated the hardening-bundle-plus-overrides shape and the habit of documenting
every exception. Tally did not copy microvm.nix's flat hypervisor enum for
heterogeneous adapters, its manual state-directory scripts, or an imaginary
srvos collection of hardening presets that does not exist upstream.

microvm.nix also participates in runtime composition, but not through a copied
executor. Hardware-boundary isolation is a standing declarative guest exposed as
an SSH worker, as described in [Why tally is Nix-shaped](nix-shaped.md#isolation-by-composition).
There is permanently no tally-specific microVM executor kind.

## Artifacts, retention, and independently checked facts

The Attic/Trustix brief was originally labelled future design research. Later
rulings implemented only the parts that fit the single-operator system:

- [Attic](https://github.com/zhaofengli/attic) supplied the explicit push/substitute
  data-plane pattern for cross-host store evidence. Tally does not embed Attic,
  reproduce its object store, chunker, authentication service, or database GC. An
  operator deploys Attic and jobs invoke its client; the multi-host check exercises
  that composition.
- Nix GC roots supplied retention. Store evidence is kept by age with a live-witness
  floor rather than by a second tally object database.
- [Trustix](https://github.com/nix-community/trustix) supplied the questions about
  append-only commitments, query projections, and independently reported facts.
  Tally still has one canonical coordinator witness—not a Trustix quorum or sparse
  Merkle map. `tally witness compare` checks advisory remote execution attestations
  against that canon.

That narrower shipped ruling is recorded in the
[plain-schema witness chapter](https://github.com/mecattaf/tally.nix/blob/4c85563a3899369f1aa4905f44e9806e424593f1/doc/witness.md): one in-place
`witness.jsonl`, predecessor archives inert, and TaskChampion as a rebuildable view.
It supersedes the briefs' “later witness version” speculation; the source notes are
preserved because they explain the questions, not because their deferred designs
remain promised.

## Two checklist-level influences

[spec-kit](https://github.com/github/spec-kit) contributed a completeness checklist
for workflow shapes: run, branch, loop, fan-out, and fan-in. It did not become a
runtime or an intent format inside tally. In particular, its durable human `gate`
step was deliberately left out. A pull request or report may be the culmination
artifact, while tally's own evidence gates judge machine-checkable declarations
after a job runs.

The Linux kernel community contributed an attribution convention rather than an
execution mechanism. Its current
[AI-assistance guidance](https://docs.kernel.org/process/coding-assistants.html)
uses an `Assisted-by:` trailer while reserving legal authorship certification for
the human. Tally's GitHub mutation sink emits a greppable projection with more
specific provenance:

```text
Assisted-by: <adapter>:<model> (tally:<taskUuid> witness:<seq>)
```

The trailer is not the proof and does not make an AI a commit author. It points
back to the canonical witness record that names the admitted job, model-bearing
adapter evidence, and sequence. This is the lineage principle in miniature:
borrow the useful social shape, bind it to tally's actual mechanism, and leave
the donor's unrelated machinery behind.
