# CLI reference

`tally` is both an operator command and the first client of the
[Unix-socket RPC contract](../reference/rpc-protocol.md). Most online commands print one compact
JSON value to stdout. Flow runs and watches are JSONL streams; `query log` and `query run` are
human-first, and selected witness commands plus `query render --format text` also have
human-readable output. Both human-first query commands accept `--json`.

On failure, the CLI normally prints `tally: ...` to stderr. Scripts should branch on the
[documented exit classes](../reference/errors.md), then parse the structured stdout or RPC
error rather than matching prose.

## Global selection

Global options may appear before or after a subcommand:

```text
--config PATH
--socket PATH
--rpc-timeout-sec SECONDS
```

The default config is `$XDG_CONFIG_HOME/tally/config.json`, falling back to
`~/.config/tally/config.json`. Online clients use it to resolve the symmetric
`maxFrameBytes`; an explicitly selected file must exist and parse. The socket order is
`--socket`, `TALLY_SOCKET`, `$XDG_RUNTIME_DIR/tally/tally.sock`, then the temporary directory's
`tally/tally.sock`.

Every verb that resolves an omitted `--data-dir` — the direct-file family (`reader-state`,
`witness verify` and `witness compare`, `witness append`, `gc`, `history compact`, and the
producer diagnostics) and `daemon run` — takes it from `TALLY_DATA_DIR`, verbatim as the
directory, before the XDG default `$XDG_DATA_HOME/tally`, else `~/.local/share/tally`. An
explicit `--data-dir` flag wins over the variable; every unit either module renders that runs
one of these verbs is given an explicit path — `--data-dir`, or `--ledger` for the witness
emitter's `witness append` — so the variable never changes what a deployment's own units read.
With the variable unset or empty, resolution is
exactly what it was before it existed. It is taken verbatim, not searched: if it names something
that cannot hold the store, the verb fails naming that path rather than falling back to the XDG
default.

Both modules export `TALLY_DATA_DIR` alongside the data directory they configure — on their
units, and on the operator's environment (`home.sessionVariables` on Home Manager,
`environment.variables` on NixOS, both `mkDefault`), because the operator's shell is where an
omitted `--data-dir` used to resolve to a different store than the daemon's. On a NixOS
deployment that store is mode 0700 and owned by the service user, so an operator who is not
that user is now refused by name instead of quietly writing a new store elsewhere; run the verb
as the service user to change the deployment's own state.

One-shot RPC commands have a 60-second client deadline. Override it with
`--rpc-timeout-sec SECONDS` or `TALLY_RPC_TIMEOUT_SEC`; the flag takes precedence. Both values
must be positive whole seconds. Long-polling `queue.await_job` and `query.watch` calls are not
subject to this per-call deadline. For job awaits, the same duration instead bounds only the
reconnect/re-arm window after a re-armable daemon or transport failure; an intact long poll can
remain open indefinitely.

Check a rendered configuration without starting the daemon:

```console
$ tally --mode check-config --config /etc/tally/config.json
{"configuration":"valid","priorityRanks":{"interrupt":1000,"high":100,"medium":50,"low":10}}
```

The check validates the Rust runtime configuration, not the Nix module's cross-option
assertions.

## Command map

| Command | Purpose | Output on success |
|---|---|---|
| `enqueue` | Admit a job; alias of `queue enqueue`. | Admission JSON, or terminal JSON with `--wait`. |
| `gc` | Reconcile/prune witness GC roots and optionally collect the Nix store. | GC report JSON. |
| `queue` | Enqueue, continue, retry, cancel, pause/resume, drain, and await. | RPC result JSON. |
| `producer` | Preview and exercise configured GitHub producers. | Diagnostic/admission JSON. |
| `adapter` | Execute a minimal live adapter diagnostic. | Smoke result JSON. |
| `campaign` | Project, arm, poll, or inspect forge-native campaigns. | Projection, admission, or registry JSON. |
| `witness` | Verify/compare chains, verify authorship, or append an advisory observation. | Text or JSON. |
| `attest` | Run a child through the advisory execution-attestation wrapper. | Child output; child's exit. |
| `lease` | Acquire, release, or inspect an explicit reservation. | Lease JSON. |
| `daemon` | Run the daemon or drain event ingress. | Long-running process or drain JSON. |
| `query` | Jobs, run status, lifecycle, proof, traces, producers, watch, status, and pool headroom. | JSON, JSONL, or compact text. |
| `flow` | Check or execute a deterministic JavaScript flow. | Meta JSON or lifecycle JSONL. |
| `migrate` | Run a one-shot forward migration of durable state written by an older binary. | Migration report JSON. |
| `reader-state` | Archive/unarchive a flow run and set/clear its triage tag. Writes directly to disk, no daemon involved. | Reader-state record JSON. |

The installed `tallyd` symlink with no arguments is equivalent to `tally daemon run`.
`tally --mode daemon` with no subcommand is another compatibility spelling.

Several `__...` helper commands exist for systemd units and producers. They are hidden,
implementation-private, and not part of this CLI contract.

## Forge-native campaigns

Project a schema-versioned worklist into a GitHub master issue and native
sub-issues, then register that issue as desired state:

```console
$ tally campaign project WORKLIST.json --repo OWNER/REPO
$ tally campaign project WORKLIST.json --repo OWNER/REPO --issue ISSUE-URL
$ tally campaign arm ISSUE-URL [--allow-actor LOGIN]... [--wait]
$ tally campaign disarm ISSUE-URL
```

