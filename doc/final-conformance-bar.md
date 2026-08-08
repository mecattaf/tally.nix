# Final conformance bar: desired-state behavior

Status: normative Phase 1 specification for the test-only final conformance bar.

Issue snapshot: #402, #403, #408, #409, #410, #415, #419, #426, #439,
#440, #441, #442, #443, #444, #445, #446, #447, and #448. The mission's
snapshot is authoritative for this document. GitHub now reports #441 closed and
the branch base contains its repair, but its contract remains part of the bar.
#410 is the one meta issue; the other 17 issues map to product or verification
behavior below.

## 1. Authority and interpretation

This document states the behavior of a conforming tally.nix, not the behavior
of the current implementation. Current source is useful only for locating
public entry points, durable formats, and harness wiring. A current output is
never an expected value merely because it was observed.

The words **must**, **must not**, and **exactly** are normative. An assertion is
at a public boundary when it observes a CLI argv or exit code, a manifest or
registry file, adapter stdin/stdout, a durable witness or attestation, a query
response, a rendered service command, a store root, or an end-to-end outcome.
Tests may use injection to make a race deterministic, but pass/fail must be
decided at one of those boundaries rather than by inspecting a private helper.

#410 supplies the verification doctrine rather than a product behavior:

- expected results come from this specification, issue contracts, repository
  doctrine, and recorded real-tool behavior;
- an eval is run against a held branch before merge and is repeated after a
  repair that touches the mechanism under evaluation;
- every new guard and every cross-boundary wire has a mutation that proves the
  assertion can fail; deletion mutations are required for call sites, argv
  fragments, roots, and dispatch edges;
- a red assertion is reported as a product non-conformance. A broken fixture,
  unavailable runner, or uncaught harness exception is an error, not a product
  failure and never a pass.

The eventual runner must accept an arbitrary tally.nix working-tree path and
must also be reachable through the flake. It must not modify production source
in the target tree. Tests known to describe unresolved issues are expected to
fail against the frozen current branch; the runner itself must still finish
and report those failures coherently.

## 2. Recorded empirical inputs

### 2.1 Codex cumulative-usage probe (#403)

The date-gated probe is settled: **REHYDRATES**.

Source record:

- `/home/tom/mecattaf/tally-codex-runs/final-bar/probe-403-verdict.md`
- fresh stream:
  `/home/tom/mecattaf/tally-codex-runs/probe-403/fresh-20260808T092733.jsonl`
- resumed stream:
  `/home/tom/mecattaf/tally-codex-runs/probe-403/resumed-20260808T092733.jsonl`

The fresh thread's final `turn.completed.usage` was:

```json
{"input_tokens":16050,"cached_input_tokens":11008,"cache_write_input_tokens":0,"output_tokens":5,"reasoning_output_tokens":0}
```

The same thread's first resumed reading was:

```json
{"input_tokens":32117,"cached_input_tokens":22016,"cache_write_input_tokens":0,"output_tokens":11,"reasoning_output_tokens":0}
```

The resumed reading includes the earlier attempt. It is a session-cumulative
checkpoint, not a per-attempt charge. Any rollup or pool meter that adds the
two raw readings is non-conforming.

### 2.2 Recorded external CLI grammar (#443, #445)

The following grammar probes were captured on 2026-08-08 from this worktree:

- `codex --version` reported `codex-cli 0.145.0`.
- `codex exec resume --help` described `--model` as an optional option and
  the positional grammar as `[SESSION_ID] [PROMPT]`. A default-model resume
  therefore does not need a fabricated model.
- `pi --version` reported `0.82.1`.
- `pi --mode json --version` exited 0 and printed only `0.82.1`.
- `pi --mode json -- --version` also exited 0 and printed only `0.82.1`.
  Thus inserting `--` does not protect this workload token from Pi's option
  parser.
- `pi --mode json -- -h` exited 0 with Pi's help, and
  `pi --mode json -- --definitely-not-a-pi-option` exited 1 naming both
  option-looking arguments. Pi has no recorded argv form in which an
  arbitrary leading-dash workload is opaque.
- `pi --mode json -p` exited 0 after emitting a session record without doing
  a supplied workload. A valid short Pi flag can therefore be stolen just as
  silently as `--version`.

