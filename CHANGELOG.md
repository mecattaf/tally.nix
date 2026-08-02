# Changelog

All notable changes to tally.nix are recorded here. The format is based on
[Keep a Changelog], and the project intends to follow [Semantic Versioning] once version tags are
authorized.

## [Unreleased]

### Added

- Added `launch.commitCapableSandboxPolicies` to the adapter surface: the
  subset of an adapter's sandbox policies under which its agent can create a
  commit. Naming any other policy for a campaign implementation node is now
  refused when the module is evaluated and again when the campaign is armed,
  rather than mid-run after the agent has done all of its work.
- Added `--sandbox`, `--approval-policy`, and `--assert-commit` to
  `tally adapter smoke`. The probe seeds a throwaway git repository, runs the
  real adapter binary in it under the named policies, and requires what
  publication requires — a clean worktree and a commit descended from the
  seeded base — reporting the outcome as `commitProbe` and retaining the
  repository when it fails. This is the pre-flight that a fixture asserting the
  argv tally intends to emit cannot be: it tests what the foreign CLI accepts
  and what its sandbox mounts read-only.
- Added automated `checkpoint` nodes to spec-build worklists. These
  dependency barriers run a declared deep-validation argv against the exact
  accumulated base, record a content-bound Git completion ref after a
  witnessed pass, and deliberately skip agents, publication, and merge. A red
  checkpoint leaves only its DAG descendants unready while independent
  frontiers continue; it never creates an operator pause. Every worklist node
  now declares an explicit `kind` of `implementation` or `checkpoint`.
- Added forge-native ad-hoc campaign containers. `tally campaign project`
  creates or maintains a master GitHub issue, native task sub-issues,
  dependency links, and merged-PR-derived checkboxes; `tally campaign arm`
  registers that issue without a Nix edit or deploy. A generic Home Manager
  poller now re-reads armed issue graphs into the shipped stateless reconciler,
  while `services.tally.campaigns` remains the recurring-work surface.
- Added `tally query run <flow-run-id>` as the compact operational view of a
  flow pass. Spec-build runs show reconciled campaign tasks as
  done/running/blocked/pending, live node elapsed time and remaining runtime
  budget, and failed stages with their retained `.err` path and bounded stderr
  tail. `--status <done|running|blocked|pending>` narrows that board to one
  state while the summary counts stay whole-run, so a 128-task worklist stays
  readable in a terminal.
- Added machine-authored spec-build failure steering and forge-derived
  quiescence. Failed task lanes now feed their stderr capture, gate outputs,
  exact brief, and bounded diff to a diagnosis agent, publish one redacted
  steering comment, retry once, and directly block only after a second marked
  diagnosis. Unrelated subtrees keep advancing; an incomplete empty frontier
  posts exactly one escalation with accumulated diagnoses, while failure-only
  passes now self-trigger the next fresh reconciliation.
- Added accept-time campaign gate preflights: every command gate declares a
  base-safe `preflightArgv` separately from its post-change `argv`. The exact
  probe executes once on the fetched base, with the same task environment and a
  bounded deadline, before the first agent dispatch; a witnessed
  `preflight-gate-<id>` records failure or timeout.
- Added campaign-scoped human task references (`crm/t07`) to flow provenance,
  admission and terminal receipts, lifecycle/journal records, query output,
  transient unit names, and capture filenames. Campaign diagnostics now retain
  the stable worklist ID alongside the durable task UUID across retries,
  recovery, and remote execution.
- Added declarative campaign constraint gates alongside command gates, starting
  with `forbidPaths` globs over each task's committed pull-request diff. A
  forbidden path now becomes a cheap, witnessed, fail-fast gate failure before
  publication or merge instead of a post-merge operator audit.
- Replaced spec-build's campaign-long serial runner with bounded stateless
  reconcile passes over marked merged pull requests. Campaigns now select a
  dependency-ready, conflict-disjoint frontier up to `maxParallel`, execute its
  task lanes concurrently, rebase and re-gate changed heads before serialized
  merges, and enqueue at most one next pass through an exact self-posted GitHub
  command; fresh mentions safely recover failed, zero-merge, interrupted, or
  redeployed passes. Parallel campaign flows explicitly reject a replayed
  flow-run identity and direct recovery to a fresh mention. Campaigns ship merge
  and mention triggers, not a periodic campaign timer. Non-empty task
  `conflictDomains` now also constrain the
  full committed path history at an early post-agent check, initial publication,
  and after rebase, so a transient or net-deleted under-declared path is rejected
  before its remote branch can move. Ownership comparisons are case-folded,
  parallel briefs cannot disable required domains with an empty array, receipts
  expose declared domains and owned paths, and underfilled ready frontiers name
  representative domain collisions.