`project` accepts `--campaign-config PATH` when the worklist does not carry a
top-level `campaign` object. `--title`, `--label`, and `--task-label` control
the forge projection. On maintenance runs, omit `--title` to preserve the
master title. Managed marker sections and projected task bodies are replaced;
operator prose outside the master markers is preserved.

`arm` accepts only canonical `https://github.com/OWNER/REPO/issues/NUMBER`
locators. It binds the current `gh` identity, an actor allowlist, the checkout's
GitHub remote, and the exact executable issue-graph digest. Polling refuses
executable edits until explicit re-arm. `--no-enqueue` validates and registers
without admitting the initial pass. `--flow`, `--driver`, `--state-dir`, and
`--workspace-root` are mechanism overrides intended primarily for verification;
`--allow-test-local-forge` is required for the non-continuing local test mode.
Re-arming increments the retry generation even when the graph did not change.
`disarm` removes only the locked local registration.

The Home Manager timer invokes the same bounded scan available to operators:

```console
$ tally campaign poll --once
$ tally campaign poll --once --wait
$ tally campaign list
```

All accept `--state-dir`. `poll` prints observed, dispatched, pruned, and failed
registration counts and returns nonzero when any live registration cannot be
validated or dispatched. It prunes closed masters. See
[Campaigns](../flows/campaigns.md) for the manifest, task-brief, checkbox-proof,
and host-mechanism contracts.

## Adapter smoke

Run exactly one minimal job through the daemon's real admission, lease,
transient-unit, execution-attestation, capture, adapter-scrape, and witness path:

```console
$ tally adapter smoke shell
$ tally adapter smoke codex --cwd /work/project
$ tally adapter smoke claude-code --pool claude-window \
    --prompt 'Reply with the single word ok.'
$ tally adapter smoke codex --sandbox danger-full-access \
    --approval-policy never --assert-commit
```

