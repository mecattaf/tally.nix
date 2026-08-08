# Pools, executors, producers, and adapters

This chapter is the operator's map of the mechanisms configured below
`services.tally`. It explains how the fields work together; the generated
[shared-core options reference](core-options.md#servicestallypools) remains the
authority for types, defaults, examples, and declaration locations.

The four registries have different jobs:

| Registry | Responsibility | Shape |
|---|---|---|
| `pools.<name>` | Decide whether a job may hold a scarce logical resource. | Open names, closed resource and predicate vocabulary. |
| `executors.<name>` | Send an admitted job to one daemonless SSH worker. | Open names, SSH-only transport. |
| `producers.<name>` | Narrow an external observation into an enqueue. | Open names, exactly five producer kinds. |
| `adapters.<name>` | Render an admitted workload as direct argv and collect advisory captures. | Open names and operator-defined adapter shapes. |

Configuration is rendered to JSON and checked by the configured tally binary
before activation. Nix assertions reject invalid relationships even earlier.
Neither layer turns a declaration into hardware isolation: pools are logical
admission gates, executors rely on a prepared remote user manager, and adapter
argv is executed without a shell.

## Pools

A [`pools.<name>`](core-options.md#servicestallypools) entry names one admission
gate. A job requesting several pool names receives one atomic lease set: tally
grants every requested pool together or queues the job without holding a
partial set.

| Option | Operational meaning |
|---|---|
| [`resource`](core-options.md#servicestallypoolsnameresource) | Classifies the gate as `vram`, `build-slot`, `cpu-slot`, neutral counted `slot`, `budget`, or `mutex`; it does not discover or allocate that resource. |
| [`capacity`](core-options.md#servicestallypoolsnamecapacity) | Bounds simultaneous holders for `co-residency`; a `mutex` is the capacity-one special case. |
| [`budgetGb`](core-options.md#servicestallypoolsnamebudgetgb) | Records a VRAM co-residency budget. Current admission still counts holders and does not sum per-job VRAM, so this is not an enforced memory total. |
| [`predicate.co-residency`](core-options.md#servicestallypoolsnamepredicateco-residency) | Selects counted-holder admission. |
| [`predicate.windowed-consumption.windowSec`](core-options.md#servicestallypoolsnamepredicatewindowed-consumptionwindowsec) | Sets the rolling look-back interval for durable budget debits. |
| [`predicate.windowed-consumption.consumptionCap`](core-options.md#servicestallypoolsnamepredicatewindowed-consumptionconsumptioncap) | Sets the spend ceiling in the pool's declared native unit. |
| [`enforce`](core-options.md#servicestallypoolsnameenforce) | Selects the shipped enforcement implementation. Only cooperative enforcement exists; declaring a pool does not create a cgroup or patched-systemd boundary. |
| [`hardPreempt`](core-options.md#servicestallypoolsnamehardpreempt) | Opts the pool into reclaiming a lower-priority holder that does not yield within the configured grace. A co-allocated victim is reclaimed only when every pool the same request asks it to yield in also opts in. |
| [`autoResume`](core-options.md#servicestallypoolsnameautoresume) | Overrides resource-specific same-row recovery after a pool returns; leaving it unset uses tally's resource policy. |
| [`priority`](core-options.md#servicestallypoolsnamepriority) | Orders pool consideration; lower ranks are considered first. It is separate from job priority. |
| [`credentials`](core-options.md#servicestallypoolsnamecredentials) | Adds `LoadCredential` references to every job that leases this pool. Values are source paths, not secret contents. |
| [`usageMeter.argv`](core-options.md#servicestallypoolsnameusagemeterargv) | Starts a supervised direct-argv feeder for observed programmatic usage. |
| [`usageMeter.pollIntervalSec`](core-options.md#servicestallypoolsnameusagemeterpollintervalsec) | Controls the feeder observation and freshness cadence. |
| [`usageMeter.budgetClass`](core-options.md#servicestallypoolsnameusagemeterbudgetclass) | Identifies the supported externally metered budget class. |

The tagged `predicate` accepts exactly one branch. `co-residency` counts live
holders. `windowed-consumption` requires every enqueue to carry an authoritative
`consumptionEstimate`, records that debit durably, and rejects a grant that
would cross the cap. Without an external `usageMeter`, built-in adapter usage
uses token counts; an observed meter can reduce headroom but cannot mint it.
Only Home Manager renders the optional meter process.

### Pool cross-field rules

The module enforces four relationships. In the diagnostics below `${name}` is
replaced by the configured pool name; the rest of each message is exact.

1. A `mutex` must use `predicate.co-residency` and `capacity = 1`:

   ```text
   mutex pool ${name} must use co-residency with capacity 1
   ```

2. `budgetGb` is accepted only for a co-resident `vram` pool whose capacity is
   greater than one:

   ```text
   pool ${name} budgetGb is valid only for a co-resident vram pool with capacity > 1
   ```

3. `predicate.windowed-consumption` belongs only to `resource = "budget"`:

   ```text
   pool ${name} windowed-consumption predicate requires resource = budget
   ```

4. `usageMeter` has the same windowed-budget prerequisite:

   ```text
   pool ${name} usageMeter requires a windowed-consumption budget pool
   ```

Credential names and a meter's non-empty direct argv have additional local
validation. Consult the generated entries above for their exact types.

## Executors

An [`executors.<name>`](core-options.md#servicestallyexecutors) entry describes
one remote execution target. Admission and leases remain centralized; the
worker runs the fixed `tally __remote-executor` helper and does not run another
daemon or queue. Workload argv travels in a bounded JSON request on standard
input and never becomes part of the SSH command line.

| Option | Operational meaning |
|---|---|
| [`kind`](core-options.md#servicestallyexecutorsnamekind) | Selects the remote transport. The shipped registry accepts SSH only. |
| [`host`](core-options.md#servicestallyexecutorsnamehost) | Supplies an explicit OpenSSH destination host or IP literal; option-like and unsafe forms are rejected. |
| [`user`](core-options.md#servicestallyexecutorsnameuser) | Supplies the explicit remote login identity. |
| [`port`](core-options.md#servicestallyexecutorsnameport) | Selects the destination TCP port. |
| [`sshProgram`](core-options.md#servicestallyexecutorsnamesshprogram) | Pins the coordinator-side OpenSSH client executable. |
| [`identityFile`](core-options.md#servicestallyexecutorsnameidentityfile) | Pins the coordinator-side private key. tally disables ambient agents and interactive authentication. |
| [`knownHostsFile`](core-options.md#servicestallyexecutorsnameknownhostsfile) | Pins host-key verification input; strict checking is not optional. |
| [`program`](core-options.md#servicestallyexecutorsnameprogram) | Names the absolute tally executable on the worker. Only its fixed remote-helper command is invoked. |
| [`stateDir`](core-options.md#servicestallyexecutorsnamestatedir) | Names the persistent worker directory for exit records and captures needed by adoption and recovery. |
| [`connectTimeoutSec`](core-options.md#servicestallyexecutorsnameconnecttimeoutsec) | Bounds initial SSH connection establishment. |
| [`serverAliveIntervalSec`](core-options.md#servicestallyexecutorsnameserveraliveintervalsec) | Sets the transport liveness probe interval. |
| [`serverAliveCountMax`](core-options.md#servicestallyexecutorsnameserveralivecountmax) | Sets how many missed replies establish transport loss. |
| [`retryIntervalMs`](core-options.md#servicestallyexecutorsnameretryintervalms) | Delays fail-closed retries and is range-checked by the module. |

Transport loss does not prove process failure. The coordinator retains the
lease and retries the same generation-fenced request. Recovery adopts a remote
unit only when task identity, attempt, lease epoch, and durable worker facts
match. Prepare the remote user manager and preserve `stateDir`; deleting it can
turn a recoverable launch into an indeterminate one.

## Producers

Every [`producers.<name>`](core-options.md#servicestallyproducers) entry must
declare [`kind`](core-options.md#servicestallyproducersnamekind). The registry is
closed because producers turn external events into durable work and may mutate
the source system.

| Kind | Kind-specific surface | What it observes |
|---|---|---|
| `calendar` | [`onCalendar`](core-options.md#servicestallyproducersnameoncalendar), [`enqueue`](core-options.md#servicestallyproducersnameenqueue) | A systemd calendar firing. |
| `events-dir` | [`pollIntervalSec`](core-options.md#servicestallyproducersnamepollintervalsec) | Bounded event files in tally's state directory. |
| `gh` | [`sources`](core-options.md#servicestallyproducersnamesources), [`triggers`](core-options.md#servicestallyproducersnametriggers), polling, actor policy, mutation policy, and [`enqueue`](core-options.md#servicestallyproducersnameenqueue) | Identity-scoped GitHub notifications or searches narrowed by explicit triggers. |
| `build-effect` | [`watch`](core-options.md#servicestallyproducersnamewatch), [`path`](core-options.md#servicestallyproducersnamepath), [`onKey`](core-options.md#servicestallyproducersnameonkey) | Distinct Nix store paths from one bounded observation surface. |
| `pool-reachability` | [`probePool`](core-options.md#servicestallyproducersnameprobepool), [`intervalSec`](core-options.md#servicestallyproducersnameintervalsec), [`hysteresis`](core-options.md#servicestallyproducersnamehysteresis), [`onLost`](core-options.md#servicestallyproducersnameonlost), [`onReturn`](core-options.md#servicestallyproducersnameonreturn), [`onReturnAttest`](core-options.md#servicestallyproducersnameonreturnattest) | Hysteresis-confirmed loss or return of one configured pool. |

Producer-level [`credentials`](core-options.md#servicestallyproducersnamecredentials)
belong to the managed observer. They are distinct from enqueue credentials,
which belong to the resulting job. A pool-reachability `onReturnAttest`
payload must set `noEnqueue = true`, and only one reachability producer may own
a given probe pool.

### The shared enqueue payload

`calendar` and `gh` use `enqueue`; `build-effect` calls the same shape `onKey`;
and each reachability transition optionally uses it. The table links the
canonical `enqueue` spelling, but the sibling generated entries have identical
semantics.

| Field | Meaning in the emitted job |
|---|---|
| [`argv`](core-options.md#servicestallyproducersnameenqueueargv) | Non-empty leaf argv appended after the selected adapter prefix. |
| [`adapter`](core-options.md#servicestallyproducersnameenqueueadapter) | Name in the open adapter registry. |
| [`cwd`](core-options.md#servicestallyproducersnameenqueuecwd) | Optional absolute job working directory; GitHub intake may resolve its documented origin placeholders. |
| [`workspace`](core-options.md#servicestallyproducersnameenqueueworkspace) | Optional durable repository, base revision, branch, and worktree metadata. |
| [`adapterOptions`](core-options.md#servicestallyproducersnameenqueueadapteroptions) | Per-job environment, pre-prompt argv, approval, sandbox, model, and effort requests, each constrained by the adapter. |
| [`gateManifest`](core-options.md#servicestallyproducersnameenqueuegatemanifest) | Optional completion-artifact path, required gate IDs, and acceptance policy. |
| [`brief`](core-options.md#servicestallyproducersnameenqueuebrief) | Optional structured JSON input, content-addressed and exposed to the job through `TALLY_BRIEF` plus its `TALLY_BRIEF_HASH`; GitHub origin placeholders are resolved recursively without placing the result in argv. |
| [`pool`](core-options.md#servicestallyproducersnameenqueuepool) | Non-empty, duplicate-free atomic set of named pools; a singleton string is accepted for compatibility. |
| [`executor`](core-options.md#servicestallyproducersnameenqueueexecutor) | Optional named remote target; an unset value executes on the coordinator. |
| [`priority`](core-options.md#servicestallyproducersnameenqueuepriority) | Admission priority of this job. |
| [`dedupKey`](core-options.md#servicestallyproducersnameenqueuededupkey) | Optional bounded `strftime` template used as an existence key. |
| [`evidence`](core-options.md#servicestallyproducersnameenqueueevidence) | Canonical evidence specifications evaluated at completion. |
| [`evidenceClass`](core-options.md#servicestallyproducersnameenqueueevidenceclass) | Optional application-defined JSON copied into evidence and witness records without interpretation. |
| [`manifestHash`](core-options.md#servicestallyproducersnameenqueuemanifesthash) | Optional application-supplied identity copied verbatim; tally neither computes nor verifies it. |
| [`consumptionEstimate`](core-options.md#servicestallyproducersnameenqueueconsumptionestimate) | Authoritative non-negative debit required when any selected pool uses windowed consumption. |
| [`runtimeMaxSec`](core-options.md#servicestallyproducersnameenqueueruntimemaxsec) | Optional transient-unit runtime watchdog. |
| [`noEnqueue`](core-options.md#servicestallyproducersnameenqueuenoenqueue) | Removes the normal child-enqueue capability from leaf or advisory work. |
| [`credentials`](core-options.md#servicestallyproducersnameenqueuecredentials) | Credential source paths passed to the admitted job by `LoadCredential`. |

### GitHub mutation policy

A `gh` producer separates intake from external side effects. The relevant
switches are [`postReceipt`](core-options.md#servicestallyproducersnamepostreceipt),
[`postEvidence`](core-options.md#servicestallyproducersnamepostevidence),
[`postFailureEvidence`](core-options.md#servicestallyproducersnamepostfailureevidence),
[`postFailureStderr`](core-options.md#servicestallyproducersnamepostfailurestderr),
[`postGateSummary`](core-options.md#servicestallyproducersnamepostgatesummary),
[`requestReview`](core-options.md#servicestallyproducersnamerequestreview),
[`reviewers`](core-options.md#servicestallyproducersnamereviewers),
[`closeOnAcceptance`](core-options.md#servicestallyproducersnamecloseonacceptance),
[`closeOnPass`](core-options.md#servicestallyproducersnamecloseonpass), and
[`neverMutate`](core-options.md#servicestallyproducersnamenevermutate).
`neverMutate` overrides every acknowledgement, comment, review request, and
close. Gate summaries and acceptance-based closure require an enqueue
`gateManifest`.

`requestReview` requires a non-empty `reviewers` list of GitHub logins, and it
notifies them for real: a pull request receives GitHub's own review request,
and an issue — which has no review concept — receives one fresh comment
mentioning them. That comment is marker-idempotent rather than upserted, so a
replay does not repeat it and does not silently edit the ping out from under
the reviewers. `closeOnPass` is an independent opt-in; leaving it unset never
closes an item, whatever `postEvidence` is set to.

`postReceipt` publishes one acknowledgement per accepted or filtered trigger.
Re-observing a trigger the ledger already holds — what every producer restart
does to every historical trigger — is producer-internal bookkeeping and is
never published, so `postReceipt` stays one switch rather than splitting by
decision. That refusal lives at the decision point, not in the sink: no
acknowledgement is built for a duplicate at all, and a sink handed one reports
an error rather than dropping it, so a future sink cannot re-introduce the
public duplicate by forgetting to suppress it. Receipts and evidence comments
are sticky: tally stores the node id
of the comment it created under the producer state directory and edits that
comment in place afterwards, falling back to the marker scan only for a thread
whose comment predates the stored id or whose state was lost. A sticky edit is
exactly one round trip; the item-state assertion rides the thread scan the
create and adopt paths run anyway. That fallback
publishes into the comment it finds rather than merely adopting it, and a
publication the forge refuses fails the mutation instead of reporting success,
so a receipt on the thread is never silently stale. Steering, escalation, and
closing-summary comments are deliberately outside this primitive; they stay
fresh comments so the operator is notified.

`postEvidence` posts only passing and reused evidence, preserving its original
operator-facing meaning. Failure evidence is a separate public side effect and
is disabled by default. `postFailureEvidence` explicitly posts one idempotent
comment per failed attempt; `postFailureStderr` additionally includes the
bounded tail after conservative secret redaction and requires
`postFailureEvidence`. Redaction is defense in depth, not a guarantee that
arbitrary application secrets can be recognized, so do not enable stderr
publication merely for convenience. Failed receipts never close an item solely
because they were posted; pass and acceptance closure policies remain separate.

```text
gh producer ${name} postFailureStderr=true requires postFailureEvidence=true
```

Pass-based closure is allowed only when the evidence comment is also enabled:

```text
gh producer ${name} closeOnPass=true requires postEvidence=true
```

This keeps a passing mutation coupled to the evidence it claims to represent.

## Adapters

An [`adapters.<name>`](core-options.md#servicestallyadapters) entry is a
structured direct-argv harness for an already admitted job. It is an open map:
adding an operator-specific adapter requires no Rust change.

| Option | Operational meaning |
|---|---|
| [`argv`](core-options.md#servicestallyadaptersnameargv) | Direct argv prefix for a fresh invocation. An empty prefix is valid for the `shell` pass-through shape. |
| [`resume`](core-options.md#servicestallyadaptersnameresume) | Optional direct argv template for recovery; `%<sessionRef>%` and other declared placeholders come from captures. |
| [`resumeRequiresLaunchCwd`](core-options.md#servicestallyadaptersnameresumerequireslaunchcwd) | Declares that the harness resolves a session by the directory it was launched in, so tally refuses a continuation whose working directory differs from the recorded one. |
| [`scrape`](core-options.md#servicestallyadaptersnamescrape) | Named stdout/stderr captures, each with a stream, mode, and non-empty pattern. |
| [`trace`](core-options.md#servicestallyadaptersnametrace) | Optional advisory provider trace with an explicit stream and JSON-lines framing. |
| [`yieldHook`](core-options.md#servicestallyadaptersnameyieldhook) | Optional direct argv checkpoint used for cooperative yield. |
| [`env`](core-options.md#servicestallyadaptersnameenv) | Adapter-wide string environment. `TALLY_*` and `CREDENTIALS_DIRECTORY` names are reserved. |
| [`launch`](core-options.md#servicestallyadaptersnamelaunch) | Closed authorization for per-job argv insertion, cwd, approval, sandbox, model, and effort requests. |
| [`hardening`](core-options.md#servicestallyadaptersnamehardening) | Optional transient-unit preset. See [Hardening presets](hardening.md) for the property contract and compatibility behavior. |
| [`skillBundle`](core-options.md#servicestallyadaptersnameskillbundle) | Resolved UTF-8 skill or agent-definition content whose exact bytes can be hashed into flow provenance. |
| [`skillRevision`](core-options.md#servicestallyadaptersnameskillrevision) | Stable external revision or name used for provenance when bundle content is unavailable. |
| [`extraConfig`](core-options.md#servicestallyadaptersnameextraconfig) | JSON-serializable adapter-specific data understood by higher-level integrations. |

`skillBundle` and `skillRevision` are mutually exclusive. Supplying both emits:

```text
tally adapter ${name} skillBundle and skillRevision are mutually exclusive
```

The [`launch`](core-options.md#servicestallyadaptersnamelaunch) submodule keeps
job-supplied policy out of free-form argv:

| Launch option | Authorization boundary |
|---|---|
| [`allowPrePromptArgv`](core-options.md#servicestallyadaptersnamelaunchallowprepromptargv) | Whether a job may insert argv before the adapter's final `--`. |
| [`cwdArgv`](core-options.md#servicestallyadaptersnamelaunchcwdargv) | Optional argv template that must contain `%<cwd>%`. |
| [`approvalPolicies`](core-options.md#servicestallyadaptersnamelaunchapprovalpolicies) | Map from allowed policy names to exact argv fragments. |
| [`sandboxPolicies`](core-options.md#servicestallyadaptersnamelaunchsandboxpolicies) | Map from allowed sandbox names to exact argv fragments. |
| [`commitCapableSandboxPolicies`](core-options.md#servicestallyadaptersnamelaunchcommitcapablesandboxpolicies) | Subset of `sandboxPolicies` under which this adapter's agent can create a commit. Declaring it makes a campaign whose implementation node cannot commit an evaluation-time refusal. |
| [`model.argv`](core-options.md#servicestallyadaptersnamelaunchmodelargv) and [`model.allowedValues`](core-options.md#servicestallyadaptersnamelaunchmodelallowedvalues) | Template containing `%<value>%` plus the closed set of accepted model values. |
| [`effort.argv`](core-options.md#servicestallyadaptersnamelauncheffortargv) and [`effort.allowedValues`](core-options.md#servicestallyadaptersnamelauncheffortallowedvalues) | Template containing `%<value>%` plus the closed set of accepted effort values. |

### Scrape modes

Each named capture selects
[`stream`](core-options.md#servicestallyadaptersnamescrapenamestream),
[`mode`](core-options.md#servicestallyadaptersnamescrapenamemode), and
[`pattern`](core-options.md#servicestallyadaptersnamescrapenamepattern).
All captures are advisory: they may support resume, queries, usage observations,
or attestations, but they do not create evidence or a verdict.

| Mode | Behavior |
|---|---|
| `regex` | Applies a regular expression and retains the last match, using the first capture group when present. |
| `jsonPath` | Evaluates RFC 9535 JSONPath against each JSON value in the stream and retains the last non-null result. |
| `jsonPathLast` | Treats the whole JSON-lines stream as one array, evaluates once, and retains the last non-null match. |

### Built-in presets

The preset table below records the shapes pinned by the `adapter-presets` flake
check. Angle-bracketed values are adapter placeholders, not shell expansion.

| Preset | Fresh `argv` | `resume` | Scrape and trace shape |
|---|---|---|---|
| `shell` | empty pass-through prefix | none | no captures, trace, or yield hook |
| `pi` | `pi --mode json` | `pi --mode json --session %<sessionRef>% --model %<model>%` | `sessionRef`, `model`, and `usage` use `jsonPath`; `finalMessage` and `occupancy` use `jsonPathLast`; stdout `json-lines` trace |
| `claude-code` | `claude --print --verbose --output-format stream-json --` | `claude --resume %<sessionRef>% --model %<model>% --print --verbose --output-format stream-json --` | the same four capture modes; stdout `json-lines` trace |
| `codex` | `codex exec --json --` | `codex -C %<cwd>% exec resume --json --model %<model>% %<sessionRef>% --` | the same four capture modes; stdout `json-lines` trace |

For the session capture, `pi` uses `$.id`, `claude-code` uses
`$..session_id`, and `codex` uses `$..thread_id`. The three structured presets
also install `tally lease status` as their cooperative `yieldHook`. `codex`
declares cwd argv and named approval and sandbox policies; the presets do not
silently authorize arbitrary job-supplied flags.

`pi` declares no `usage` key mapping, and that is now a finding rather than a
gap. A real `pi --mode json` capture is checked in at
`test/fixtures/traces/pi.jsonl`, and it shows pi stating
`{ input, output, cacheRead, cacheWrite, reasoning, totalTokens, cost }` on
every assistant message and no attempt-level roll-up anywhere in the stream.
A declared spend mapping would therefore report one turn as an attempt's
usage. The per-turn reading those numbers do support is occupancy, which `pi`
declares against the same objects, scoped to assistant `message_end` events
whose `stopReason` is neither `aborted` nor `error` — pi zero-fills the usage
object on an aborted turn, and an unguarded scrape would report a fabricated
empty context for a session that is thousands of tokens full.

`pi` is also the one preset with no trailing `--`: it has no end-of-options
separator and exits 1 on one. A pi workload argv whose first element begins
with `-` would therefore be parsed by pi as a flag, so its
`launch.rejectOptionLikeWorkloadHead` declaration makes tally return a typed
pre-launch refusal before admission. `pi` additionally keys its
session store by the directory it was launched in, so a resume from a
different working directory prints `Session found in different project`,
prompts on stderr, and exits 0 without doing any work; pinning
`--session-dir` does not change that, because pi still matches the session's
recorded cwd exactly. Resume a pi node in the directory it was launched in.

## Coordinator-wide controls

The remaining fields tune one daemon and its generated units. Follow the links
for types and defaults rather than copying values from another deployment.

| Option | What it controls |
|---|---|
| [`enqueue.depthCap`](core-options.md#servicestallyenqueuedepthcap) | Maximum job-originated parent-to-child depth. |
| [`enqueue.fanoutCap`](core-options.md#servicestallyenqueuefanoutcap) | Maximum accepted children for one parent. |
| [`enqueue.requireDedupKey`](core-options.md#servicestallyenqueuerequirededupkey) | Whether job-originated child enqueue must carry a dedup key. |
| [`transport.maxFrameBytes`](core-options.md#servicestallytransportmaxframebytes) | Symmetric local protocol read/write frame bound. |
| [`scheduling.agingThresholdSec`](core-options.md#servicestallyschedulingagingthresholdsec) | Wait before a queued job receives one rank of priority aging. |
| [`lease.graceSec`](core-options.md#servicestallyleasegracesec) | Epoch-keyed restart recovery grace. |
| [`lease.yieldPollSec`](core-options.md#servicestallyleaseyieldpollsec) | Cooperative-yield checkpoint cadence. |
| [`lease.yieldGraceSec`](core-options.md#servicestallyleaseyieldgracesec) | Grace before an opted-in hard reclaim. |
| [`retention.enable`](core-options.md#servicestallyretentionenable) | Whether to generate age-based store-evidence collection. |
| [`retention.horizon`](core-options.md#servicestallyretentionhorizon) | Systemd timespan used as the witness-liveness retention floor. |
| [`retention.onCalendar`](core-options.md#servicestallyretentiononcalendar) | Collection schedule. |
| [`retention.lifecycleMaxBytes`](core-options.md#servicestallyretentionlifecyclemaxbytes) / [`lifecycleHorizon`](core-options.md#servicestallyretentionlifecyclehorizon) | Byte trigger and protected recent window for lifecycle prefix compaction. |
| [`storage.dataDir`](core-options.md#servicestallystoragedatadir) / [`storage.stateDir`](core-options.md#servicestallystoragestatedir) | Warning/hard allocated-byte budgets plus warning/hard filesystem free-space thresholds; intake probes free space live and hard pressure refuses only new work. |
| [`attestations.exec.enable`](core-options.md#servicestallyattestationsexecenable) | Whether fresh and recovered executions receive advisory per-host attestation wrappers. |
| [`gitAi.enable`](core-options.md#servicestallygitaienable) | Whether code-result revisions are bound to externally provisioned Git AI notes. tally.nix does not package `git-ai`. |
| [`gitAi.mode`](core-options.md#servicestallygitaimode) | Whether a missing or invalid binding is advisory or result-failing. |
| [`gitAi.awaitTimeoutSec`](core-options.md#servicestallygitaiawaittimeoutsec) | Settlement-barrier timeout. |
| [`gitAi.globalAwaitOk`](core-options.md#servicestallygitaiglobalawaitok) | Explicit permission for the process-global barrier on an isolated execution host. |
| [`journald.native`](core-options.md#servicestallyjournaldnative) | Native journal datagrams versus JSON stdout records. |
| [`dataDir`](core-options.md#servicestallydatadir) | Durable witness, attestation, brief, and lifecycle data. Preserve it across restarts. |
| [`stateDir`](core-options.md#servicestallystatedir) | Mutable events, captures, exit records, lease epochs, and producer state. Preserve it for recovery. |
| [`package`](core-options.md#servicestallypackage) | The tally daemon/CLI build used both to validate and run this configuration. |
| [`installTallydSymlink`](core-options.md#servicestallyinstalltallydsymlink) | Whether the installed package exposes the compatibility `tallyd` argv-zero alias alongside `tally`. |

## Home Manager and NixOS are not deployment peers

Both wrappers expose the same typed names, and both make enabled configuration
depend on tally's checked JSON derivation. That schema symmetry does not imply
unit symmetry.

Home Manager is the complete deployment surface. It renders the user daemon,
event drain, retention timer, all five producer kinds, usage-meter processes,
and scheduled flow runners. Use the generated
[Home Manager options](home-manager-options.md#servicestallyenable) when
building that topology.

The NixOS wrapper renders the system daemon and witness emitter, plus — when
[`campaignForge.enable`](nixos-options.md#servicestallycampaignforgeenable) is
set — the campaign execution surface and its poll units for forge-native
campaigns, described in
[Campaigns on a NixOS host](../flows/campaigns.md#campaigns-on-a-nixos-host).
It renders no producer, meter, or flow units at all, and can type-check those
declarations without deploying anything that drives them. Use the generated
[NixOS options](nixos-options.md#servicestallyenable) for that narrower system
surface, and do not infer a producer service merely because its option
evaluated successfully.