- Added flow-node `approvalPolicy` and `sandboxPolicy` fields plus campaign
  `agentApprovalPolicy`/`agentSandboxPolicy` options. Spec-build implementation
  agents now default to Codex's writable `workspace-write` + `on-request`
  pairing, while adapters without named policy maps can opt out explicitly.
- Added `tally adapter smoke <name>` to execute one bounded, witnessed job
  through real admission, transient-unit launch, capture, and adapter scraping;
  failed smokes now print a bounded excerpt from the retained stderr capture.
- Added first-class Home Manager `services.tally.campaigns.<name>` support: one
  attrset now renders the generic witnessed `spec-build` flow, a scoped GitHub
  mention producer, campaign mutex and node pools, and the packaged policy
  driver. A fixture repository and live policy-bearing agent-adapter test prove
  per-task brief delivery, merge-before-next-prep ordering, fail-fast gates,
  and explicit replay continuation.
- Documented fixed-budget replay as the continuation mechanism for flow
  campaigns that exceed one 24-hour evaluation, and added a regression test
  proving that a budget-stopped run reuses its witnessed prefix, attaches to its
  live frontier, and completes identically to an uninterrupted run.
- Added a neutral `slot` pool resource for counted external or metered
  concurrency, so Codex and Claude Code subscription lanes can admit multiple
  holders without being mislabeled as local CPU capacity or serialized as a
  mutex.
- Replaced the `agency-nightly` example flow with the real overnight wave: a deterministic
  worklist node that witnesses the wave declared in the flow's own arguments (the worklist is the
  script plus its args — no external worklist source), per-task parallel `codex()` implementation
  in per-task git worktrees, cross-harness `claude()` review that finds but never certifies, and a
  settled deterministic culmination that opens pull requests and writes a morning report even on a
  partially failed wave. Ships a packaged deterministic driver (`agency-nightly-driver`), a
  documented example argument file, hermetic driver tests, and flow tests covering settle-mode
  failure routing and mid-wave restart reuse/attach under a pinned flow-run identity.
- Added offline `tally history compact --keep-days N`: drops lifecycle records older than the
  window, records the cut in the durable retention metadata (complete=false, truncation boundary,
  reason), refuses to run while a daemon owns the state directory, and never touches durable
  enqueue events, which remain recovery inputs. The lifecycle retention policy string is now
  `unbounded-until-compacted`, sequences survive compaction unrenumbered, and a history whose
  prefix is missing without a recorded boundary fails closed.
- Added an ignored-by-default capacity envelope soak (50k witness records, 50k lifecycle events,
  20k durable rows) asserting bounded startup verification passes, no per-query ledger
  re-verification, near-zero continuation-page IO, bounded steady-state RSS growth, and amortized
  O(record) change-log bytes per event.
- Added characterization tests pinning the durable change-log window across reopen and crash
  artifacts, pagination cursor stability and deterministic page boundaries under the response
  byte cap, and client wire framing at and over the protocol frame limit.
- Added retention pruners for the two on-disk sets that previously grew without bound: per-attempt
  capture archives under the state directory (max age 30 days, independent of the witness ledger —
  witness records do not pin archives) and consumed producer event files (`events/done` max age
  180 days; `events/rejected`, the adversarially drivable set, max age 30 days or max count 10,000,
  whichever is exceeded first, oldest pruned). They run inside the existing `tally gc` sweep under
  the GC-roots lock, with no second timer, and `tally gc` reports the new counts. All four horizons
  are configurable through `services.tally.retention`.
- Documented the ratified trust boundary: what the per-job capability token enforces, that
  demotion to operator class and same-UID environment access are by design rather than gaps, and
  that hardening presets rather than the token are the containment story.