The smoke's own verdict is three-valued and is reported on `verdictState`: `PASS` (exit 0),
`FAIL` (exit 1), or `VERDICT-UNAVAILABLE` (exit 5) when a result read exceeded its RPC deadline.
The third is never a statement about the adapter — see
[Exit codes](../reference/errors.md#i-could-not-read-the-verdict-is-not-the-adapter-failed).
The deadline for that read is `--rpc-timeout-sec` / `TALLY_RPC_TIMEOUT_SEC`, and the value used
is echoed on `rpcTimeoutSec`.

Every invocation is keyless, so it creates a new execution rather than reusing
an earlier pass. The job is bounded to five minutes, carries `noEnqueue`, and
records this witnessed `evidenceClass` marker:

```json
{"kind":"adapter-smoke","label":"adapter-smoke:codex","adapter":"codex"}
```

The default prompt is `Reply with the single word ok.` Agent adapters receive
that string as their workload argv. The `shell` smoke instead directly invokes
a hidden tally helper that prints `ok`; `--prompt` does not turn the shell
adapter into a free-form command runner.

`--cwd` accepts an absolute path or a path relative to the invoking process.
When omitted it defaults to the invoking process's current directory. Run an
agent smoke from a suitable repository, or pass its worktree explicitly, when
the harness enforces a trusted-working-directory precondition.

`--sandbox NAME` and `--approval-policy NAME` launch the smoke under named
policies from the adapter's `launch.sandboxPolicies` and
`launch.approvalPolicies` maps. A name the adapter never declared would render
no argv at all and silently smoke the adapter's own defaults, so it is refused
before any job is admitted.

`--assert-commit` answers the question a fixture cannot: whether this adapter,
under these policies, can do what a campaign implementation node must. It seeds
a throwaway git repository, runs the adapter in it with a write-stage-commit
workload, and then requires exactly what publication requires — a clean worktree
and at least one commit descended from the seeded base. It refuses `--cwd`,
because the point is a repository nothing else owns. The result is reported as
`commitProbe` in the diagnostic:

```json
{"status":"verified","repository":"/var/.../tally/adapter-smoke/probe-...","baseRev":"...","headRev":"...","commits":1,"worktreeStatus":[]}
```

A verified probe deletes its repository; any other status (`no-commit`,
`dirty-worktree`, `unrelated-history`, or `not-checked` when the adapter itself
failed) exits nonzero and retains the repository as the evidence. Every failure
after the repository exists names its path, including the ones that say nothing
about the commit assertion, and the repository is seeded only once the daemon
connection is open — a smoke that cannot reach the daemon at all leaves nothing
behind. Retained repositories expire on the capture-archive horizon like any
other retained evidence: `tally gc --state-dir DIR` sweeps `probe-*` under
`DIR/adapter-smoke/` and reports `adapterProbesExamined`/`adapterProbesPruned`.
This is the one-command pre-flight for a policy pairing: an agent that writes
its files correctly and cannot reach git metadata to commit them reports
`no-commit` here in seconds instead of failing publication after a full campaign
node.

**Point the probe at the state directory whose gc sweeps it.** `tally gc` reaps
`probe-*` only under the `--state-dir` *it* is given, so the probe root and the
gc root have to be the same place or nothing ever reaps a retained probe:

- `--state-dir PATH` names the state directory the default probe root derives
  from — the probe lands in `PATH/adapter-smoke/`. Without it the CLI resolves
  `$XDG_STATE_HOME/tally` (or `$HOME/.local/state/tally`). On a Home Manager
  deployment that already coincides with the module's `stateDir`. On a **NixOS**
  deployment it does not: the module's `stateDir` is `/var/lib/tally/state` and
  that is what the retention timer passes to `tally gc`, so pass
  `--state-dir /var/lib/tally/state` (as the service user, which owns it) or
  remove the retained repository by hand.
- `--probe-root PATH` names the probe directory outright and is not derived from
  any state directory. A probe seeded outside `<gc state dir>/adapter-smoke/` —
  which includes every campaign workspace root — is **not** swept by `tally gc`
  and must be removed by hand.

`--probe-root` is never the system temporary directory, for two independent
reasons. A [hardened](../configuration/hardening.md) adapter's transient unit
runs with `PrivateTmp=yes`, so a `/tmp` working directory does not exist inside
its namespace and systemd kills the unit before the adapter binary runs — a
harness failure whose empty capture reads exactly like a policy failure. And an
agent sandbox may treat `$TMPDIR` and `/tmp` as default writable roots, which
would let a confining policy pass a probe it should fail. Name the campaign's
workspace root to probe where implementation nodes actually run.

The probe repository is submitted as the job's workspace, which is how a
campaign implementation node reaches its own worktree. That is what places it in
the transient unit's `ReadWritePaths=`, so `--assert-commit` works unchanged
under every `hardening` preset without relaxing any of them.

`--pool` names one configured pool. Without it, the CLI uses the first
configured conventional lane for the adapter:

- `shell`: `build`, then `stock`, then `shell`;
- `codex`: `codex-window`, then `codex`;
- `claude-code`: `claude-window`, then `claude-code`;
- `pi`: `pi-window`, then `pi`;
- another adapter: `<name>-window`, then `<name>`.

If no matching lane is configured, the command requires `--pool`; the CLI
never guesses among unrelated pools.

The compact result names the task, selected pool and cwd, terminal verdict,
witness sequence, and capture status. For an adapter that declares
`sessionRef` or `finalMessage`, success waits for those advisory captures to be
parsed and projected. Their contents are not judged; smoke proves mechanism,
not answer quality. A missing declared projection is a diagnostic failure even
when the process exited zero.

The normal job verdict determines the ordinary CLI exit class. On a failed
verdict, smoke prints the final 2 KiB of the retained stderr capture before the
final error line. An empty capture is stated explicitly. The same tail is
available for every failed job through `queue await-job` and `query log`, not
only smoke jobs. Raw adapter stderr lives in `.adapter.err`; the failure-only
`.err` file is absent on success.

## Enqueue

Supply one or more pools and exactly one invocation form:

```console
$ tally enqueue \
    --pool local-ai \
    --priority high \
    --dedup-key review:42 \
    --evidence exit:0 \
    --wait \
    -- codex exec "review issue 42"
```

```console
$ tally queue enqueue \
    --pool build \
    --invocation "nix build '.#checks.x86_64-linux.doc'"
```

`-- <ARGV>...` preserves arguments directly. `--invocation STRING` uses tally's small
shell-like tokenizer; it handles whitespace, single/double quotes, and backslash escaping but
does not invoke a shell.

### Admission flags

| Flag | Contract |
|---|---|
| `--pool NAME` | Required and repeatable; all named pools are co-leased atomically. |
| `--executor NAME` | Optional configured execution target. |
| `--priority CLASS` | `interrupt`, `high`, `medium`, or `low`; default `medium`. |
| `--adapter NAME` | Configured adapter; default `shell`. |
| `--cwd PATH` | Working directory. Defaults to the supplied workspace worktree. |
| `--env NAME=VALUE` | Repeatable adapter environment. `TALLY_*` and `CREDENTIALS_DIRECTORY` are reserved. |
| `--pre-prompt-arg VALUE` | Repeatable adapter pre-prompt argument. |
| `--approval-policy`, `--sandbox-policy`, `--model`, `--effort` | Per-job adapter overrides validated by the adapter. |
| `--workspace-repo`, `--workspace-base-rev`, `--workspace-branch`, `--workspace-worktree` | All four are required together. |
| `--brief JSON` / `--brief-path PATH` | Mutually exclusive structured brief sources. |
| `--source SOURCE` | `manual` by default; also `orchestrator`, `calendar`, `events-dir`, `gh`, `build-effect`, `pool-reachability`. |
| `--dedup-key KEY` | Deduplication key. |
| `--submission MODE` | `full` or `legacy`; defaults to `full` when a dedup key is present. |
| `--orchestration JSON` | Opaque flow provenance capsule. |
| `--parent UUID` | Durable parent task identity. |
| `--evidence SPEC` | Repeatable canonical evidence specification. |
| `--evidence-class JSON`, `--manifest-hash VALUE` | Witnessed evidence/manifest metadata. |
| `--consumption-estimate N` | Direct admission estimate required by a windowed-consumption pool. |
| `--runtime-max-sec N` | Positive execution watchdog. |
| `--no-enqueue` | Forbid child enqueue from this job. |
| `--related-trigger JSON` | Fallback producer-trigger provenance. |
| `--wait` | Await the admitted/reused job and map its terminal verdict to the CLI exit. |

Gate-manifest flags are deliberately all-or-nothing at the CLI:

```text
--gate-manifest PATH
--required-gate ID       (at least one, repeatable)
--acceptance-policy manual|execution-and-gates
```

The CLI is narrower than the RPC here: it will not send a manifest spec with an empty required
gate list.

The exact RPC request fields, canonical payload hash, and result shapes are in
[the enqueue protocol](../reference/rpc-protocol.md#enqueueparams).

### Submission mode

With `--dedup-key`, the public enqueue command defaults to **full mode**. Identical live
submissions attach to the existing task, while the same key with a different canonical payload
fails with `dedup-key-conflict`. Pass-witness reuse and the other full-mode dispositions follow
the enqueue protocol.

`--submission legacy` is the compatibility escape hatch. It omits the RPC `submission` object
and retains pass-witness reuse without live attachment or live conflict detection. Without
`--dedup-key`, the CLI omits `submission` regardless of the selected mode, so keyless enqueue
requests are unchanged.

`--wait` does not make enqueue one blocking RPC. The CLI first admits, then calls
`queue.await_job` using the returned `task_uuid`. If a daemon restart interrupts that await, the
CLI reconnects with backoff and reissues only the idempotent await for up to 60 seconds by
default (or the selected RPC timeout). The enqueue RPC itself is never retried.

## Queue control

### Continue and retry

Continue a terminal adapter session with new arguments:

```console
$ tally queue continue TASK-UUID --wait -- "address the review"
```

The old job must be terminal and have a scraped session reference. Pools, executor, adapter,
priority, source, workspace, and adapter options are inherited. The new admission has a new task
identity and points back through `resumeFrom`.

Retry a terminal failed task in place:

```console
$ tally queue retry TASK-UUID
```

Retry keeps the UUID, increments `attempt`, and requires a governing terminal non-pass witness.
A pass cannot be retried.

### Cancel, pause, and resume

```console
$ tally queue cancel TASK-UUID
$ tally queue cancel TASK-UUID --force
$ tally flow cancel FLOW-RUN-ID
$ tally queue pause local-ai
$ tally queue pause --all
$ tally queue resume local-ai
$ tally queue resume --all
```

Without `--force`, cancel affects paused/queued work but leaves running work untouched. Forced
cancel reclaims running execution and emits a cancelled witness. Pausing a pool withdraws its
queued lease requests and marks those jobs paused; it does not preempt running holders. Resume
re-admits jobs only when none of their required pools remains paused or unreachable.

`tally flow cancel` force-cancels every nonterminal child in the named flow run. This is a
flow-scoped operation: unlike `tally queue cancel` without `--force`, running children are not
silently left behind.

### Drain and await

```console
$ tally queue drain
$ tally daemon drain
$ tally queue await-job TASK-UUID
$ tally queue await-barrier 'barrier:TASK-UUID:1'
```

The two drain spellings call the same `queue.drain` method. They claim pending event-directory
files and return a snapshot barrier for all active jobs. The CLI does not expose the RPC
`producer` filter.

They differ in one place. `tally daemon drain` is what `tally-drain.timer` runs every five
seconds, and a daemon restart takes longer than that, so it treats an unreachable socket as
nothing to drain and exits **0** rather than 3 — otherwise every activation that restarts the
daemon raises a unit failure indistinguishable from a real one. The line naming the unreachable
socket is still written to stderr, so an operator running it by hand still sees which case it was.
It also absorbs the busy-daemon shape: a daemon that connected but did not answer `queue.drain`
within the client deadline (60s by default; `--rpc-timeout-sec` / `TALLY_RPC_TIMEOUT_SEC`)
records a retryable skip and likewise exits **0**, because the producer event files are durable
on disk and the next tick drains them — nothing is lost. The line naming the expired deadline is
written to stderr the same way. Every other drain failure — including a daemon that is listening
and refuses — keeps its exit code. `tally queue drain` is unchanged and still exits 3 on an
unreachable socket, and 1 when the deadline expires. The unit also
carries `ConditionPathExists` on the socket, so a drain scheduled while the daemon is down is
recorded as a skipped start rather than being invoked at all.

The limit of that, on the record: a daemon that crashed and left its socket file behind satisfies
the condition, is then refused at connect, and is absorbed as an absence like any other — so a
quiet `tally-drain` no longer distinguishes a healthy daemon from a dead one whose socket outlived
it. The alarm for that case is `tally-daemon`'s own unit failure, which is where it belongs; drain
silence is not evidence the daemon is up.

`await-job` and `await-barrier` print raw terminal JSON. Unlike `enqueue --wait`, they do not map
a failed returned verdict to a non-zero process exit; inspect the JSON in scripts. Job awaits
use the same bounded reconnect/re-arm window as `enqueue --wait`. Exhausting that window is an
unreachable-daemon exit (3). Drain barriers cannot be re-armed.

## Query

### Jobs and flow grouping

```console
$ tally query jobs --state running --pool local-ai --limit 100
$ tally query jobs --flow-run RUN-UUID
$ tally query jobs --cursor 'OPAQUE-CURSOR'
$ tally query job TASK-UUID
$ tally query run RUN-UUID
```

`query jobs` filters:

```text
--state / --live-state
--verdict / --terminal-verdict
--pool
--executor
--adapter
--source
--origin
--parent
--flow-run
--session
--since
--until
--limit
--cursor
--json
```

`--flow-run` matches the witnessed/durable `orchestration.flowRunId` and is the supported way to
group a runner's nodes; `--flow-run-id` is an accepted alias, so the spelling `tally flow run`
uses works here too. Each item carries the node's `dedupKey` and the `disposition` that wrote its
row. Pagination cursors are daemon-memory snapshots: repeat the same filters, and restart from the
beginning if the daemon or cursor snapshot is gone.

By default `query jobs` follows the cursor to the end of the window inside the one invocation and
prints a single merged envelope; `--json` or an explicit `--cursor` keeps single-page semantics
and hands the cursor back to the caller. `--limit` sizes a page, not the result set — when the
command is following cursors it changes how many round trips the window takes, not how much of
the window you see. Narrow the window with the filters, not with `--limit`. Either way the envelope carries `truncated` and
`elidedItems`, and anything that stops the output from being the whole window is stated on
stderr.

### Run status, proof, log, and trace

```console
$ tally query status
$ tally query status --pool build
$ tally query storage
$ tally query run RUN-UUID
$ tally query run RUN-UUID --status blocked
$ tally query run RUN-UUID --json
$ tally query lineage RUN-UUID
$ tally query proof --task TASK-UUID --attempt 2
$ tally query proof --flow-run RUN-UUID
$ tally query log --task TASK-UUID --attempt 2 --event completed
$ tally query log --flow-run RUN-UUID
$ tally query log --flow-run RUN-UUID --json
$ tally query log --flow-run RUN-UUID --json --provenance
$ tally query log --flow-run RUN-UUID --after 'log-v1:00000000000000000041:00000000000000000007'
$ tally query trace --task TASK-UUID --attempt 2 --limit 100
```

`query run` prints the flow state and a per-task table. A spec-build pass shows every
`campaign/task-id` as done, running, blocked, or pending; `--status <state>` narrows that table
to one of those states while the summary counts stay whole-run, which is how a 128-task campaign
board stays readable. Campaign anomalies print above the board, never inside it, and put the run
in `needs-attention`: a sub-issue closed by hand while its task holds no revision-valid merged
pull request completes nothing, and a reader who misses that debugs the wrong surface. Each
anomaly line names the task, the sub-issue URL, and what is missing. A sub-issue the campaign
closed itself — by merging a pull request that carried `Closes #<sub-issue>` — is never reported
here, however stale that pull request's revision marker has since become; it is a pass warning,
not operator error. A flow with no reconciled task table reaches `complete` once every one of
its nodes holds a passing terminal verdict. Its current-node section includes elapsed time and
the remaining `runtimeMaxSec` budget, negative when a node has run past that budget; its failure
section prints the retained failure capture path — or `<not retained>` when none exists — and
the bounded stderr tail with its indentation intact. Terminal escape sequences written by an
adapter are stripped from every human rendering. `--json` emits the same compact projection as a
structured object.

Under the task counts, `query run` answers what the run cost. The line is deliberately never a
bare total: the token sum arrives with the attempts it covers, the member tasks those attempts
belong to, and the grade of the evidence, because these numbers are advisory adapter captures
summed per attempt, not a bill. Whether any attempt reported usage is read from the coverage
count, so a run where none did prints exactly that and no component line underneath it. The second
line spells out `fresh input N (= input N + cache write N)` — `inputTokens` alone understates any
harness that writes to a prompt cache — and marks reasoning tokens as nested inside the output
figure rather than beside it. A component **no attempt reported** prints `--`, never `0`: a
measured zero is a measurement and keeps printing `0`, and the two must not read alike. A cost
line appears only where a harness reported cost, and carries the daemon's own basis sentence,
which states that tally's cgroup `charge` is a separate figure that is not summed there and is a
floor. A final `partial:` line names every reason the sums are incomplete: member tasks the
attestation ledger holds nothing about, attempts that reported no usage, attempts whose reported
usage no declared mapping could read, a component some reporting attempt did not report
(`partial-components` — this is what one renamed harness key looks like), an attempt that
reported only a harness total beside attempts that reported components, so the component lines
cover fewer attempts than the total does (`total-only-attempts`), a total mixing harness-stated
and derived figures. To find which component drifted, read the per-component
`attempts` counts in `--json`: it is the one whose `attempts` is below
`coverage.attemptsReportedWithComponents`. Do **not** look for the `--` on the line above — that
only appears when *no* attempt reported the component, so on any multi-attempt run the drifted
component prints a real-looking partial number instead. No `partial:` line means the rollup covers
every attempt the ledger could speak for. `query standup` carries the same rollup per run
its window touched, under `runs`. See the [RPC protocol
reference](../reference/rpc-protocol.md#usage-rollups) for the full field set. A run retired by `tally flow supersede` reads `superseded` whatever its own
node verdicts say, and names its successor above the board — a reader who misses that would wait
for progress that can never come.

`query lineage` answers the generation question for any run, including one that has never been
superseded: `superseded`, `supersededBy`, `supersedes`, the whole `chain` oldest-first, and
`currentFlowRunId` — the run that should actually be started.

`query log` prints terse human transition lines by default. Evidence observations and the
second journal/witness representation of a terminal fact are collapsed, so a node normally
appears once when queued, started, and passed or failed. `--json` keeps the structured fields
while applying the same collapse. Add `--provenance` to either rendering mode to preserve every
journal, evidence, and witness record. An explicit `--event evidence_pass` or
`--event evidence_fail` keeps the requested evidence observations even without
`--provenance`.

`query log` additionally filters by `--session`, `--source`, `--since`, and `--until`, and
supports `--cursor` and `--after`. The human renderer follows the cursor to the end of the window
inside the one invocation, so it prints the whole filtered window rather than the first capped
page; it writes one unambiguous stderr line for anything that stops the window from being
complete — an expired cursor (it restarts once and says so), elided oversized fields, or a
position that predates retained history — and reports the current stream position. `--json` keeps
single-page semantics and marks the page with `truncated`, `nextCursor`, and `elidedItems`.

`--after` takes a durable lifecycle-stream position, `log-v1:<lifecycle>:<witness>`, taken from
the `position` field of a previous response. It is not `--since`, which remains a wall-clock time
filter, and it is not `--cursor`, which is an ephemeral page offset; a page or watch cursor
handed to `--after` is refused rather than misread. `--after` plus empty `items` means nothing after that
position matched the filter; read `items` rather than `position`, which is the head of the whole
lifecycle stream and advances whenever anything else on the daemon does. A run-scoped response
also reports `flowRunTasks`. Run membership is a durable admission fact, written for all five
dispositions before the admission is acknowledged, so `flowRunTasks: 0` means the daemon holds no
membership for that run ID — usually a mistyped or stale ID, but also a repaired or deleted
ledger, a compacted-out idle run, or an admission that reported `membershipDegraded`. See [Poll a flow run
correctly](observability.md#poll-a-flow-run-correctly) for the full monitoring contract and
[Run membership is a durable admission
fact](observability.md#run-membership-is-a-durable-admission-fact) for what replaced the #247
seam.
`query trace` also supports page cursors. Proof is not just a witness
lookup: it reports whether a witness is expected, returns the canonical record when present,
separates advisory attestations, and includes ledger verification state.

`query storage` is the daemon's cached disk-pressure view. It reports both stores' allocated and
apparent bytes, filesystem-available bytes, configured size and free-space warning/hard levels,
and growth per canonical completion. `sampledAt` identifies the off-thread tree sample; `freeSpaceCheckedAt` identifies
the latest periodic or admission-time `statvfs` check. The query itself never walks the stores.
At a hard level, `intake.accepting` is false; existing work and this query remain usable.

Each `failed` log item includes the bounded `stderrTail` and a
`stderrTruncated` boolean. Start there before constructing a capture path by
hand. The current generation's `.err` is the same bounded diagnostic
projection; raw adapter bytes remain in `.adapter.err`.

`query log --flow-run` restricts the lifecycle stream to one run's nodes. A lifecycle event
carries no orchestration capsule, so the run's task UUIDs are resolved from the durable rows and
the witness chain, which do.

`query proof --flow-run` returns one proof per node in node-ordinal order under `items`, instead
of requiring the task UUIDs the operator is trying to discover. It is mutually exclusive with
`--task`, and `--attempt` applies only to `--task`.

### Producers, pools, render, and stand-up

```console
$ tally query producers
$ tally query producers --name flow-monthly --kind calendar
$ tally query pools
$ tally query render --format json
$ tally query render --format text
$ tally query standup --since 2026-07-27T00:00:00Z
```

The RPC `query.render` supports an additional `scope` field, but the current CLI does not expose
it and always requests the default `all` scope. Likewise, the RPC stand-up method supports a
`source` filter that the CLI does not expose. Every completed, in-flight, gate-failed, or
cancelled stand-up entry includes `taskRef` when the job belongs to a campaign task.

`query jobs` and `query standup` both take `--archived` (include jobs/entries whose creating run
is archived reader-state) and `--no-archived` (the default, spelled explicitly). `query standup`'s
digest additionally carries `archivedHidden` (task entries hidden) and `archivedRunsHidden` (`runs`
rows hidden) — two separate counts, since one archived run can hold several task entries. See
[Archive a run](observability.md#archive-a-run).

### Watch

```console
$ tally query watch
$ tally query watch --after 'change:00000000000000000123'
```

Watch prints each change record as its own JSON line. With no cursor, the first poll seeds at the
current head and prints no historical items; the command then polls every 500 ms. The durable
change log retains 4,096 entries. A gap prints the `cursor-expired` envelope and exits 2; decide
whether the missing interval is acceptable before resuming from `resumeAfterCursor`.

## Flow

Check syntax, metadata, deterministic-global restrictions, arguments, catalog requirements, and
configured pool closure without contacting the daemon:

```console
$ tally flow check ./review.js \
    --args '{"subject":"https://example.invalid/change/42"}' \
    --catalog /nix/store/...-tally-catalog.json
```

On success, `flow check` prints the script's normalized meta object as compact JSON. Use
`--args-path args.json` instead of `--args JSON` when the input should stay out of the checker's
process argv; the two flags are mutually exclusive. Manual argument paths may be relative and may
resolve through a symlink, but the target must be a regular file no larger than 16 MiB.

Run a flow:

```console
$ tally flow run ./review.js \
    --flow-run-id 018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321 \
    --args '{"subject":"change-42"}' \
    --max-nodes 1000 \
    --catalog /nix/store/...-tally-catalog.json
```

Flow arguments default to `{}`. `--args JSON` and `--args-path args.json` are mutually
exclusive. Job-launched runners can instead use `--args-from-brief`, which reads the private JSON
file named by inherited `TALLY_BRIEF` and verifies its canonical bytes against
`TALLY_BRIEF_HASH`; both variables are required. Generated flow and campaign producers use this
form so the structured input never appears in runner argv. `--max-nodes` defaults to 1,000 and is intersected
with a smaller literal `meta.maxNodes`. `--flow-run` is an accepted alias for `--flow-run-id`, so
one spelling carries across `tally flow run` and `tally query`. A run ID is required from
`--flow-run-id` or inherited `TALLY_TASK_UUID`; it must be a UUID. When the runner itself is a tally
job, `TALLY_TASK_UUID` and `TALLY_JOB_ID` must be present together and valid.

The runner emits lifecycle objects as JSONL and ends with either `flow-report` or
`flow-failed`. Each node contributes a `node-submitted` object when the daemon answers its enqueue
and a `node-terminal` object when its result is observed; both carry the node's `ordinal`,
`dedupKey`, and `disposition`, and neither is suppressed on replay. Script/determinism errors exit 10; replay divergence or a changed script in the
same run exits 20; flow-scoped cancellation exits 4 unless the cancelled node used
`settle: true`. See the [full mapping](../reference/errors.md#structured-flow-errors).

Retire a run that can no longer be replayed, and name what replaces it:

```console
$ tally flow supersede \
    --flow-run-id 018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321 \
    --new-flow-run-id 019a1c33-64d8-7f02-9b41-5b0d2e7c8a55 \
    --reason generation-change
```

`--reason` is required and closed: `generation-change`, `script-changed`, `args-changed`,
`catalog-changed`, or `operator`. The old run keeps every row, witness, and history record it
had; only the relationship becomes durable. Repeating the identical command answers
`disposition: "reused"` and writes nothing, so an unattended supervisor may retry it after its
own restart — but idempotency is keyed on the whole triple, so persist the successor UUID before
calling rather than minting a fresh one each attempt. Afterwards, replaying the old ID exits 20
with `flow-run-superseded` naming the successor, and the successor — which must not have started
yet — runs as an ordinary fresh run.

Run IDs are canonicalized to hyphenated lowercase, so a pasted upper-case or unhyphenated UUID
names the same run as the runner's own rendering. `--flow-run-id` must name a run that already
has a durable node; anything else exits 4 (`not_found`), which is how a typo is caught rather
than recorded against nothing. A supersede that contradicts durable lineage, or that would
strand unfinished nodes, is refused with `flow-lineage-conflict` and exits 1.

### Catalog is flag-only

The selector catalog travels exclusively through `--catalog PATH`. There is no
`TALLY_FLOW_CATALOG` channel, and one is not reserved for future use. A Nix-rendered producer
adds the flag only when `services.tally.flows.<name>.catalog` is non-null. The option itself is
optional; a script whose literal meta declares selector use fails checking unless a catalog is
provided.

### Declarative runner pool and `workloadMutex`

The generated calendar producer renders the runner with:

```json
{"pool":["flow"]}
```

It always requests `flow`. If `services.tally.flows.<name>.workloadMutex` is
non-null, the generated pool value is instead `["flow","<mutex>"]`. The extra
pool must be a named capacity-1 co-residency mutex; arbitrary extra runner
pools, reserved `flow`/`build`, and windowed-consumption pools are rejected.
The mutex lasts for the runner process. Death or preemption releases it, and a
replay waits behind whichever run acquired it next.

Direct `tally flow run` holds no runner lease. For a mutex-declaring flow,
enqueue the runner as a parent job with both `--pool flow` and
`--pool <mutex>`; direct invocation is not sanctioned. When the configured
flow registry matches the invoked script, a missing inherited parent identity
returns `FlowStartupError` code `workload-mutex-parent-required` before daemon
connection. Flows without a mutex may still run directly, with the documented
depth/fanout bypass.

The former `budgetPool` option has been removed because it never affected the
runner or child pool set. Declaring it now fails configuration with guidance to
use priorities for flow contention or `workloadMutex` for process-scoped
exclusion. `workloadMutex` is the sole typed extra runner pool; it is not a
general run-lifetime pool list.

## Witness

Verify the canonical and adjacent advisory ledgers:

```console
$ tally witness verify
$ tally witness verify /srv/tally/witness.jsonl --format json
$ tally witness verify \
    --ledger /srv/tally/witness.jsonl \
    --attestations /srv/tally/attestations.jsonl \
    --exec-attestations /srv/worker/exec-attestations.jsonl
```

Compare per-host execution observations with coordinator canon:

```console
$ tally witness compare \
    --canon /srv/tally/witness.jsonl \
    --attestations /srv/worker-a/exec-attestations.jsonl \
    --format json --strict
```

`--data-dir DIR` selects `DIR/witness.jsonl` instead of `--canon`. `--attestations` is required
and repeatable.

Verify a Git AI authorship binding:

```console
$ tally witness verify-authorship \
    --ledger /srv/tally/witness.jsonl \
    --repository /work/repo \
    --task TASK-UUID \
    --attempt 1 \
    --lease-epoch 7 \
    --format json
```

Append an **advisory** observation:

```console
$ tally witness append --payload '{"kind":"operator-note","value":"checked"}'
```

Despite its name, `witness append` defaults to `attestations.jsonl`; it cannot append canonical
verdict records. Record bytes, verification reports, tamper diagnostics, and comparison
semantics are in [Witness format and verification](../reference/witness-format.md).

## Explicit leases

```console
$ tally lease acquire gpu mutex
$ tally lease status LEASE-ID
$ TALLY_JOB_ID=TASK-UUID tally lease status
$ tally lease release LEASE-ID
```

`acquire` accepts one or more pools and returns a grant or queued ticket. With no lease argument,
`status` resolves the current job's lease through `TALLY_JOB_ID`. These are explicit reservation
tokens; they do not turn a flow runner into a full-run multi-pool lease.

## Producer diagnostics

The current public producer commands exercise configured GitHub producers:

```console
$ tally producer preview github-worklist
$ tally producer poll github-worklist --once --no-enqueue
$ tally producer explain github-worklist --item 'https://github.com/owner/repo/issues/42'
$ tally producer test github-worklist \
    --item 'https://github.com/owner/repo/issues/42' \
    --event label \
    --actor operator
```

All accept `--state-dir PATH`. `poll` currently requires `--once`; omitting it is exit 2.
`preview`, `poll --no-enqueue`, and `test` without `--promote` are non-admitting. `test` events
are `command-comment`, `mention`, `assignment`, or `label`. `--promote` performs the real
admission path and conflicts with `--no-enqueue`.

These commands use the local config and producer state directly, not the public RPC method
table. `query producers` is the read-only daemon inventory.

## Daemon

Run in the foreground:

```console
$ tally daemon run \
    --config /etc/tally/config.json \
    --socket /run/user/1000/tally/tally.sock \
    --state-dir /var/lib/tally/state \
    --data-dir /var/lib/tally/data \
    --cpu-weight 100 \
    --memory-max-bytes 8589934592 \
    --yield-grace-sec 20
```

`--cpu-weight` and `--memory-max-bytes` are required either as flags or through
`TALLY_CPU_WEIGHT` and `TALLY_MEMORY_MAX_BYTES`. The systemd module supplies them. `--mock`
prints `tally mock daemon ready` and does not open a real daemon.

`tally daemon drain` is an online client command, not a daemon lifecycle action.

## Retention, derived view, and execution wrapper

Inspect and then apply retention:

```console
$ tally gc --horizon 30d --dry-run
$ tally gc --horizon '30d' --collect
```

The horizon accepts systemd-like components such as `30d`, `1h 30min`, or `1.5h`.
`--dry-run` reports without changing roots. Without `--collect`, an applying run reconciles and
prunes tally's witness GC roots but does not invoke Nix store collection. `--data-dir` must be
absolute.

Run a command through the advisory execution wrapper:

```console
$ tally attest exec \
    --task-uuid TASK-UUID \
    --attempt 1 \
    --lease-epoch 7 \
    --payload-hash sha256:... \
    --adapter codex \
    --evidence exit:0 \
    --ledger /var/lib/tally/exec-attestations.jsonl \
    -- command arg
```

`--task-uuid`, `--attempt`, `--lease-epoch`, and a non-empty trailing argv are required.
`--brief-hash`, `--executor`, and repeatable `--evidence` are optional. The wrapper inherits
stdio, environment, and cwd, appends one advisory execution observation when it can, and
propagates the child's status.

## One-shot forward migrations

`tally migrate` holds the migrations that repair durable state written by an older tally. These
are not compatibility shims: the read paths stay strict, and each verb exists so the error that
refuses old state can name one documented command.

```console
$ tally migrate unit-exit-labels --state-dir /var/lib/tally/state
$ tally migrate unit-exit-labels --state-dir /var/lib/tally/state --apply
```

Rewrites `unit-exit/<uuid>.json` records that name `tally-job-<uuid>.service` for a row whose
orchestration carries a `taskRef`, which now owns `tally-job-<campaign>-<task>-<uuid>.service`.
Without `--apply` the plan is printed and nothing is written; read `rewritten` first.

`--state-dir PATH` must be absolute. Without it the CLI resolves `$XDG_STATE_HOME/tally` (or
`$HOME/.local/state/tally`), exactly as [`tally gc`](#retention-derived-view-and-execution-wrapper)
does. On a Home Manager deployment that already coincides with the module's `stateDir`. On a
**NixOS** deployment it does not: the module's `stateDir` is `/var/lib/tally/state`, so pass it
explicitly — and run the command **as the service user, which owns that directory**. Exit
records are written mode 0600 with no ownership repair, so a record rewritten under `sudo` is one
the daemon can no longer read. The startup refusal prints the correct absolute path; copying it
from there is the safe route.

A state directory that is not a coordinator's — a typo, or a worker's — is refused rather than
reported clean: both the directory and its `events/` must exist. There is no configuration key
for the state directory, so `--config` does not select one; it is read only for the
`executors.<name>.stateDir` values used to name remote records (below), and an absent default
config file is not an error.

The report is JSON:

```text
schemaVersion, applied, stateDir, labeledRows, rewritten[], alreadyLabeled, skipped[]
```

`rewritten[]` carries `uuid`, `path`, `recordedUnit`, and `expectedUnit`. `skipped[]` carries
`uuid`, `expectedUnit`, a `reason`, and — for a remote-owned row — `executor`, `recordPath`, and
`preLabelUnit`. Re-running is a no-op: already-labeled records are counted, not rewritten. Only
the `unit` field changes; every other field round-trips untouched and the witness ledger is not
read or written.

**This command repairs records on the coordinator only.** The labeled name is derived from the
durable rows, which exist only here — a worker runs no tally daemon and has no `events/` — so
running it on a worker reads nothing and rewrites nothing. Remote-owned rows are therefore
reported, never repaired, and `skipped[]` carries everything the hand repair on that host needs.
See [recovery](recovery.md#startup-refuses-pre-label-unit-exit-records) for the procedure.

```console
$ tally migrate capture-labels --state-dir /var/lib/tally/state
$ tally migrate capture-labels --state-dir /var/lib/tally/state --apply
```

Moves `capture/<uuid>.*` entries — and the `capture/archive/<uuid>/` directory — to the
`<uuid>.<task>` stem the current binary derives for a row whose orchestration carries a
`taskRef`. Same generation gap as `unit-exit-labels`, in the capture stem rather than the unit
name, and no error names it: the daemon starts clean and `tally query run` simply reports such a
failure as having no capture, because `capturePath`/`stderrTail` resolve through the derived
stem with no bare-uuid fallback.

Without `--apply` the plan is printed and nothing moves; read `renamed` first. The report is
JSON:

```text
schemaVersion, applied, stateDir, labeledRows, renamed[], alreadyLabeled, skipped[]
```

`renamed[]` carries `uuid`, `from`, and `to`. `skipped[]` carries `uuid`, a `reason`, and — for a
remote-owned row — `executor`, `captureDir`, `preLabelStem`, and `expectedStem`. Contents, modes
and mtimes are untouched; only names change. `unit-exit/<uuid>.json` and
`unit-exit/<uuid>.capture.json` are keyed on the bare uuid under both binaries and are not moved.
An entry that exists under both stems is listed in `skipped[]` rather than resolved, because this
command does not choose between two captures. The same `--state-dir`, ownership, and
coordinator-only rules stated above apply unchanged.