These captures refine one sentence in #445 without changing its ruling: this
installed Pi sometimes accepts `--`, but it still interprets following
option-looking workload values. The safe contract remains pre-launch refusal.

The checked-in real-stream oracles remain authoritative too:

- `test/fixtures/usage/codex.jsonl` has a thread ID, final message, and usage,
  but no `model` key anywhere;
- `test/fixtures/traces/pi.jsonl` is a recorded normal Pi JSON stream.

Synthetic provider fields must not be added to make a preset test pass.

## 3. Canonical cross-boundary contracts

### 3.1 Campaign manifest: Rust arm CLI and packaged Python driver

Issues: #439, #444, #446.

There is one versioned operation over the JSON between
`<!-- tally:campaign:v1 -->` and `<!-- tally:campaign:v1:end -->`:

```text
normalize(raw manifest, canonical task records)
  -> reject(error)
   | accept(normalized manifest, canonical graph JSON, sha256 digest)
```

The canonical task record is exactly `number`, `title`, and body (empty when
the forge supplies no body), in admitted task order. Managed checkboxes,
issue state, timestamps, and prose outside the marked manifest are not graph
identity.

The accepted result has these rules:

1. Every object and tagged variant is closed. An unknown member is rejected;
   it is never ignored before hashing.
2. Repository checkout identity is **filesystem-canonical at admission**.
   The path must be absolute and name the validated Git worktree; `..` and
   symlink spellings are resolved once at arm time. The normalized manifest
   and runtime repository configuration both carry that same canonical path.
   A later consumer must not independently resolve only its copy.
3. An explicit steward containing only `adapter` and `argv` normalizes omitted
   fields to `env: {}`, `finalMessagePattern: "^TALLY_FINAL_MESSAGE=(.*)$"`,
   and `runtimeMaxSec: 120`. Omission and those
   explicit values are byte-identical. Explicit `finalMessagePattern: null`
   is invalid; `runtimeMaxSec: null` retains its separately documented
   unlimited meaning.
4. A steward final-message regex must compile and contain exactly one capture
   group. Zero, two, or more groups are invalid.
5. An agent model is a non-empty scalar of at most 128 Unicode scalar values.
   The 127- and 128-character cases are accepted; 129 is rejected.
6. A `forbidPaths` pattern ending in `/` is invalid, as are the already
   documented absolute, parent-traversing, malformed-`**`, duplicate, or
   oversized forms.
7. `conflictDomains` has three distinct states. A non-empty array is a
   declared allowlist. An explicit empty array is a declared-empty allowlist
   and permits no changed path. Absence is permitted only for an
   implementation task when `maxParallel == 1`, and it remains absent through
   every producer, schema, brief, and driver call. It must never normalize to
   `[]`.
8. For an absent serial `conflictDomains`, a passing agent's tree-delta gate
   uses exactly the ownership node's certified `ownedPaths` as its allowlist
   and records `owned-paths-fallback` as the source. The task's own committed
   delivery therefore passes. If the agent failed, ownership did not run, and
   no declared domains exist, the gate fails closed as **no certified
   allowlist**, not as a proved out-of-allowlist breach. Parallel omission is
   rejected by project/arm before work starts.

Canonical graph JSON is compact UTF-8 JSON: object keys sorted by Unicode code
point, no insignificant whitespace, array order preserved, ordinary Unicode
left unescaped except where JSON requires escaping. The digest is lowercase
`sha256:` plus SHA-256 of those bytes. Both normalized JSON and digest are
contract outputs; digest equality alone cannot hide two wrong normalizations.

Arm is the admission authority. On rejection it must write no registration
and enqueue no pass. On acceptance it records the normalized manifest it
hashed. The packaged driver either consumes that admitted value or invokes the
same versioned normalizer when checking the live issue; it must not maintain a
second set of defaults. A live raw manifest with unchanged meaning normalizes
to the admitted bytes. A meaningful edit produces a different digest and the
existing receipt with both digests and the first divergent canonical path.

The shared corpus must drive the actual Rust arm parser and packaged Python
driver. It includes valid default/maximal manifests, a symlink checkout, a
`..` checkout spelling, a minimal steward, serial omitted/empty/non-empty
domains, and rejection mutations for every object/enum plus the path, regex,
nullability, and 127/128/129 model boundaries. Each fixture yields either the
same canonical bytes and digest from both sides or rejection by both sides.
The symlink/`..` and minimal-steward fixtures also run through arm and the
packaged reconcile action; both must advance without a digest-mismatch receipt.

