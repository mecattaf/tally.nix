# Query and observability

The query surface joins four different kinds of fact without pretending they
have equal authority:

- acknowledged enqueue rows are durable admission facts;
- `lifecycle.jsonl` contains tally's execution observations;
- `witness.jsonl` is the canonical terminal record; and
- attestations and provider captures are advisory.

When they disagree, the query keeps the disagreement visible and the witness
wins for canonical verdict and usage. Querying does not mutate a job.

## Monitor tally's own disk

Use the daemon's measured view before workload-side free-space guards:

```console
$ tally query storage | jq '{intake, dataDir, stateDir, growthPerCompletion}'
```

The two store sizes use allocated filesystem blocks for budget decisions and also expose
apparent bytes and file counts. Each store reports `filesystemAvailableBytes`,
`warningFreeBytes`, and `minimumFreeBytes`; falling below the first emits an early warning and
falling below the second is hard pressure even when the store's own allocated bytes are small.
`schemaVersion` is 3: the section version 2 carried for the live task-database projection was
removed with that projection and no placeholder replaces it. The CHANGELOG entry for the removal
names the exact fields that disappeared.

Directory measurement is an off-thread, cached sample. `sampledAt` is the tree-walk age
boundary; `freeSpaceCheckedAt` is the latest cheap filesystem probe. `query storage` and
`query status` return the cache without filesystem work. Every enqueue performs only `statvfs`,
updates the free-space fields and pressure state, and never walks either tree. The periodic timer
starts one sample on every configured interval when the previous single-flight sample is done;
there is no second elapsed-time guard. If a walk overruns an interval, the next idle tick starts
the next walk. The blocking walk does not occupy the daemon's current-thread runtime, accept loop,
intake path, completion path, lease tick, or watchdog, and a blocking-worker panic cannot take
ownership of or permanently lose the monitor.

`growthPerCompletion` compares samples across canonical witness-count boundaries. Signed byte
rates make both growth and successful compaction visible. `query status` embeds the same object
under `storage`, so a future human-oriented status surface can consume it without another disk
contract.

Warning and hard transitions are fsynced to `<dataDir>/storage-warnings.jsonl` and emitted on the
daemon's journal stream. A level recovers only after allocated bytes fall below 90% of the crossed
threshold. Free space must rise above the crossed threshold by the larger of 10% or 1 GiB; this
absolute band prevents shared-filesystem noise from repeatedly closing and reopening an episode.
Warning-to-hard-to-warning changes stay in one episode until full recovery, so GitHub campaign
intake with evidence receipts enabled gets at most one idempotent issue comment for that pressure
episode.

At a hard size or free-space threshold, tally rejects only new enqueue and continuation requests
with `storage-budget-exceeded`; admitted work, retry, cancel, pause, resume, and every query remain
available. If measurement itself fails, the same intake scope is refused with the distinct
`storage-monitor-unavailable` code and `monitorError` explains the failure. Concurrent removal of
a directory below either store is treated as a normal vanished entry, not a monitor outage.

`<dataDir>/storage-metrics.json` is derived advisory state. Unsupported schema versions, foreign
fields, malformed JSON, and inconsistent episode fields are ignored at startup, journaled, and
replaced by a fresh sample. The durable warning log supplies the next episode sequence so a reset
does not collide with an earlier campaign receipt.

## Find the task anchor

`tally enqueue` returns a task UUID. Keep it: query pages call it the `anchor`,
and it remains stable across attempts even when a live systemd job ID changes.
If all you have is the live job ID, `query job` accepts that too and resolves
the task anchor.

Campaign nodes additionally expose an optional human `taskRef`, such as
`crm/t07`. It is diagnostic provenance, not a replacement for the UUID.

```console
$ tally query jobs --state running --limit 100
$ tally query jobs --state queued --pool worker-gpu
$ tally query job <task-or-live-job-uuid>
```