- Added a daemon-minted per-job capability token, delivered to local jobs as `TALLY_JOB_TOKEN`,
  persisted by hash in the durable row so a running job keeps one identity across daemon restarts,
  and forwarded by the CLI as the `callerJobToken` enqueue field. Remote and SSH-executed jobs
  never receive it.
- Added a payload-hash normalization table, the flow-cannot-choose-a-model rule, and a
  redeploy-during-an-in-flight-run section to the submission and replay chapter.
- Added two cookbook recipes with executable examples — bounded fan-out over a witnessed worklist,
  and expressing domain failure as a validated envelope instead of a thrown error.
- Added an evaluation-time flow width check: a script whose explicit `meta.maxNodes` exceeds
  `services.tally.enqueue.fanoutCap` now fails the generation build instead of hitting the cap at
  run time.
- Added `dedupKey` and `disposition` to the job projection, `--flow-run` filters to `tally query
  log` and `tally query proof`, and per-node `node-submitted` and `node-terminal` lifecycle events
  to the flow runner's stream, so a claimed replay is verifiable rather than asserted.
- Added a typed per-flow `workloadMutex`, co-leased with `flow` for the runner process lifetime,
  with replay-behind-the-next-holder semantics and an admitted-parent manual invocation contract.
- Added operator chapters for configured mechanisms and declarative flows, including exact module
  contracts and deployment asymmetries.
- Added a hermetic, offline dependency-policy gate with a flake-pinned RustSec advisory database,
  a tree-derived license allowlist, crates.io-only sources, and duplicate-version warnings.
- Added private vulnerability-reporting policy, a supported threat model, and an end-to-end release
  and rollback runbook.
- Added this changelog and the contribution rule that keeps `[Unreleased]` current.
- Added randomized checks for bounded NDJSON framing, byte-exact request round trips, witness
  mutation detection, durable JSONL tail repair, and canonical pool-set and migration behavior.
- Added flake-native rustfmt, Clippy, and repository-wide Nix formatting checks.

### Fixed

- Fixed `tally adapter smoke --assert-commit`, which could never pass for an
  adapter with `hardening` set — the configuration the hardening guide's only
  worked example recommends for codex. The probe repository was created under
  the system temporary directory, and a hardened adapter's transient unit runs
  with `PrivateTmp=yes`, so the working directory did not exist inside the
  unit's namespace and systemd killed it before the adapter ran. The operator
  saw an empty capture and a `not-checked` probe, indistinguishable from "this
  adapter cannot commit". The probe now lives under `adapter-smoke/` in the
  state directory, is declared as the job's workspace the way a campaign
  implementation node reaches its worktree — which is what places it in the
  unit's `ReadWritePaths=` without weakening any hardening property — and takes
  a `--probe-root` override so it can run where implementation nodes actually
  do. Moving it off `$TMPDIR` also stops the probe from measuring a sandbox
  inside a directory codex treats as a default writable root, where a confining
  policy could pass a probe it should fail.
- Fixed the `launch.approvalPolicies` option example, which published
  `--ask-for-approval` — the exact argv `codex exec` rejects — as the only
  worked example in the generated option reference, so a consumer declaring a
  custom codex-family adapter by copying the documentation reproduced the
  original defect. The documentation build now refuses to publish that flag in
  any generated option page, closing the class rather than the one site.
- Fixed the shipped codex adapter's approval policies, which rendered
  `--ask-for-approval` — a top-level codex flag that `codex exec` rejects
  outright, so every campaign codex node died in three seconds, exit 2, before
  the model ran. All four named policies now render the exec-local
  `-c approval_policy="<name>"` override that the real binary accepts.
- Fixed the campaign agent defaults, which paired an approval policy nobody
  could grant with a sandbox that cannot commit. `agentSandboxPolicy` now
  defaults to `danger-full-access` and `agentApprovalPolicy` to `never`: under
  codex's `workspace-write` the repository's git metadata is read-only, so an
  implementation agent wrote every file correctly and then failed at
  `.git/index.lock` with nothing publishable, and an unattended node has nobody
  to grant an escalation. The already-deployed consumer configuration —
  `agentSandboxPolicy = "danger-full-access"` with `agentApprovalPolicy = null`
  — keeps working unchanged.