### 3.2 Evaluated Nix module/preset and rendered argv

Issues: #442, #443, #445.

The canonical fixture input is:

```text
(evaluated module/preset JSON, command kind, job options, cwd,
 prior captures, workload argv)
```

Its output is exactly one rendered argv or one typed pre-launch refusal. The
same fixture is evaluated through the Nix producer and the Rust consumer; the
pair may not carry independent implied grammar.

#### Tally configuration locator (#442)

Every daemon-spawned Tally child inherits the exact configuration locator of
the `CampaignHost` that admitted it. The global prefix is:

```text
<tally> --config <exact-path> <subcommand> ...
```

It applies to both the initial campaign `flow run` and every continuation.
For the NixOS surface the path is `/etc/tally/config.json`; Home Manager and
explicit hosts use their own exact configured path. An XDG fallback, an
environment variable no child reads, or a config path applied only to the
continuation is not equivalent.

A producer-through-consumer case places the only config at
`/etc/tally/config.json`, gives it a non-default pool and `maxFrameBytes`, and
executes the actual packaged initial and continuation children. Both must
resolve that file and observe the same sentinels before any flow node is
admitted.

#### Codex default-model recovery (#443)

The real Codex fixture must scrape a session reference, final message, and all
declared usage while honestly leaving model absent. A missing provider-emitted
model is not an error for a default-model session.

The evaluated stock preset declares its usage counter scope as
`session-cumulative`, matching the #403 probe. Counter scope is part of the
adapter contract emitted by Nix and consumed by Rust; it is not inferred from
the adapter name during rollup.

For a default-model recovery the rendered resume has no `--model` token or
model value:

```text
codex -C <cwd> exec resume --json <sessionRef> -- <workload...>
```

When admission explicitly supplied an authorized model, the same command
contains exactly one `--model <exact-model>` pair before the session
reference. The explicit value remains pinned across restart, pool return, and
public continuation. Tally must not manufacture a model capture from config,
the current default, or fixture-only JSON.

The daemon round trip is fresh attempt -> persisted real capture -> forced
restart/recovery or continuation -> rendered resume. The default-model path
must reach process launch without a model flag; restoring an unconditional
model placeholder must make it fail.

#### Pi option-looking workloads (#445)

The Pi preset declares that its first workload argument may not begin with
`-`. Rust enforces that declaration for launch and resume before enqueue and
before process creation. Refusal is a stable adapter-contract error with code
`option-like-workload-head` and identifies the adapter, index 0, and offending
argument. It must not be reported later as a missing final message or a
projection timeout.

At minimum, `--version` and `-p` are refused and a process-spawn sentinel stays
untouched. The normal recorded Pi fixture and a normal non-option workload
still render and scrape successfully. This policy is data-driven and Pi
specific; a custom adapter with a genuinely safe transport may define a
different policy. Merely inserting `--` into Pi's argv is non-conforming under
the recorded grammar above.

### 3.3 Git-ai attribute producer and validator (#441)

The correlation attribute contract is one closed set of at most seven keys:

```text
required: taskUuid, attempt, leaseEpoch, adapter
optional: flowRunId, nodeOrdinal, taskRef
```

Values are non-empty bounded scalars with no control characters. The execution
request producer includes `taskRef` whenever orchestration has one, and the
validator admits it. Neither side may widen or narrow the set independently.

With `gitAi.enable = true`, a campaign node carrying a task ref reaches its
payload and the git-ai attributes retain that exact ref end-to-end. Removing
the producer insertion or the validator allowance is a failure.

Any executor-side validation rejection is the node's immediate terminal
cause. It has structured code `executor-validation-failed`, message
`execution request is invalid: <validation message>`, and structured details
containing the validation message. The same structure is hash-covered in the
witness, appears in `tally query run`, and reaches the flow error. The flow
must not wait `projectionWaitMs`, relabel it `result-projection-timeout`, or
report `result-schema-mismatch`.

### 3.4 Campaign registry writer and N-1 reader (#447)

The durable authority record and host-local tuning are separate contracts.