`query jobs` can filter by verdict, pool, executor, adapter, source, origin,
parent, flow run, session, and time. Its JSON response contains `items`,
`nextCursor`, `truncated`, `elidedItems`, and immutable snapshot metadata.

By default the command follows the cursor to the end of the window and prints
one merged envelope, so what you get is the whole filtered set. Pass `--json`
to take single-page semantics and own the cursor yourself, then pass
`nextCursor` back with the same filters:

```console
$ tally query jobs --source orchestrator --limit 100 --json
$ tally query jobs --source orchestrator --limit 100 --cursor '<opaque-cursor>'
```

A cursor is bound to its original method, filters, and snapshot. Do not edit it
or reuse it with a different query, and do not try to hold one between polls —
it is an ephemeral snapshot offset. For incremental polling see [Poll a flow
run correctly](#poll-a-flow-run-correctly).

## Follow a flow

Every child admitted by a flow carries an orchestration capsule. Group those
nodes without matching descriptions or argv:

```console
$ tally query jobs --flow-run <flow-run-uuid>
```

Each item exposes top-level `taskRef` when present, `orchestration.flowRunId`, the node ordinal, the orchestration
`scriptHash`, `argsHash`, and `catalogHash`, pool, executor, parent task, live
state, and terminal facts. The runner itself is the parent row and is not one of
the orchestrated child items unless it also carries that capsule.

Two fields make a replay auditable rather than merely asserted:

| Field | Meaning |
|---|---|
| `dedupKey` | The node's submission identity. A flow-local `key` is rendered `flow:<run-id>:k:<key>`; a node without one is `flow:<run-id>:<ordinal>`; a `dedupKey` set by the script is used verbatim. |
| `disposition` | How this row came to exist: `created`, `reused`, or `substituted`. |

`disposition` covers only admissions that write a row. An `attached` answer joins
a job already in flight, and a full-mode `reused` or `terminal` answer returns a
governing witness — none of the three writes a second row, which is exactly what
a correct replay looks like from here. Those answers appear in the runner's own
lifecycle stream instead; see [Follow a flow run's
nodes](#follow-a-flow-runs-nodes) below.

Not writing a row no longer means not joining the run: all five dispositions
record durable run membership at admission, so `query jobs --flow-run` and
`query log --flow-run` show a node the run attached to or reused even though
its row belongs to another run. See [Run membership is a durable admission
fact](#run-membership-is-a-durable-admission-fact).

To verify that a re-run replayed rather than re-executed: the node count and the
`dedupKey` set must be unchanged, and every `disposition` must still read
`created` from the first run.

## Follow a flow run's nodes

The flow runner writes one JSON object per line. Alongside `log`,
`selector-resolved`, and `flow-completed`, it emits a pair of events per node:

| Event | Emitted | Carries |
|---|---|---|
| `node-submitted` | When the daemon answers the node's enqueue | `ordinal`, `dedupKey`, `label`, `taskRef`, `disposition`, `taskUuid`, `payloadHash`, `attempt` |
| `node-terminal` | When the node's terminal result is observed | `ordinal`, `dedupKey`, `taskRef`, `disposition`, `taskUuid`, `verdict`, `witnessSeq`, `exitCode`, `errorCode` |

Unlike `log`, these are never suppressed on replay: a replayed prefix reporting
`reused` is the fact an operator needs to see. `node-submitted` follows admission
order and `node-terminal` follows the replay-stable observation order.

The same run ID filters the two ledgers:

```console
$ tally query run <flow-run-uuid>
$ tally query log --flow-run <flow-run-uuid>
$ tally query proof --flow-run <flow-run-uuid>
```

Start with `query run` when the question is “what is happening now?” It shows the
spec-build reconciler's task table, any current nodes with elapsed time and remaining runtime
budget, and failure capture pointers plus stderr tails. Use `--json` when a steering agent needs
the same compact view as structured data: `items` is the complete durable member identity list,
including members with no row, lifecycle event, or witness of their own. The reconciliation-only
`tasks` key is omitted when no such board exists, leaving one unambiguous member array.

It also answers "what did this run cost", summed from exact per-attempt
`usageEvidence.accounting` in the advisory attestation ledger and scoped by the
run's durable membership — so a retried task is charged for every
attempt and a node the run attached rather than created is inside the sum. Read the coverage
beside the number: it is a sum over advisory captures, it says how many attempts reported usage
against how many it observed, and it names every reason it is partial. `query standup` carries
the same rollup for every run its window touched and reader-state did not hide (see
[Archive a run](#archive-a-run) below, and `archivedRunsHidden` for how many were withheld),
with the three fixed statements every entry would repeat stated once instead in a digest-level
`usageBasis`.

For one job, keep the provider's statement and tally's charge visibly apart:

```console
$ tally query job <task-uuid> --json | jq '.job | {usage, usageAccounting}'
```

`usage` may be a session-cumulative raw observation. `usageAccounting` is the
fresh or checked-delta result used by rollups and the built-in meter. An
unavailable predecessor produces a typed reason and no fabricated meter update;
a legacy raw-only record remains inspectable but is excluded from confident
totals.

### When the daemon stops answering

Every read above goes through the daemon, so a stalled daemon takes observability with it —
exactly when an operator is diagnosing the stall (#431). `query run` therefore has a second
source. When a live `query.run` exceeds its RPC deadline the command falls back **automatically**
to a durable-state view read from disk, and `--durable` asks for the same view without contacting
the daemon at all:

```console
$ tally query run <flow-run-uuid> --durable \
    --state-dir /var/lib/tally/state --data-dir /var/lib/tally/data
```

The fallback is automatic rather than flag-only because a deadline is exactly the moment an
operator has no time to spend rerunning a command with a flag they have to remember. It is safe
to take automatically because it is labelled in both renderings and never claims to be live:
`--json` carries `view: "durable-state"`, `live: false`, and a `caveats` array; the human
rendering leads with the same caveats on `!` lines. The live rendering states `view: "live"` for
the same reason, so a consumer tells them apart by reading a field rather than by noticing the
absence of one. Only a *timeout* falls back; no other failure quietly changes what the command
answers.

Read it as strictly weaker than the live view. It is reconstructed from the durable enqueue
events, the verified witness ledger, the lifecycle history, durable flow membership, the advisory
attestation ledger, and the retained capture tree — so it carries the reconciled task table,
terminal verdicts, the usage rollup, and failure capture pointers. It cannot carry in-flight
state: a task the daemon is running right now has no terminal witness yet and reads as pending,
because the systemd unit facts that distinguish the two are the daemon's to collect. It is also
not a snapshot at one instant, and it is read-only — it never creates, locks, or repairs a
durable store, because a diagnostic must not be able to damage the thing it is diagnosing. Two
consequences worth stating: reading a directory that is not a tally store leaves that directory
exactly as it found it, and the view renders where you can read the daemon's data and not write
it — which is the ordinary case for an operator reading a system daemon's store.

The two directories are the daemon's own, and they default to `$XDG_STATE_HOME/tally` and
`$XDG_DATA_HOME/tally`. Pass `--state-dir` and `--data-dir` when the daemon's are elsewhere — a
NixOS deployment's are `/var/lib/tally/state` and `/var/lib/tally/data` — for the same reason
`tally reader-state` needs them: a wrong directory is not an error, it is an empty answer about a
run that is not there.

`query log` restricts the lifecycle stream to the run's nodes, resolved from the
orchestration capsule on the durable rows and the witness chain, because a
lifecycle event carries no capsule of its own. `query proof` returns one proof
per node in node-ordinal order under an `items` array, rather than requiring the
task UUIDs the operator is trying to discover; it is mutually exclusive with
`--task`, and `--attempt` applies only to `--task`.

Both spellings of the run ID work everywhere: `--flow-run` and `--flow-run-id`
are aliases on `tally query jobs`, `tally query log`, `tally query proof`, and
`tally flow run`.

## Archive a run

Once a run is dealt with, mark it so it stops crowding the standup and job
lists. Every verb below targets the daemon's own **data directory**, not the
socket — `tally reader-state` never talks to the daemon at all. An omitted
`--data-dir` resolves through `TALLY_DATA_DIR`, then `$XDG_DATA_HOME/tally`,
else `~/.local/share/tally` (#416); both modules export `TALLY_DATA_DIR` into
the operator's environment as well as onto their units, so on a deployment
the family aims at the deployment's store by default. A NixOS deployment's
daemon normally runs against `/var/lib/tally/data`, mode 0700 and owned by
the service user — so with the export inherited these verbs are refused by
name unless run as that user, which is the intended outcome. An invocation
that inherits neither the export nor an explicit flag — one run through a
privilege boundary that resets the environment, say — still resolves to the
user default, and omitting both against a different data directory than the
one the daemon reads is not an error: it creates a fresh, unrelated store,
prints a normal-looking success line, and changes nothing any `query`
command shows. Passing the flag is what settles it in every case:

```console
$ tally reader-state archive <flow-run-uuid> --tag flaky-fixture --data-dir /var/lib/tally/data
$ tally reader-state unarchive <flow-run-uuid> --data-dir /var/lib/tally/data
$ tally reader-state tag <flow-run-uuid> needs-followup --data-dir /var/lib/tally/data
$ tally reader-state untag <flow-run-uuid> --data-dir /var/lib/tally/data
$ tally reader-state show <flow-run-uuid> --data-dir /var/lib/tally/data
```

This is **reader-state**, not evidence: `archived` and the triage tag live in
their own file (`reader-state.jsonl`), outside the witness and attestation
ledgers and excluded from every hash chain. Nothing durable about the run
itself changes, and no daemon or reconciler code path can write this file —
only the CLI verb above, which writes it directly. `query run` always exposes
the current `archived` flag and `triageTag` (and prints a loud `-- ARCHIVED`
banner in its human text view, however you got to that run); `query jobs` and
`query standup` broad views default to **hiding** archived runs, add
`--archived` to see them, or `--no-archived` to say the default explicitly.
`query jobs --flow-run <id>` is instead an explicit by-ID inspection: it
always returns matching archived members with `archived: true`, just as
`query run <id>` returns the archived run. The broad-view controls
`--archived` and `--no-archived` therefore conflict with `--flow-run`.

`query standup`'s digest carries two separate hidden counts, because they
count different lists at different granularity — one archived run holding
three tasks removes one `runs` row and up to three task entries, and merging
the two numbers would make either a claim about a list it does not describe:

- **`archivedHidden`** — task entries hidden, summed across `completed`,
  `gateFails`, `cancelled`, and `inFlight`.
- **`archivedRunsHidden`** — `runs` rows (per-run cost rollups) hidden.
  `runs` is populated from both the *creating* run and every run that
  merely *attached* the task (durable membership, the W-316 shape), so a
  run that only attached a task still has its cost row hidden and counted
  here even when no task entry moved.

Both are accumulated as the collections are filtered, by the same call that
filters them and never by a separate recount — so "the list looks short" is
never silently indistinguishable from "the list is short." The `reused` and
`canonicalGpuSeconds` aggregates describe that same visible task-entry view.
After archive filtering, they are recomputed from the retained task UUIDs and
the canonical witness records: reuse uses the latest terminal
`laborClass: reused`, while GPU seconds use the canonical GPU contribution
predicate across qualifying attempts. They are not inferred from displayed
entry `gpuSeconds` and are not summed from the filtered per-run cost rows.
With `--archived`, every task entry remains visible, so both aggregates are
the whole-window values.

A reader-state store that is missing, empty, truncated, or hand-edited into
garbage degrades every query that consults it to "nothing is archived" rather
than failing the query; it carries no weight `witness verify` or `tally
attest` care about either way.

For one node, inspect all attempt lanes:

```console
$ tally query job <task-uuid>
```

The `attempts` array retains each observed `(taskUuid, attempt, leaseEpoch)`
lane. This is the useful view after a retry or daemon restart because it does
not overwrite the earlier attempt.

## Read the final agent message

The built-in `pi`, `claude-code`, and `codex` adapters declare a
`finalMessage` scrape. After the terminal acknowledgement, tally projects it
onto the job:

```console
$ tally query job <task-uuid> | jq -r '.job.finalMessage.value'
```

That field is the first-class result; there is no need to search raw JSONL
captures for the last provider event. Its authority is
`advisory-provider-capture`, not canonical evidence. A flow agent node receives
the same projected value as `NodeResult.result`. If a configured projection
does not appear within ten seconds of terminal acknowledgement, the flow node
reports `result-projection-timeout` rather than silently returning an empty
result. A structured terminal error is already authoritative and bypasses this
wait; executor request rejection is returned immediately as
`executor-validation-failed` and remains visible in the witness and
`query run` failure entry even when no capture was created.

Shell output is not automatically a trace or a final message. A custom adapter
must declare the scrape or trace explicitly.

## Inspect proof

```console
$ tally query proof --task <task-uuid>
$ tally query proof --task <task-uuid> --attempt 2
$ tally witness verify
```

`query proof` returns the selected full witness record, evidence observations,
separate advisory-attestation references, the verified chain head, and one of
three statuses:

- `verified`: a canonical witness exists and the chain verifies;
- `no-witness-expected-yet`: the selected attempt is not terminal; or
- `proof-missing`: tally observed a terminal condition but cannot find the
  witness it should have.

`proof-missing` is an incident. Preserve the data directory and inspect daemon
logs; do not manufacture a replacement record. `tally witness verify` checks
the ledger offline and should be part of restart and deployment verification.

## Check an eval coverage manifest

An adversarial eval can put a versioned coverage manifest in its Markdown
findings file. Check that standalone artifact from the repository checkout:

```console
$ python3 test/eval_manifest_check.py <findings-file.md>...
```

Every schema-valid per-file summary retains the human accounting clauses and
also carries stable machine tokens. `coverage` is `checked`, `unchecked`, or
`zero-covered`. `verification=present` means at least one declared surface's
matching `bullets[]` or `files[]` entry explicitly has `status: "covered"`;
`verification=none` means none does. Being accounted for, reused, failed,
successfully parsed, or covered only outside `expected` does not count. One
explicitly covered declaration is enough for both `coverage=checked` and
`verification=present`, even when another checked declaration failed.

The process exit contract is:

| Exit | Meaning |
|---|---|
| 0 | Every manifest is valid and checked, and each has at least one explicitly covered declared surface. |
| 1 | At least one findings file or manifest is malformed, missing, ambiguous, or has an uncovered declaration. |
| 2 | Usage error: no findings paths were given. |
| 3 | Every manifest is valid, but at least one declares no expected bullets or no expected files, so coverage was not checked. |
| 4 | Every manifest is valid and checked, but at least one has zero explicitly covered declared surfaces. |

For multiple files the precedence is 1, then 3, then 4, then 0, regardless of
argument order; usage exit 2 is immediate. No close-out is currently wired to
this checker. The exit and tokens are the stable contract for a future consumer,
without requiring it to parse the human diagnosis.

## Read lifecycle and provider traces

Lifecycle events and provider output are separate:

```console
$ tally query log --task <task-uuid> --attempt 2 --limit 100
$ tally query log --task <task-uuid> --attempt 2 --json
$ tally query log --task <task-uuid> --attempt 2 --json --provenance
$ tally query log --task <task-uuid> --after 'log-v1:00000000000000000041:00000000000000000007'
$ tally query trace --task <task-uuid> --attempt 2 --limit 100
```

The default log is a terse human transition view. It suppresses evidence observations and
collapses a terminal journal record with its canonical witness, so “started” and “passed” are
not repeated just because tally retained both authorities. `--json` retains the structured
fields with the same collapse. `--provenance` restores every journal, evidence, and witness echo
for an audit. The underlying RPC remains tally's uncollapsed durable observation history.

The trace is exposed only when
the adapter declares a JSON-lines provider stream. Trace records preserve
provider order, parsed JSON when valid, raw text, and base64 for non-UTF-8
bytes. Both each trace record and each generation summary expose `taskRef` when
the attempt belongs to a campaign task. The response also says whether the
generation is complete, unavailable, unsupported, or truncated. A running remote trace can honestly report
`remote-live-trace-unavailable`; it is never presented as an empty successful
trace.

For a campaign node, every journal/lifecycle record carries
`TALLY_TASK_REF=crm/t07`, its `MESSAGE` includes `taskRef=crm/t07`, and the
`query log` projection exposes `taskRef: "crm/t07"`. The same value is exported
to the child as `TALLY_TASK_REF`.

A `failed` log item carries `stderrTail` and `stderrTruncated`. The tail is a
lossy UTF-8 rendering bounded to 2 KiB including the omission marker; it is a
diagnostic projection, not evidence. Read it first. Inspect the retained raw
capture only when the bounded tail is insufficient.

The `completed`, `inFlight`, `gateFails`, and `cancelled` entries returned by
`query standup` likewise expose `taskRef`, so the campaign digest does not
require a UUID-to-worklist lookup.

Query reads at most 16 MiB from one capture generation. Larger local capture
files remain on disk, but the trace reports
`query-read-truncated-at-16777216-bytes`. Remote capture transfer is also
bounded to 16 MiB per stream.

## Poll a flow run correctly

This is the contract a monitor must follow. It exists because a monitor that
cannot tell "no new events" from "you are looking at a capped or stale page"
reports silence during an incident — the failure recorded in #247, where a
`query log --flow-run` window sat unchanged for three hours while thirteen
tasks merged.

Four facts drive the contract:

- The lifecycle window is ordered **oldest first**. Page one of a long run is
  therefore permanently stale by construction: it never changes no matter how
  far the run advances.
- Page cursors (`--cursor`, `page-v1:…`) are ephemeral snapshot offsets. Only
  32 snapshots are retained, and a daemon restart drops all of them, so a
  poller cannot hold one between polls.
- `--since` is a wall-clock **time filter**. It is not a stream position and
  never was.
- **Run membership is durable, written at admission.** See [Run membership is a
  durable admission fact](#run-membership-is-a-durable-admission-fact) below.
  It used to be recomputed per call, which is the mechanism behind the original
  #247 report; a run-scoped window is now evidence about the run, and
  `flowRunTasks` says how many nodes it resolved to.

### For a human at a terminal

```console
$ tally query log --flow-run <flow-run-uuid>
```

The human view follows the cursor to the end of the window inside the one
invocation, so it prints the whole filtered window rather than the first
capped page. Anything that stops it from being whole is one unambiguous line
on stderr: a page cursor that expired mid-window (the query restarts once and
says so), items whose oversized fields had to be elided, or a requested
position that predates retained history. Silence on stderr means you are
looking at all of it. The current stream position is printed there too.

### For an unattended monitor

```console
$ position=$(tally query log --flow-run <id> --json | jq -r .position)
$ tally query log --flow-run <id> --after "$position" --json
```

`--after` takes a **durable** lifecycle-stream position, `log-v1:<lifecycle>:<witness>`,
reported as `position` on every `query log` response. It survives daemon
restarts and page-cache eviction, so a poller can hold one between polls.

> `--after <position>` returning empty `items` means no event **after that
> position** matched your filter. That is the signal to act on, and it is
> sound: the filter runs on durable per-stream sequence numbers, so an
> out-of-order timestamp cannot hide an event behind it.

Read `items`, not `position`, to decide whether anything happened. `position`
is the head of the **whole** lifecycle stream at projection time, not of your
filter, so on any daemon doing other work — the normal case for a campaign,
whose runner node emits lifecycle events of its own — it advances between
polls while your filtered `items` stay empty. It is what you *hold*, not what
you *check*. Two successive polls over an idle daemon are identical apart from
`snapshot.createdAt`, which dates the projection rather than the stream.

Rules for the loop:

1. Advance the held position only from a response whose `truncated` is
   `false`. A truncated response has not shown you everything before the head
   it reports; page it out with `--cursor` first, or re-issue with a larger
   `--limit`.
2. `--json` deliberately keeps single-page semantics: the caller owns the
   cursor. `truncated`, `nextCursor`, and `elidedItems` are all in the
   envelope, so nothing is withheld silently.
3. `positionGap` in a response means the held position predates retained
   lifecycle history. Events before that boundary are gone. Treat it the way
   you treat a `query watch` gap: take a fresh whole-window read, then resume.
4. An item too large for the 48 KiB response cap is served with its largest
   text fields cut down and an `elided` object naming what was cut. A campaign
   runner whose argv embeds an issue body no longer makes its run
   unmonitorable. Only an item too large because of its *structure* is still
   an error, and that error names itself. On the default walked output the
   `elidedItems` counter is summed across every page the command walked, so it
   can exceed the per-page maximum of one.
5. A terminal transition is delivered once, at the position its **journal**
   record occupies. Tally retains two representations of a terminal fact — the
   journal record and the canonical witness — and collapses them into one item
   carrying the journal's cursor. If the witness lands after you have already
   polled past that cursor, you keep the bare `completed`/`failed` transition
   you were given and never receive the enriched row that would have carried
   `terminalVerdict`, `exitCode`, `artifactHash`, and `laborClass`. Both
   records are written in the same terminal handling, so the window is narrow,
   but it is real: **do not read a terminal verdict off the incremental
   stream.** Take it from `query proof` or a whole-window `query log`, which
   rebuild the projection from scratch and always collapse correctly.

`--since`/`--until` continue to filter by wall clock and compose with
`--after` unchanged. Use `--since` to bound a window in time; use `--after` to
resume a stream.

### Run membership is a durable admission fact

A `--flow-run` filter selects on run membership, and membership is written down
at admission time. Every admission carrying an orchestration capsule records
`(flowRunId, taskUuid)` in a durable ledger — `<data-dir>/flow-membership.jsonl`
— before the admission is acknowledged, for **all five dispositions**:
`created`, `attached`, `reused`, `terminal`, and `conflict` (which admits
nothing, and therefore joins the run to nothing).

This closed a real hole, and the shape of the hole is worth knowing because it
still describes what older data looks like. Three admissions write no durable
row of their own: `attached`, and full-mode `reused` and `terminal`. Each hands
the caller a task UUID for work that is real and running, while the row stays
with whichever run first created it. Membership used to be *recomputed* per
call by scanning rows and witness records for a capsule naming the run, so a
re-triggered campaign that attached to nodes still in flight from its previous
run got a window that showed the same items forever, with `nextCursor: null`
and nothing elided, while the run executed. No page cap was involved, so none
of the truncation machinery above fired. That is the shape the #247 report
described, and it is fixed.

A run resolves to the union of the ledger and the old scan, so nothing
regresses across an upgrade: a run whose rows were written before the ledger
existed still resolves exactly as it did, from its rows.

Every `query log` and `query jobs` response scoped to a run reports how many
tasks that run resolved to:

```console
$ tally query log --flow-run <id> --json | jq .flowRunTasks
0
```

**`flowRunTasks: 0` means the daemon holds no membership for that run ID.** That
is a narrower claim than "the run admitted nothing", and the difference matters
in exactly the incident this chapter exists for. A mistyped or stale ID is the
ordinary cause and worth checking first, but the daemon can also have lost
membership it once had:

- the ledger was deleted, or a line removed, following the
  [`repair-flow-membership-ledger`](troubleshooting.md#repair-flow-membership-ledger)
  runbook;
- compaction evicted the run, which it does only to runs that have been idle
  and hold no executing work, but which it does do;
- an admission reported `membershipDegraded`, meaning it was acknowledged with
  its membership unrecorded.

A node admitted as `created` still has its own durable row and is found by the
scan in all of those cases. A row-less node — `attached`, or full-mode `reused`
or `terminal` — does not, and is invisible again until the ledger is repaired.
The human path says all this on stderr rather than making you look.

A count *below* the number of nodes the runner reports submitting is a real
discrepancy to chase, not an expected artefact: corroborate with `tally query
run <id>` for the reconciled task table, the runner unit's own liveness, or
`tally query log --task <uuid>` for a specific node, which does not go through
run membership at all.

If the ledger itself is damaged, run-scoped queries fail loudly with
`repair-flow-membership-ledger` rather than quietly answering with a smaller
run, and flow admissions are refused before they commit anything — so there is
no half-admitted work to clean up, and re-submitting after the repair is the
whole recovery. It is plain JSONL: delete the offending line, or delete the
file, in which case membership falls back to the row scan. See
[`repair-flow-membership-ledger`](troubleshooting.md#repair-flow-membership-ledger)
for the runbook, including the one case that is acknowledged with a
`membershipDegraded` warning instead of refused.

## Resume a watch

`query watch` emits one JSON record per line for job, lifecycle, trace, proof,
pool, and producer changes:

```console
$ tally query watch
```

With no cursor it starts at the current tail. Save the `cursor` from the last
record you processed and resume after a disconnect:

```console
$ tally query watch --after 'change:00000000000000001234'
```

The durable change log retains the latest 4,096 records. If a reader falls
behind, tally returns `status: "cursor-expired"` with
`earliestAvailableCursor`, `resumeAfterCursor`, and an explicit `gap`
termination. Treat that as a missed interval: take a fresh `query jobs`
snapshot, then start a new watch. Do not pretend the stream was continuous.

## Where the files live

| Data | Home Manager default | NixOS default | Retention |
|---|---|---|---|
| Witness and attestation ledgers | `~/.local/share/tally/` | `/var/lib/tally/data/` | Append-only |
| Reader-state (`archived`, triage tag) | same data directory, `reader-state.jsonl` | same data directory | Self-compacts to one record per run past `READER_STATE_COMPACT_THRESHOLD`; outside every hash chain, written only by `tally reader-state` |
| Lifecycle history and watch log | same data directory | same data directory | Lifecycle compacts an old prefix after `lifecycleMaxBytes`, preserving `lifecycleHorizon`; watch keeps 4,096 records |
| Enqueue events, captures, unit exits, meters | `~/.local/state/tally/` | `/var/lib/tally/state/` | Selected sets only; see retention policy |
| Current stdout/raw adapter stderr | Ordinary: `<stateDir>/capture/<uuid>.out` and `.adapter.err`; task-ref node: `<uuid>.<task-id>.out` and `.adapter.err` | same layout | Accumulates |
| Failure-only stderr | Ordinary: `<stateDir>/capture/<uuid>.err`; task-ref node: `<uuid>.<task-id>.err`; atomic UTF-8 projection capped at 2 KiB, only present after `failed` | same layout | Current generation remains; archived copy follows the archive horizon |
| Older attempt captures | Ordinary: `<stateDir>/capture/archive/<uuid>/`; task-ref node: `archive/<uuid>.<task-id>/`; each retains the same stream distinction | same layout | Pruned by `captureArchiveHorizon` (30 days by default) on the coordinator |
| Worker-side remote state | configured executor `stateDir` | configured executor `stateDir` | Accumulates on the worker |

`.adapter.err` may contain benign adapter-runtime chatter on a healthy job;
the presence of current-generation `.err` is the failure signal. `.err` is not
a second raw stream. These files are private implementation storage.
Prefer the query API: it validates authority, attempt identity, bounds, and
pagination that a direct file read would have to reconstruct. Capacity planning
and the one managed GC path are covered in
[Retention and growth](retention.md).