- Reconciled the campaign documentation with the shipped
  `tally-campaign-poll.timer`. The flows guide claimed outright that no
  periodic campaign timer exists, which was true only of module-declared
  campaigns; those continue through their own `/tally reconcile <name>`
  comment, while forge-native armed campaigns post no continuation comment and
  depend on the timer. The guide now names both mechanisms, states that
  campaigns are Home Manager only and that the NixOS module renders no campaign
  surface at all, and a check keeps the contradiction from returning.
- Stopped campaign passes from starving their own failure diagnosis. The
  per-lane node budget counted only the success path, but `maxNodes` counts
  cumulative rows, so a lane that failed at merge overran it on the diff,
  diagnosis, and steering nodes — the machine-steering write was rejected
  exactly when a failure needed it. Lanes are now budgeted at their worst case.
- Made the forge-native campaign poll schedulable. The timer's interval and its
  bound on one scan are now `services.tally.campaignPoll.interval` and
  `.timeout`, and `.enable` turns the poller off. The scan holds the registry
  lock exclusively across its forge round-trips, so the explicit timeout caps
  how long a wedged call can block an interactive `arm`, `disarm`, or `list`.
- Fixed the failure-path `taskRef` in the spec-build flow. Diff, diagnosis, and
  steering nodes derived their task reference from the campaign name rather
  than the campaign task identity, so under a forge-native container those
  nodes were invisible to the cross-run blocking filter.
- Stopped adapter-controlled terminal control from reaching an operator's TTY.
  Every human rendering of adapter text — `query run` and `query log` tables,
  failure stderr tails, and the `adapter smoke` capture excerpt — now passes
  through one shared filter that removes escape sequences, C0/C1 controls, and
  bidirectional overrides. Failure tails keep their leading whitespace, so
  stack traces and diffs stay readable.
- Removed a per-record scan from `query log`: node labels were resolved for
  every candidate lifecycle record before filtering, costing
  O(records x (witnesses + rows)) on the daemon thread even for a single-task
  query. Labels are now indexed once and applied only to records that survive
  the filter.
- Corrected three `query run` readings. A finished flow with no reconciled task
  table reports `complete` instead of `idle`; exit codes print whenever the
  lifecycle record carries one, not only after a witness merge; and a node past
  its runtime budget reports a negative remainder instead of saturating at zero.
  A queued node now reads `elapsed=not-started`, and an absent failure capture
  prints `capture: <not retained>` rather than being omitted.
- Merged a terminal witness into only the newest journal terminal for an
  execution, so a `preempted` followed by a `failed` no longer reports the same
  canonical verdict twice, and a second witness sharing one execution identity
  survives as its own record instead of being dropped by a map overwrite.
- Bound campaign worklists to the fetched remote-base blob and checkpoint
  receipts to one exact base revision. Immutable create-only receipts now prove
  dependency ancestry, reject forged or annotated targets, and are invalidated
  by either a pushed worklist edit or any later base commit instead of silently
  treating a point-in-time integration result as permanent.
- Made campaign sweeping daemon-liveness-backed instead of process-assumed:
  every run hash is bound to its flow-run identity, an older paused, queued, or
  running child defers the new pass before reconciliation, and legacy lanes
  without that proof leak safely. Rebase abandonment receipts now name the
  recoverable published head, post-rebase policy failures abandon it with the
  same exact lease, completed-sweep replay refusal is limited to `reused`, and
  the single continuation comment is retried and read-after-write verified.
- Narrowed public redaction so it stops destroying the reports it protects.
  Secret prefixes now match only at a token's start, so ordinary words such as
  `task-1`, `subtask-2` and `disk-1` survive; a bare lowercase git object id is
  no longer mistaken for a hex secret; and a marker such as `token` or `secret`
  hides a whole line only where it stands in key position, so a diagnosis about
  an auth-token bug is still readable. Real credentials are still caught, on a
  labelled line as well as bare. Failure stderr and machine steering hand-write
  the same rules in Rust and Python and now both assert against one committed
  vector at `test/fixtures/redaction/vectors.json`; the redaction identity is
  `conservative-v2`, and receipts written by the superseded redactor stay
  readable.
- Stopped a worklist edit between campaign passes from bricking the campaign.
  Machine receipts naming a task the worklist no longer has, and receipts left
  without the attempt that should precede them, are now witnessed reconciler
  warnings that drop the receipt instead of hard failures that also disabled
  escalation's own self-report.