- The closed v2 authority JSON in `campaigns/armed/*.json` contains campaign
  identity, graph authority, observations, and pinned asset paths. A current
  writer must not add a member while still labeling the object v2.
- `projectionWaitMs` is host tuning, not campaign authority. It lives in a
  separately versioned sidecar below `campaigns/host-tuning/`, a directory the
  N-1 reader does not scan. Absence means the current 10-second flow-host
  default.
- Authority and tuning updates are atomic as a lifecycle operation. A missing
  or old sidecar cannot corrupt, change the digest of, or make unreadable the
  authority record.

The rollback policy is explicit. Current default bytes and current bytes for
an arm with an explicit projection wait are both readable by the actual N-1
authority decoder, because neither puts the tuning member in v2. On rollback,
N-1 ignores the sidecar and uses its own default wait; campaign identity,
digest, actors, asset paths, and last observation remain unchanged. Rolling
forward again recovers the sidecar's explicit value. This loss of a tuning
override during rollback is accepted and documented; loss of authority or an
unpollable registry is not.

The reverse direction is also required: the current reader accepts literal
N-1 v2 bytes and supplies default tuning without rewriting authority. Every
future authority schema change must ship forward migration, a stated rollback
policy, and an N/N-1 corpus update. Adding a serialized member to a closed
version without updating that corpus fails the bar.

### 3.5 Campaign asset producer and Nix garbage collection (#448)

A successful registration owns the exact flow and driver it pins for its
entire lifetime.

For an asset inside `/nix/store`, the registration creates a durable indirect
GC root for the containing store object before the registration becomes
visible. Flow and driver are independent ownership obligations even when a
normal package happens to put them in one output. For a non-store override,
arm copies the file into registration-owned content-addressed immutable
storage, preserves the executable mode needed by that asset, and registers the
snapshot path. Later changes or deletion of the source path do not change the
armed machinery.

Registration plus both ownership records is one transaction:

- failed arm leaves neither a visible registration nor orphan ownership;
- re-arm establishes all new ownership before switching the registration,
  then removes superseded ownership;
- disarm and automatic closed-issue pruning remove the registration, roots,
  and unreferenced snapshots;
- startup/poll reconciles interrupted transactions: referenced ownership is
  repaired while the store object still exists, and orphan roots/snapshots are
  removed. Reconciliation never silently repins the current package.

This lifecycle belongs to the campaign registry used by interactive arm,
NixOS, and Home Manager. A module-only root is non-conforming.

The boundary case arms with independently identifiable flow and driver store
objects, removes every other reference, performs or simulates collection,
upgrades the polling generation, and then polls. The exact generation-N flow
and driver must execute. The same case proves cleanup after disarm and closed
prune. Omitting ownership of either asset must make the survival assertion
fail.

## 4. Rollup and usage evidence model

Issues: #402, #403, #408, #409.

The rollup answers two different questions and must not let either define the
other:

1. **What attempts should exist?** The independent durable execution census.
2. **What usage can be charged to each attempt?** Verified adapter-scrape
   evidence.

The attestation ledger is evidence for question 2; it is never the denominator
for question 1.

### 4.1 Durable attempt census (#402)

For each durable run member, the row's current attempt counter defines the
contiguous expected attempts `1..=currentAttempt`. Verified terminal witness or
lifecycle facts may supply the counter for a row-less member, but the usage
attestation ledger may not. A member with no independent counter produces an
`attempt-census-unavailable` caveat rather than an assumed zero.

The query wire publishes at least:

- `coverage.tasks`;
- `coverage.attemptsExpected`;
- `coverage.attemptsObserved`;
- `coverage.attemptsMissingAttestation`;
- `coverage.tasksWithoutAttestation`; and
- `coverage.ledgerVerified`.

An attestation is deduplicated by `(taskUuid, attempt, leaseEpoch)`; a re-scrape
of that identity supersedes its older record. An attestation outside the
independent census is not summed and raises `unexpected-attestation`.

For a task whose durable counter is 3 and whose ledger contains only attempt
3, the required result is `attemptsExpected: 3`, `attemptsObserved: 1`,
`attemptsMissingAttestation: 2`, a non-empty
`attempts-missing-attestation` caveat, and `isComplete == false`, even though
`tasksWithoutAttestation == 0`. A failed ledger verification sums nothing and
is incomplete.

