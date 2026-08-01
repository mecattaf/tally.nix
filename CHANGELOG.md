# Changelog

All notable changes to tally.nix are recorded here. The format is based on
[Keep a Changelog], and the project intends to follow [Semantic Versioning] once version tags are
authorized.

## [Unreleased]

### Added

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
  tail.
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

- Changed `tally query log` to print one terse human line per lifecycle
  transition by default. `--json` retains structured fields; both human and
  JSON modes collapse journal/evidence/witness echoes, while `--provenance`
  restores the uncollapsed source stream.

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