- Separated campaign machinery faults from evidence that a task's work is
  wrong. A red gate, rejected ownership, a non-zero agent, and a red checkpoint
  command still spend one of a task's two steering attempts; preparing a lane,
  a lane exception, rebasing, publishing and merging now post a bounded,
  forge-counted retry receipt and the continuation instead. The retry budget is
  two per task, so a permanently broken lane still reaches escalation.
- Stopped campaign checkpoints from spending steering attempts on work they do
  not own. A checkpoint that runs red while unblocked, unrelated implementation
  work is still outstanding now defers instead of failing, and the reconciler
  considers a deferrable checkpoint last so it never displaces real work from a
  bounded frontier.
- Made a campaign pass post its continuation even when the diagnosis lane
  itself faulted, so one transient adapter failure no longer stops a campaign
  with neither steering nor a mention to resume from.
- Held campaign diagnosis nodes to the read-only obligation their brief states
  through the new `agentDiagnosisSandboxPolicy`, which defaults to `read-only`
  rather than inheriting the implementation node's writable sandbox.
- Completed campaign task-reference observability in `query trace` records and
  generations and in every `query standup` bucket; task-ref-qualified archived
  captures are now regression-tested through retry trace lookup and recovered
  stderr receipts. Core and flow also share one wire `TaskRef` type, preventing
  validator drift from becoming a flow-runner protocol failure.
- Made `forbidPaths` campaign gates history-scoped and case-insensitive, bound
  their witnessed result to the checked base and head, and re-evaluate them at
  publication so cleanup commits and stale green nodes cannot publish an
  unexamined artifact. Gate kinds and constraint deadlines are now explicit.
- Moved generated flow and campaign arguments out of runner argv and into the
  daemon's content-addressed structured-brief transport. Runners now read those
  arguments through `TALLY_BRIEF`, verify them against `TALLY_BRIEF_HASH`, and
  stay pinned to the configured tally package, keeping job queries,
  transient-unit status, and process argv bounded independently of campaign
  input size; manual flow invocations can also use an `--args-path` JSON file.
- Failed lifecycle records, `tally query log`, terminal waits, and flow-node
  failures now carry the final bounded 2 KiB of captured stderr. Raw adapter
  stderr is retained as `.adapter.err`, while the conventional `.err`
  diagnostic projection is materialized only for failed jobs, so routine
  adapter chatter is no longer a false failure signal. GitHub failure evidence
  is governed by separate default-off publication controls described below.
- Reconciled missing current-generation `.err` projections from failed
  witnesses during startup, serialized projection creation against retry and
  remote-capture replacement, classified substituted results as successful
  lifecycle events, and avoided a replacement character when a byte-bounded
  stderr tail starts inside a UTF-8 codepoint.
- Restricted startup GitHub completion replay to durable rows already in
  completed/deleted recovery states and to terminal verdicts, preventing
  unrelated pending rows or nonterminal witnesses from being replayed as
  completion mutations.
- Made producer briefs single-copy and retention-owned: producers now write
  directly into `<dataDir>/briefs`, admission and GC share a lock, and the
  existing retention horizon preserves live/recent job inputs while pruning
  orphaned, older-terminal, and legacy duplicate files under `stateDir`.
- Allowed a campaign to opt into self-posted operator-facing GitHub mentions with
  `services.tally.campaigns.<name>.allowSelfTriggered`, while preserving the
  loop-breaking `false` default on that broad mention producer.
- Made jobs without an explicit working directory execute from
  `workspace.worktreePath`, so flow-submitted agent nodes start inside their
  prepared worktrees across systemd and direct-spawn execution.
- Made regex adapter scrapes line-oriented by default, so documented
  newline-terminated `^TALLY_FINAL_MESSAGE=(.*)$` captures are attested and
  projected immediately after local completion without requiring a daemon
  restart.
- Refused daemon startup when its state directory is a symlink or another
  non-directory, with instructions to replace it with a real directory and
  move the state files, instead of reporting healthy and failing producer
  drains later.
- Reset an invalid or foreign-format `changes.jsonl` to an empty watch feed at
  daemon startup instead of reporting disposable, non-evidence state like
  corruption that needs operator intervention.