### 4.2 Per-attempt semantic envelope (#403, #408)

Every completed execution attempt writes one `adapter-scrape` attestation,
including an attempt whose adapter declares no usage or whose stream reports
none. That record preserves the normalized raw `usage` observation and carries
a sibling `usageEvidence` object with these semantic members:

| Member | Meaning |
|---|---|
| `schemaVersion` | Version of this evidence contract. |
| `declaredFields` | Sorted unique logical usage fields declared by the exact adapter contract for this attempt. |
| `counterScope` | `attempt` or `session-cumulative`. |
| `derivation` | `attempt`, `fresh-zero`, `delta`, `baseline-missing`, `counter-regressed`, or `lineage-fork`. |
| `lineage` | Adapter plus provider session/thread reference; required for cumulative counters. |
| `predecessor` | For `delta`, the prior attestation's task, attempt, lease epoch, sequence, and hash. |
| `contribution` | The normalized per-attempt observation that may be metered and rolled up; absent for an unsafe derivation. |

The attestation hash covers all of these members. `declaredFields` uses the
logical field vocabulary already published by `usage.rs`, including
`inputTokens`, `inputTokensWithCacheRead`, `cacheReadTokens`,
`cacheWriteTokens`, `outputTokens`, `reasoningTokens`, `totalTokens`, and
`costUsd`. It records the contract at dispatch time, not whatever a later
configuration says the adapter would declare.

The existing three observation states remain distinct. `not-declared` means
the adapter declared no usage capture, `not-reported` means it declared one but
the stream carried none, and `reported` includes a measured zero. None is
interchangeable with a missing attestation or an unsafe cumulative baseline.

An `attempt` counter contributes the normalized observation directly. A fresh
launch of a `session-cumulative` adapter may use zero as its baseline only when
the invocation created a new provider lineage; it records `fresh-zero`. A
resume must name a verified predecessor in the same lineage and contributes a
component-wise monotone delta. The predecessor may belong to another run: it
is a baseline, not a charge to the queried run.

A missing predecessor, a decreasing counter, incompatible field topology, or
two intervals that fork/overlap one lineage is never repaired by treating the
current cumulative checkpoint as a fresh attempt. Its contribution is absent,
the corresponding typed caveat (`cumulative-baseline-missing`,
`cumulative-counter-regressed`, or `cumulative-lineage-fork`) appears, and the
rollup is incomplete. The pool meter uses `contribution` under exactly the
same rule and emits no guessed charge for an unsafe derivation.

For the recorded #403 pair, the contributions are:

| Field | Fresh contribution | Resumed contribution | Two-attempt sum |
|---|---:|---:|---:|
| provider `input_tokens` | 16050 | 16067 | 32117 |
| `cached_input_tokens` | 11008 | 11008 | 22016 |
| `cache_write_input_tokens` | 0 | 0 | 0 |
| `output_tokens` | 5 | 6 | 11 |
| `reasoning_output_tokens` | 0 | 0 | 0 |

After Codex's inclusive-input normalization, the run therefore has 10,101
fresh input tokens, 22,016 cache-read tokens, 0 cache-write tokens, 11 output
tokens, and a derived total of 32,128. Adding the raw fresh and resumed
checkpoints would instead double-charge and must fail the fixture.

The fresh/resumed pair is checked in under `test/fixtures/usage/` with its
command/version provenance and is included in the flake's packaged source set,
so the conformance path exercises the same bytes under `nix flake check`.

### 4.3 Declared surface controls completeness (#408)

Completeness is evaluated per declared logical field, not against an assumed
four-component universal adapter.

The query wire publishes `coverage.declaredByField` and
`coverage.reportedByField`. A component's existing `attempts` count is compared
with the number of attempts that declared that component. An undeclared field
is outside the denominator; a declared field absent or unreadable in the raw
observation is partial evidence and names that field.

Consequences:

- an adapter declaring only `inputTokens` and `outputTokens` is complete when
  it reports both; absent cache fields cause neither `partial-components` nor
  `partial-fresh-input`;
- fresh input is complete over the fresh-input fields the adapter actually
  declared. If it declares both input and cache-write, both are required; if
  it declares input only, that input is the complete fresh-input contribution;