- Pinned the fleet gate's changelog decision to the audited SHA's status at script start instead of
  re-deciding it when the stage runs. A merge landing while the run waited for the runner lock or
  worked through the ladder moved the tip of main away from the audited commit, and an otherwise
  green audit of main then exited 1 claiming no open pull request contained the head SHA.
- Excluded the trailing record-framing newline from the single-byte mutation properties for the
  witness and attestation chains. Replacing that byte leaves every record identical and the chain
  legitimately valid, so the properties reported a false tamper miss on the seeds that selected it.

### Changed

- Campaigns now continue themselves through the events directory instead of a
  public `/tally reconcile <name>` comment. A pass that merged work, passed a
  checkpoint, or published machine steering writes one bounded JSON enqueue
  payload into `<stateDir>/events`; the 5s drain admits the next pass. Both
  classes take this path: a module-declared pass writes its own flow-run argv
  under a run identity derived from the pass it continues, and a forge-native
  pass writes the same `campaign poll --once` registry scan the timer runs, so
  the next pass still inherits the `campaign:<repo>:<number>:<revision>` dedup
  key. This removes GitHub API availability from the campaign's critical path,
  cuts merge-to-next-pass latency from the poll interval to the drain cadence,
  and stops publishing the machine's note-to-self. The human at-mention surface
  is unchanged.
- Removed `mkCampaignReconcileProducer` and the per-campaign
  `producers.campaign-<name>-reconcile` GitHub producer, and with it the
  rendered `reconcileCommand` campaign argument. One generic `events-dir`
  producer, `producers.campaign-continuation`, is installed once and
  unconditionally in its place, so arming a forge-native campaign still needs
  no Nix change. `tally-campaign-poll.timer` is unchanged and becomes the
  recovery path for a lost continuation event: the campaign pool is a
  capacity-1 mutex and the continuation payload carries a deterministic
  `dedupKey` under full submission, so a duplicate event or a race with the
  timer attaches to the pass already admitted rather than starting a second one.
- Removed the tracked `legacy-docs/` tree. Those 26 pre-book design and campaign
  records were entirely off the build path; `README.md` now points at them
  through a pinned-commit GitHub link, the same archival form
  `doc/src/architecture/lineage.md` already used. Git history keeps every byte.
- Added `.wrangler/` to `.gitignore` so a worktree carrying build and tool
  clutter (`result`, `result-*`, `__pycache__/`, `.wrangler/`) still reports a
  clean `git status`.
- Added the `TALLY_TEST_TIMEOUT_SCALE` test-harness knob. It multiplies the
  fixed wait budgets in the live flow suite so a loaded host can be given slack
  without editing tests or changing what they assert. Unset is byte-identical to
  the previous budgets; a value that is not a positive finite number panics
  rather than silently running unscaled. The variable is read from the test
  process environment, so it applies to a direct `cargo test` reproduce run and
  not to the sandboxed tests run by `nix flake check`.
- Removed the TaskChampion live projection in full. Nothing read the replica at
  query time, its distinctive features (sync, recurrence, reports) were compiled
  out, and every ordinary terminal completion issued a full O(N) replica rewrite
  — the pathology that grew one incident host's data store to 270 GiB from 6,981
  tasks. Deleted with it: the unbounded post-ack commit channel and its worker
  thread, the offline `tally view rebuild` verb (`tally view` no longer parses),
  the `taskchampion` and `rusqlite` dependencies together with their bundled
  SQLite chain, the stock-Taskwarrior compatibility test and the `slow-sqlite`
  scenario that both existed only to exercise the replica, and `taskwarrior3`
  from the package check inputs and the development shell. The durable store is
  untouched: flat JSON enqueue events, the hash-chained witness ledger, and
  recovery read exactly the same bytes as before, and query v1/v2 semantics are
  unchanged.
  **Upgrade note:** an existing `<data_dir>/taskdata/` directory and any
  `taskdata.pre-rebuild-*` archives become inert. Nothing reads or writes them,
  no retention lane sweeps them, and they still count against the data-store
  byte budget — deleting them by hand is what actually returns the space.
- Bumped `query storage`'s `schemaVersion` from 2 to 3 and removed its
  `taskchampion` section (`databaseBytes`, `walBytes`, `shmBytes`,
  `totalBytes`, `taskCount`, `operationHighWater`, `readError`) along with
  `growthPerCompletion.taskchampionBytes` and
  `growthPerCompletion.taskchampionOperations`. The fields are gone outright
  rather than emitted as null placeholders. The rest of the storage monitor —
  data/state directory budgets, the free-space floor, and hard-pressure intake
  refusal — is unchanged.
- Removed the projection-archive retention lane, which had nothing left to
  sweep: the `retention.projectionArchiveHorizon` NixOS/Home Manager option, the
  `--projection-archive-horizon` flag on `tally gc`, the corresponding
  `projectionArchiveHorizon` config key, and the `projectionArchivesExamined` /
  `projectionArchivesPruned` fields of the GC report. The option is removed
  outright, so a configuration that still sets it is now rejected.
- Corrected the release runbook's scenario ladder, which the TaskChampion delete
  had left naming `slow-sqlite` — a scenario the same change deleted. A releaser
  following `RELEASING.md` verbatim hit `usage: … {fleet-conformance|fanout-guardrail|pool-vanished/return}`
  and exit 2 at the gate step, and could not produce the scenario evidence the
  document requires them to attach to the release. `CONTRIBUTING.md` had been
  updated in that change; `RELEASING.md` had not.
- Changed `tally query log` to print one terse human line per lifecycle
  transition by default. `--json` retains structured fields; both human and
  JSON modes collapse journal/evidence/witness echoes, while `--provenance`
  restores the uncollapsed source stream.

- Hardened forge-native campaign admission: arming now binds the authenticated
  GitHub identity, allowed issue/comment actors, checkout repository, immutable
  registration identity, and canonical executable graph digest. Polling refuses
  executable revisions until explicit re-arm; agents receive filtered steering
  snapshots; PR/checkpoint completion is source-revision-bound; issue-native
  checkpoints remain typed end to end; completed masters close and are pruned;
  and `campaign disarm` removes a locked local registration. Ad-hoc workspaces
  now default outside tally's state/data budgets, and `forge: "local"` is an
  explicit test-only mode.
- Restored `postEvidence` to its original pass/reuse-only meaning. Operators
  may opt into one idempotent public comment per failed attempt with
  `postFailureEvidence`; retries therefore accumulate distinct failure
  receipts only under that explicit policy.
- Made failure-only `.err` files atomic UTF-8 diagnostic projections capped at
  2 KiB instead of full byte-for-byte duplicates of `.adapter.err`. The raw
  stream remains authoritative; coordinator attempt archives are pruned by the
  existing `captureArchiveHorizon` policy (30 days by default).

- Served the daemon's per-query row projections from Arc-shared snapshots rebuilt only after a
  mutation instead of deep-cloning every projection per query.
- Collapsed daemon startup to at most two full witness verifications and gave the daemon a
  verified in-memory witness view with per-task and per-dedup-key indexes. Queries, retry
  admission, dedup probes, and completed-job waits now verify only newly appended ledger bytes
  instead of re-reading and re-hashing the whole chain per operation; continuation pages for
  paginated queries are served from the page cache with zero witness reads, and the page cache
  enforces a 64MiB byte budget with byte-identical page boundaries.
- Made witness, attestation, and change-log appends O(1) in steady state. The witness and
  attestation ledgers cache their verified head and byte offset, verifying only bytes other
  writers appended since instead of rescanning the whole chain on every append; prefix tampering
  is now detected at startup, view rebuilds, and explicit verification rather than on every
  operation, while post-open suffix tampering is still caught at the next append or read. The
  change log keeps its in-memory window at exactly the retention limit but lets the durable file
  grow to at most twice that before one amortized rewrite drops it back, so at least the newest
  4096 changes remain durably available. Daemon startup now opens one shared attestation handle
  instead of re-verifying the chain for every hydration helper and recovery row.

- Derived job-originated enqueue identity from the daemon-minted capability token rather than from
  the client-supplied `callerJobId`, which is now accepted only when it names the same identity.
  The depth, fan-out, `noEnqueue`, and ancestry guardrails are consequently enforced rather than
  cooperative, and a request presenting a job token is refused the administrative and
  `__producer.*` method classes.
- Made `tally enqueue --dedup-key` use full submission semantics by default, with
  `--submission legacy` as the compatibility escape hatch.