- a cost-only adapter that reports its declared `costUsd` is not diagnosed as
  four drifted token keys;
- a legitimate total-only adapter that declares and reports only
  `totalTokens` is complete, including in a run beside component adapters;
- an adapter declaring components plus `totalTokens` whose component keys all
  drift while the total survives is incomplete and names the missing declared
  components, even when every attempt has that same shape.

Component sums may cover different legitimate subsets of a heterogeneous run.
Their declared/reported counts state that scope. A subset is not a caveat when
it exactly matches the declared contracts; a missing member of a declared
subset is.

The CLI diagnosis follows the same evidence. It identifies declared-but-missing
fields from `declaredByField`/`reportedByField`; it must not tell an operator
that all token keys drifted merely because a cost-only adapter declared none.

### 4.4 Legacy evidence and the #409 arithmetic

An older attestation with no declared-field or counter-scope evidence is not
silently upgraded from its reported shape. It may be interpreted only when a
durable immutable adapter-contract snapshot proves the missing semantics;
otherwise its contribution is excluded and `declared-surface-unknown` makes
the rollup incomplete. Raw observation remains available for audit.

For compatibility diagnostics, the old reported-shape
`total-only-attempts` inference remains defined over legacy records: whenever
one or more ambiguous total-only records sit beside a legacy record kept in
the component denominator, the caveat fires. It is a positive-count predicate,
not an exactly-one predicate. One component-shaped record plus two ambiguous
total-only records must therefore carry the caveat. Documentation and the
source comment must call this a legacy reported-shape inference and point to
`declared-surface-unknown`; they must not claim that drift cannot hide behind a
stated total or that the neighbour necessarily reported components.

For new evidence, exact `declaredFields` replaces this inference: legitimate
total-only attempts are known to be legitimate, and wholly drifted component
attempts are known to be drifted.

## 5. Reader-state aggregate decision (#415)

**Decision: a filtered view's aggregates describe the rows in that view. An
explicit identity lookup is not a filtered browse.**

The rule has two parts:

1. `query run <id>` and `query jobs --flow-run <id>` are explicit identity
   requests. They return archived data and mark it archived; the human run
   view retains its `-- ARCHIVED` banner. `flowRunTasks` remains the true
   durable membership count, and ordinary pagination may still make
   `items.len()` smaller. A contradictory explicit `--no-archived` with a
   flow-run identity is refused rather than producing a silently withheld
   response.
2. Browse views (`query jobs` without a run identity and default
   `query standup`) hide archived rows. Every aggregate displayed beside
   those rows is computed after the same filter. `standup.completed`,
   `gateFails`, `cancelled`, `inFlight`, `runs`, `reused`, and
   `canonicalGpuSeconds` all describe the visible set. `archivedHidden` and
   `archivedRunsHidden` separately count what was removed. `--archived`
   includes the rows and recomputes the aggregates over that larger visible
   set.

Thus an archived-only standup probe that previously showed
`completed=0, archivedHidden=1, canonicalGpuSeconds=42, reused=1` must show
`completed=0, archivedHidden=1, canonicalGpuSeconds=0, reused=0`. The hidden
count communicates history; a cost or reuse number with no visible row does
not.

Rationale: view-local numbers are composable and auditable from the payload in
front of the consumer. Historical cost is still available through the
explicit run lookup or `--archived`; silently mixing it into a filtered digest
creates a third, unnamed view.

## 6. Eval-manifest zero-covered decision (#426)

**Decision: introduce exit code 4 for checked and fully accounted declared
surface with zero declared items in status `covered`.**

The decided table is:

| Exit | Meaning |
|---:|---|
| 0 | Schema valid, both expected categories non-empty, every declared key accounted for, and at least one declared item is `covered`. Other declared items may be `reused` or `failed`; this is still not a claim that all passed. |
| 1 | Refused: missing/ambiguous/unparsable/schema-invalid manifest or an expected key with no item. |
| 2 | Usage error: no findings paths supplied. |
| 3 | Schema valid but at least one expected category is absent/empty, so coverage was not checked. |
| 4 | Schema valid, both expected categories non-empty, all declared keys accounted for, and zero declared items are `covered` (all are `reused` and/or `failed`). |