- Run NixOS system-mode daemon, transient jobs, and witness emission as a dedicated configurable
  unprivileged user and group, migrating existing root-owned state during activation.
- Run NixOS system-mode drain and retention schedules as system timers, and reject Home
  Manager-only producers, flows, and usage meters during NixOS evaluation.
- Standardized pull-request merge evidence on the canonical worker-run local ladder transcript in
  the single-operator phase.
- Required each behavior-affecting pull request to update `[Unreleased]` unless it carries the
  `no-changelog` label.
- Single-sourced the declarative-flow node field contract across JavaScript validation, live-wire
  rendering, and canonical hashing.
- Excluded flows from windowed-consumption admission at configured check time; flow contention
  uses priorities while direct and producer enqueue retain consumption estimates.
- Removed the inert declarative-flow `budgetPool` option; its tombstone points to priorities and
  the typed process-scoped `workloadMutex` instead.
- Restricted system-indexed flake outputs to `x86_64-linux` and documented that supported platform.
- Accepted `--flow-run` and `--flow-run-id` interchangeably across `tally query` and
  `tally flow run`.
- Corrected the flow documentation where it diverged from the code: evidence absoluteness and the
  single-`exit` rule, a missing `pools` field's real error class, `drv()` unknown-field handling,
  the diversity interleave order, the `*-history-conflict` exit code, `RangeError` classification,
  computed banned-global access, and how a reached timer job is reported.
- Made the five worst flow authoring errors actionable: `unknown-spec-field` names its surface and
  lists the accepted fields, `duplicate-key` reports the first claim's ordinal and position,
  environment names separate invalid from reserved, every banned global names what to do instead,
  and a float in a whole-number field says so rather than naming a Rust type.

### Fixed

- Reject a `parallel()` thunk or `pipeline()` stage that returns anything but a promise, naming the
  branch index, so the brace mistake `() => { sh(...) }` fails the run instead of silently
  computing on `undefined`.
- Reject a flow-fixed sugar option such as `claude(prompt, { pools })` during `tally flow check` and
  the generation build rather than only on the first node at 2am.
- Detect canonical payload-hash drift on a flow's first admission instead of storing a mismatch
  that becomes an unrecoverable replay divergence on the next run.
- Made full-mode flow credential resolution symmetric between the client and daemon, including a
  hard error when `tally flow run` has no client configuration.
- Preserved inherited job identity and `noEnqueue` guardrails through CLI continuation, and made
  negative or signalled waited process outcomes return a nonzero CLI status.
- Preserved the original launcher-failure status and stderr when transient-unit reclamation cannot
  find or clean up the failed launch.

### Security

- Stopped raw private process stderr from crossing the GitHub mutation boundary
  through `postEvidence`. New `postFailureEvidence` and `postFailureStderr`
  controls default to false for generic producers and campaigns; explicitly
  published tails receive conservative secret redaction, and HTML-significant
  JSON characters are escaped so evidence cannot inject a completion marker.

- Disabled the executor's unhardened direct-process fallback by default; library consumers must
  opt into that compatibility path explicitly.
- Exposed opt-in hardening and scoped writable-path declarations through the Nix adapter library
  without changing the intentionally un-hardened default.
- Added the opt-in `production` adapter hardening bundle and narrowed `strict` and `production`
  transient jobs to execution-scoped state writes, with explicit per-adapter writable-path
  extensions for required agent state.
- Documented the single-trusted-Unix-user boundary, cooperative versus token-bound job identity,
  unsigned witness-chain limits, and why hardening presets are not a hostile-code sandbox.
- Replaced Boa 0.21.1 with the exact upstream `c39e6bf` migration commit that removes the
  unmaintained `paste` proc macro in favor of maintained `pastey`, and removed the
  `RUSTSEC-2024-0436` advisory suppression.

## [Pre-release history]

Development through baseline commit [`c6c304e`] was pre-release; see `git log c6c304e` for that
earlier history. No version tag is implied by this retroactive section.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html
[Unreleased]: https://github.com/mecattaf/tally.nix/compare/c6c304e...HEAD
[Pre-release history]: https://github.com/mecattaf/tally.nix/commits/c6c304e
[`c6c304e`]: https://github.com/mecattaf/tally.nix/commit/c6c304e