Classification uses only entries named by `expected`, not undeclared extra
items. `all-declared-failed.md` must move from 0 to 4; an all-reused fixture is
also 4. A mix containing at least one declared `covered` item remains 0.

For multiple files, refusal (1) wins over every manifest result, unchecked (3)
wins over checked results, and otherwise any zero-covered manifest makes the
invocation 4. Exit 2 applies only to the no-argument invocation. Numeric order
does not define precedence.

Successful lines keep `coverage=checked`/`unchecked` and add a stable
`verification=present`/`none` token. Exit 4 is a branchable valid outcome, not
a schema failure. The checker header, operating documentation, and fixture
tests must state the same table.

Rationale: the first close-out consumer should not have to parse English or
reverse-engineer three counts to distinguish some direct verification from
none. Code 4 preserves exit 0's narrow accounted-for meaning while making the
degenerate case explicit before consumers ossify around it.

## 7. Verification and recovery seams

### 7.1 Parallel tally-core test population (#419)

Resolution is a property of the whole default-parallel test binary, not a
green rerun of whichever test failed last.

Each known mechanism receives a deterministic regression test with explicit
synchronization or fault injection. In particular, retention tests own a
private/deterministic lock-probe condition, witness assertions wait on a real
post-ack persistence barrier, process-publication readers cannot observe an
empty intermediate file, and wall-clock-only `Elapsed(())` assertions use a
counted causal event or an explicit quiescence barrier. Sleeps, blind retries,
and global test serialization are not fixes.

The four latest named deadline cases are included in that deterministic sweep:

- `confirmed_pool_loss_witnesses_and_return_re_presents_the_same_row`;
- `fleet_conformance_coordinator_switch_bumps_epoch_and_re_adopts_remote_work`;
- `preset_gate_defaults_distinguish_absent_manifest_from_gates_passed`; and
- `public_continuation_uses_the_scraped_session_without_manual_captures`.

Closure then uses the repository's measured population gate exactly as
documented: prebuild the exact `tally_core` test binary from the candidate tree,
verify the probe is not pointed at a stale binary, then run
`test/flake-probe.sh <binary> 480 3` under the real concurrent-suite load. A
conforming wave runs three concurrent suites for 480 seconds and records zero
failing runs among however many complete in that interval. A spinner or
synthetic stressor is not equivalent load. The suite stays at default
parallelism. Because `flake-probe.sh` is a measuring tool and exits 0 even when
it observes failures, the conformance runner parses its total and turns any
nonzero measured failure count into a failed #419 assertion.

Deleting any causal barrier must fail its focused deterministic test. The wave
then checks for mechanisms not yet named; it is not a substitute for the
focused mutations.

### 7.2 Session launch cwd at every producer seam (#440)

Whenever a path derives and installs a `sessionRef`, it atomically derives and
installs the cwd from which that exact session was launched. This holds for all
five paths:

1. ordinary completion-time scrape;
2. restarted-daemon capture re-derivation;
3. startup re-presentation hydration;
4. adopted-metadata hydration; and
5. recovered-job installation.

For an adapter declaring `resumeRequiresLaunchCwd`, a same-cwd continuation
through each path succeeds and a different-cwd continuation is refused with
the existing measured-cwd mismatch. `UnrecordedLaunchCwd` is reserved for a
genuinely legacy or corrupt pointer that arrived without a launch record; a
current recovery path may not manufacture that state.

Each path has its own black-box restart/recovery case. Deleting that path's
session-cwd binding makes only its case fail while the other four remain
green. A source scan that merely counts calls does not satisfy this contract.

## 8. Full-pipeline consequence

The bar includes one hermetic path through actual packaged artifacts:

```text
project/arm -> poll -> reconcile -> dispatch -> execute -> sweep -> digest
```

It uses the local-forge test hook, a real Git repository, the packaged
spec-build flow and driver, and deterministic local adapters—no live GitHub or
provider account. It must cross the issue body/registration boundary rather
than starting below arm, and it must reach a terminal digest after at least
one implementation task changes and commits a file. Observable assertions
include the admitted canonical digest, initial and continuation config
locators, task-ref execution with git-ai enabled, ownership/tree-delta result,
sweep result, terminal witness, and final campaign digest.

This scenario is not allowed to stop at reconcile. It exists to make a broken
downstream edge fail in the same run, while focused corpora retain precise
diagnosis of the responsible contract.

## 9. Issue-to-assertion and mutation ledger

| Issue | Minimum public assertion | Mutation that must be caught |
|---|---|---|
| #402 | A three-attempt row with only attempt 3 attested reports two missing attempts and is incomplete. | Restore ledger-derived attempt denominator. |
| #403 | The recorded fresh/resumed Codex pair rolls up by the exact deltas above; a missing predecessor is caveated and not charged as fresh. | Sum raw attempt snapshots or let the meter consume the resumed cumulative reading. |
| #408 | Two-field, cost-only, total-only, and components-plus-total-drift adapters are graded from their declared fields. | Remove declared fields or compare every attempt with a universal component set. |
| #409 | One component-shaped legacy record plus two total-only legacy records emits `total-only-attempts`; public wording states the limitation. | Change the positive-count predicate to `== 1` or restore the retired no-hidden-drift claim. |
| #415 | Explicit archived run lookup returns rows; archived-only standup has zero visible aggregates and nonzero hidden count. | Filter the explicit lookup or leave pre-filter GPU/reuse totals. |
| #419 | Focused causal race cases pass, followed by a 480-second/three-concurrent-suite zero-failure wave. | Delete a synchronization edge or serialize the whole test binary. |
| #426 | All-declared-failed and all-reused fixtures exit 4; a mixed fixture with one covered declaration exits 0. | Collapse 4 into 0 or classify from undeclared extra entries. |
| #439 | A serial task omitting domains commits an owned file and passes via `owned-paths-fallback`; explicit `[]` breaches. | Normalize absence to `[]` at either producer/schema seam. |
| #440 | Same-cwd resume succeeds after each of the five session-ref producer paths. | Delete any one cwd binding. |
| #441 | A task-ref campaign node executes with the exact seven-key closed contract; validation rejection is immediate and structured on flow/witness/query surfaces. | Remove `taskRef` from producer or validator, or route rejection through projection waiting. |
| #442 | Initial and continuation packaged children read the only config at `/etc/tally/config.json` and its non-default sentinels. | Remove either serialized `--config <path>` pair. |
| #443 | Real no-model Codex capture resumes without `--model`; explicit model remains pinned. | Require `%<model>%` or inject a synthetic model into captured data. |
| #444 | Symlink/`..` checkout and minimal steward produce byte-identical normalized JSON and digest at arm and driver. | Reintroduce one-sided `.resolve()` or null/default divergence. |
| #445 | Pi `--version` and `-p` workloads are refused before spawn; normal fixture still runs. | Delete the Pi workload-head policy or replace it only with `--`. |
| #446 | Unknown-key, trailing-slash, regex arity/syntax, null, and 127/128/129 fixtures have arm/driver rejection parity before side effects. | Remove any mirrored check or closed-object rule. |
| #447 | Current default and explicit-tuning arms decode in actual N-1; current decodes N-1 without authority drift. | Add a v2 member or put tuning back in the authority JSON. |
| #448 | Both independently rooted assets survive upgrade/collection and exact versions execute; disarm/prune clean ownership. | Omit either root/snapshot or any lifecycle cleanup edge. |

#410 has no separate row because it is the doctrine governing every mutation
in the table. A test tied to one of the 17 rows without its named failure mode
is incomplete even if it is green.

## 10. Recorded decisions, in one place

- #415: reader-state browse filtering defines a view; all aggregates follow
  the visible rows, while explicit run identity requests do not hide.
- #426: checked/accounted but zero-covered is the valid, branchable exit code
  4, with `verification=none`.
- #439: serial omission remains absence and activates the certified
  `ownedPaths` fallback; explicit empty remains deny-all.
- #444: checkout identity is filesystem-canonical at arm time; minimal steward
  defaults are the driver's published pattern and 120-second budget.
- #447: host tuning is an N-1-invisible sidecar; rollback preserves authority
  while temporarily falling back to N-1's wait default.
- #448: store assets are independently GC-rooted and non-store overrides are
  registration-owned immutable snapshots.
- #402/#403/#408/#409: the execution census supplies the denominator; each
  attestation carries the declared field surface and counter scope; resumed
  cumulative providers contribute verified lineage deltas; ambiguity or a
  missing baseline is caveated and never guessed.
