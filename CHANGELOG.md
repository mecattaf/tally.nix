# Changelog

All notable changes to tally.nix are recorded here. The format is based on
[Keep a Changelog], and the project intends to follow [Semantic Versioning] once version tags are
authorized.

## [Unreleased]

### Retired usage shape exemptions (#409)

- Removed `total-only-attempts` from declared-field grading and deleted the
  arithmetic that inferred a component-less subset from two aggregate
  coverage counts. Declared per-field coverage is now the sole completeness
  authority for mixed and uniform-drift runs.
- Declarationless legacy records now raise `declared-surface-unknown`. Their
  reported-shape is retained only for diagnosis: ambiguous legacy total-only
  attempts beside a component-shaped observation also raise
  `total-only-attempts`, without entering sums or declared-field thresholds.
- `attemptsReportedWithComponents` remains as a deprecated wire projection for
  older consumers, but is computed only from durable declarations and never
  drives completeness or caveats.
- The uniform-drift regression now covers one honest attempt beside two
  total-surviving drifted observations and pins three declarations against one
  exact component report, so shape-based exemption thresholds cannot return.

### Declaration-aware usage completeness (#408)

- Run and standup rollups now use each attempt's durable `declaredFields` as
  the only completeness denominator. Strict-subset, input-only, total-only,
  cost-only, and mixed-adapter runs are graded against what each adapter
  promised rather than against whichever shape happened to arrive.
- Coverage now publishes per-field declared, exact-reported, unreadable, and
  accounting-unavailable attempt counts. Missing declared token fields raise
  `partial-components`; declared total and cost gaps raise `partial-total` and
  `partial-cost`. Legacy declarationless records remain caveated and are never
  guessed into a contract.
- Sparse `declaredByField` and `reportedByField` projections now expose the
  public field census directly, including zero reports for a field that was
  declared but drifted away. `missingDeclaredFields` names every strict
  declared/reported gap directly in logical schema order.
- Fresh input includes cache write only where that field was declared, and
  adapter validation now requires `cacheReadTokens` beside the cache-inclusive
  input convention. `partial-fresh-input` identifies a partly reported
  formula; an entirely absent formula is already diagnosed by its missing
  declared component fields. The CLI diagnosis recipe therefore names only
  declared missing keys instead of accusing cost-only or cache-less adapters
  of token drift.

### Independent usage coverage denominator (#402)

- Run and standup rollups now derive expected attempts from each member's
  durable row counter, with the latest canonical witness attempt as the
  fallback only when row detail is absent. A missing independent ceiling is a
  typed caveat, never an assumed attempt or a dropped task.
- Coverage now publishes expected, attested, and missing attempt counts plus a
  bounded list of missing identities. The prior physical
  `attemptsObserved` count remains a compatibility projection and no longer
  drives completeness.
- The public count is `attemptsMissingAttestation`, its caveat is
  `attempts-missing-attestation`, and every rollup serializes `isComplete` so
  consumers do not have to reconstruct the completeness policy.
- Multiple leases for one logical attempt select the last verified ledger
  record and contribute once. Over-ceiling attestations are caveated and
  excluded rather than expanding the denominator or the usage sum.

### Exact resumed usage accounting (#403)

- Adapter contracts now declare `usageCounterScope`; the stock Codex preset is
  `session-cumulative` because the 2026-08-08 probe confirmed that
  `codex exec resume` rehydrates its thread counters. Resume lineage names an
  exact predecessor task, attempt, and lease across continuations, retries,
  pool returns, and restart recovery.
- Every completed scrape now records schema-1 `usageEvidence`, keeping the raw
  provider observation separate from fresh or checked-delta per-attempt
  accounting. Missing, legacy, mismatched, unreadable, and underflowing
  predecessors are typed unavailable states; cost deltas use exact decimal
  arithmetic.
- Rollups accept equivalent public checkpoint projections when they carry
  declarations and explicit accounting: either sibling accounting fields or
  schema-1 `derivation`/`contribution` evidence. Bound `fresh-zero` and `delta`
  checkpoints contribute exact per-attempt values; `baseline-missing` raises
  `cumulative-baseline-missing`. Raw cumulative usage alone remains
  unchargeable legacy evidence.
- Run rollups and the built-in meter consume accounted usage, never cumulative
  raw observations. The committed real Codex pair pins fresh 16,209, resumed
  delta 16,636, combined 32,845, and rejects the old 49,054 double charge;
  legacy raw-only records remain visible on job detail but are caveated and
  excluded from confident totals.

### Campaign child host locators (#442)

- Forge-native initial flow runners and continuation polls now extend one
  `CampaignHost` argv prefix containing the exact optional config locator and
  required socket locator, with both global flags before the subcommand.
- NixOS campaigns therefore keep using `/etc/tally/config.json` inside both
  daemon-launched children instead of letting the initial `flow run` fall back
  to a nonexistent or unrelated config in the service account's XDG home.
- Campaign poll observations now include the remote base and the driver's
  campaign-scoped state refs. A plain public poll therefore admits a fresh
  successor after a local merge or checkpoint even when the forge is unchanged,
  allowing the terminal digest and issue close to converge without depending
  on the daemon's continuation event.
- Campaign checkbox repairs now pass `gh issue edit --body-file` a private
  temporary file instead of the stdin sentinel `-`, so recorded forge adapters
  and the real GitHub CLI execute the same terminal mutation grammar.
- Forge-native completion summaries now follow the GitHub campaign thread even
  when `--allow-test-local-forge` selects local Git state for code merges, so
  closeout posts one digest-bound comment before closing the master issue. Its
  idempotence lookup uses the same paginated JSON grammar as every other issue
  comment read, keeping record/replay forge adapters on one invocation surface.
- The system-socket VM now runs an initial and continued fake-forge campaign
  against a sentinel pool, a 20 MiB frame bound, and a real-file-only forge
  recorder, then mutation-replays both real argvs without their `--config` pair
  and requires failure before a flow node is admitted.

### Preserved conflict-domain states (#439)

- Project conversion, canonical Rust admission, the packaged driver, and both
  flow task arms now preserve an omitted serial-task `conflictDomains` key
  instead of materializing `[]`. Parallel campaigns still require a present,
  non-empty declaration before dispatch.
- The three states now retain their distinct enforcement meanings end to end:
  omitted uses the ownership receipt's exact `ownedPaths` after ownership ran,
  explicit `[]` denies every changed path, and a non-empty array permits only
  its declared prefixes. Omission on a failed-agent path remains unjudgeable
  and aborts because ownership produced no certified fallback.
- Ownership, publication, rebase, narration, diagnosis, and tree-delta payloads
  omit the key when the task omitted it. Schemas keep arrays strict when the
  key is present, and receipts expose presence rather than making readers infer
  it from an empty value.

### Single campaign manifest grammar (#446)

- Campaign admission now closes both gate variants, rejects trailing-slash
  forbid patterns, bounds agent models and steward environment values, and
  validates steward argv and scalar fields in the shared Rust contract.
- `finalMessagePattern: null` is rejected while omission still receives the
  canonical default. Accepted patterns use a bounded Rust/Python-portable
  subset, forbid engine-specific groups, flags, look-around, and
  backreferences, and contain exactly one capture group.
- The packaged Python driver no longer parses or defaults raw manifests. It
  exactly decodes `CanonicalCampaignGraphV1`, while the flow input schema now
  requires every normalized canonical member. A mutation corpus pins accepted
  canonical bytes and rejects near-valid input before driver dispatch.

### Canonical campaign admission (#444)

- Added `tally_core::campaign_contract` as the single owner of campaign
  manifest types, defaults, validation, canonical JSON, and executable graph
  digests. Arm and project now filesystem-canonicalize the checkout and require
  an existing Git worktree before that path can enter the manifest or digest.
- A minimal explicit steward now canonicalizes to an empty environment, the
  `^TALLY_FINAL_MESSAGE=(.*)$` capture, and a 120-second runtime while an
  omitted steward remains absent.
- Every forge-native dispatch carries the complete normalized
  `CanonicalCampaignGraphV1`. The packaged driver consumes those admitted
  manifest/task bytes and only refetches mutable forge state, eliminating its
  independent path resolution and manifest-defaulting digest contract.
- The dispatch also retains #433's normalized `armedManifest`. Compatibility
  briefs may omit `campaignGraph`; the driver exact-decodes that manifest,
  restores only immutable native task content, and requires the resulting
  envelope to reproduce `worklist.graphDigest` without applying defaults.

### Campaign executable ownership (#448)

- Armed flow and driver paths now have registry-owned lifetime. Exact Nix-store
  subfiles remain in frozen schema-2 authority while two stable indirect roots
  retain their containing outputs; non-store overrides are copied to hashed,
  owner-immutable snapshots that preserve executable bits.
- Asset generations are derived from `(registrationId, armSerial)` and publish
  before authority. Re-arm removes the prior serial only after the new
  authority rename; disarm and closed-issue pruning remove authority first.
  Registry entry reconciles missing roots plus safe pre/post-publication
  leftovers and reports a typed error when an old unrooted asset is already
  gone.
- Both campaign-poll module wrappers now include Nix in their runtime PATH, so
  scheduled reconciliation and interactive CLI arms use the same lifecycle.

### Campaign registry rollback compatibility (#447)

- Campaign schema-2 authority is frozen to the closed field set understood by
  the preceding tally generation. An explicit host-local `projectionWaitMs`
  tuning now lives in a separately versioned `campaigns/host-tuning/` sidecar;
  an absent sidecar resolves to the historical 10-second default. Both the
  default and an explicit override produce authority bytes the literal N-1
  decoder accepts.
- Current readers narrowly migrate the already-shipped polluted schema-2 shape
  under the registry lock, reject every other unknown authority member, and
  preserve the sidecar across an N-1 poll/rewrite. During rollback N-1 uses its
  historical 10-second wait; rolling forward restores the explicit override
  without changing campaign authority, observations, digest, or asset paths.
- Registration paths, validation, locking, atomic publication, and migration
  now live in the shared versioned `tally_core::campaign_registry` lifecycle.

### Honest default-model Codex recovery (#443)

- The stock Codex resume template no longer requires a scraped `model` or
  synthesizes `--model` from one. Real default-model Codex JSONL captures do
  not state a model, so those jobs now recover and continue with no model flag,
  allowing Codex to select its default again.
- An explicitly authorized per-job model remains durable in `adapterOptions`
  and is inserted exactly once through the same typed `launch.model` override
  on launch and resume. Codex declares the resume insertion point so that
  provider options precede the positional thread ID.
- The preset and daemon proofs now use the committed real Codex usage capture,
  including its session, final message, five-field usage object, and absent
  model. The usage capture also publishes its provider-facing
  `counterScope = session-cumulative` declaration and validation keeps that
  declaration aligned with adapter accounting.

### Pi option-like workload heads (#445)

- `AdapterLaunchConfig` now carries the typed boolean
  `rejectOptionLikeWorkloadHead`, false by default for opaque workload argv
  and true for harnesses that cannot separate provider flags from workload.
- The stock `pi` preset declares that refusal. Fresh launches and resumes now
  fail before admission or execution with a typed pre-launch RPC error whose
  structured detail names reason `option-like-workload-head`, workload index
  0, and the offending argument such as `--version` or `-p`, instead of
  letting pi consume it as a flag and potentially exit 0 without doing the
  job.
- Authorized pre-prompt options remain part of the adapter prefix. Pi still
  has no trailing `--`, because its CLI rejects that separator.

### Eval-manifest zero-covered outcome (#426)

- A checked coverage manifest now needs at least one declared surface whose
  explicit status is `covered` to return success. If every declaration is
  accounted for but all are `reused` or `failed`,
  `test/eval_manifest_check.py` exits 4 and emits the stable
  `coverage=zero-covered` and `verification=none` summary tokens. One covered
  declaration still returns 0 even when another checked declaration failed,
  and emits `verification=present`. Covered entries outside the declared
  `expected` surface do not change that verification token.
- Multi-file precedence is invalid (1), unchecked (3), zero-covered (4), then
  success (0), independent of argument order; usage remains the immediate exit
  2. No close-out consumer was added.

### Filtered query views and archive state (#415)

- `query jobs --flow-run ID` is now an explicit lookup, matching `query run
  ID`: archived members remain visible with `archived: true`. Broad jobs
  queries still hide archived runs by default and include them with
  `--archived`; the CLI rejects either `--archived` or `--no-archived` beside
  `--flow-run` because those broad-view controls cannot change an explicit
  lookup.
- Singular `query run ID --json` now carries `items`, the exact durable
  member-identity list, so a row-less reused or attached member remains visible
  by `taskUuid` even when no reconciled campaign task table exists.
  An absent reconciliation board no longer serializes as `tasks: []`, which
  previously shadowed that member list for tasks-preferring consumers.
- After `query standup` hides archived task entries, `reused` and
  `canonicalGpuSeconds` are recomputed from the retained task UUIDs and the
  canonical witness records. Reuse follows the latest terminal
  `laborClass: reused`; GPU seconds use the existing canonical contribution
  predicate across qualifying attempts. `--archived` retains the whole
  window and its whole-window aggregates. Hidden task and run counts, per-run
  usage rows, and `usageBasis` conservation are unchanged.
- The query protocol is now **5** (schema remains 1) because these aggregate
  and explicit-lookup meanings replace the documented protocol-4 contract.

### Campaign git-ai execution and terminal diagnostics (#441)

- Campaign nodes may now carry the full closed git-ai correlation set of at
  most seven attributes: `taskUuid`, `attempt`, `leaseEpoch`, `adapter`,
  `flowRunId`, `nodeOrdinal`, and `taskRef`. The daemon had emitted `taskRef`
  since #265 while executor validation still admitted only six attributes, so
  every task-correlated campaign node was rejected before launch.
- Executor request validation failures now produce a canonical structured
  `error` with code `executor-validation-failed` on the terminal result and
  hash-covered witness. Restart reconstruction and `query.run` project that
  same object, and the human run view prints it even when pre-launch rejection
  means there is no stderr capture.
- A flow terminal result that already carries this error skips the advisory
  `finalMessage` projection wait entirely. The flow host preserves its code and
  validation message instead of spending the wait budget and relabelling the
  failure `result-projection-timeout` or `result-schema-mismatch`.

### Adapters and observability (#425, #434, #419)

Three surfaces the operator reads to decide what is broken: a resume that
must land in the directory its session lives in, a preflight tool that must
not report failure for work that passed, and a test suite whose reds must
mean something.

#### #425 — the cross-cwd resume invariant is enforced rather than documented

A harness that resolves a session by the directory it was launched in cannot
reach that session from anywhere else. For `pi` the failure is soft: exit 0,
`Session found in different project` on stdout, an interactive prompt on
stderr, no work done — which in a headless pipeline reads as a successful
attempt. No adapter argv can assert the invariant (pi exposes no cwd flag and
a `--session-dir` pin does not bypass the filter), so enforcement is now
Rust-side.

- New adapter declaration `resumeRequiresLaunchCwd` (Nix option and
  `AdapterConfig` field, default `false`). The `pi` preset declares it on
  reproduced evidence; `codex` and `claude-code` do not, because codex
  re-presents the directory in its own resume argv and claude-code has not
  been measured here. A `false` says "unmeasured", never "safe".
- `RowSeed` gained `session_cwd: Option<RecordedLaunchCwd>`, recorded beside
  `session_ref` at every seam that writes the pointer from a scrape (one in
  `daemon/completion.rs`, four in `daemon/startup.rs`, one at the retired-row
  seam a continuation reads the pointer back through). It is `#[serde(skip)]` —
  transport only, no durable-format change, no row-version move — because it is
  exactly as durable as the pointer it qualifies: startup re-derives both from
  the retained captures and the durable row.
- **A row that declared no working directory records that fact rather than
  recording nothing.** `RecordedLaunchCwd::ServiceManagerDefault` and an absent
  record are different states: the first says both attempts run wherever the
  service manager put the daemon, which is one directory, so the continuation
  is admitted; only the second is refused. Collapsing them into a single `None`
  would have permanently blocked continuations for every `pi` job enqueued
  without `--cwd`.
- `queue.continue` now refuses a continuation whose working directory is not
  the recorded launch directory, naming **both** directories, and refuses
  fail-closed when nothing recorded where the session was launched. Each
  refusal names the fact it actually found. Directory equality resolves before
  it compares, matching pi's own
  `sessionCwdMatches(session.cwd, resolvedCwd)`.
- `queue.retry` and recovery are deliberately not guarded: both re-render one
  row's own resume against that row's own cwd and cannot move it.

#### #434 — RPC reads stay honest during a daemon stall

`adapter smoke` reported **failure** for two smokes whose daemon-side verdicts
were exit 0 and witness-emitted PASS, because their `query.job` read timed out
during a stall (#431). A false negative from the estate's preflight tool costs
diagnosis time and poisons the operator's model of what is broken.

- `adapter smoke`'s verdict is now three-valued on a `verdictState` field:
  `PASS` (exit 0), `FAIL` (exit 1), `VERDICT-UNAVAILABLE` (exit **5**). A
  timed-out result read is never rendered as adapter failure, the diagnostic
  object is still printed in that state, and a retained commit probe is kept
  rather than judged.
- The smoke's result read is now bounded by `--rpc-timeout-sec` /
  `TALLY_RPC_TIMEOUT_SEC` (default 60) instead of a private 10-second constant
  no flag could reach; the value used is echoed on `rpcTimeoutSec`. The
  10-second capture-*projection* window remains its own separate bound, because
  "not projected yet" and "not answered at all" are different failures.
- `tally query run` gained a durable-state view: automatic on RPC timeout, and
  available outright as `--durable` (with `--state-dir`/`--data-dir`). It
  reconstructs the run from durable enqueue events, the verified witness
  ledger, the lifecycle history, durable membership, the advisory attestation
  ledger, and the retained capture tree — reconciled task table, terminal
  verdicts, usage rollup, and failure capture pointers, with no live RPC. It is
  labelled in both renderings (`view: "durable-state"`, `live: false`, plus
  caveats; the live path now states `view: "live"`), shows no in-flight state,
  and is strictly read-only: it never creates, locks, or repairs a durable
  store. That last claim is asserted over the whole state and data tree before
  and after a read, so it covers stores this view later learns to read; it also
  renders where the operator can read the daemon's data and not write it, which
  is the deployment the automatic fallback exists for.

#### #419 — a fifth flake-population member, and a way to keep counting

The population is **not** declared closed. Four members were already fixed on
main (b4fa724); a wave measured on this branch surfaced a fifth, with an
unrelated mechanism, which is the pattern that keeps this issue open.

- `daemon::tests::watchdog_keepalive_pings_while_a_dispatch_arm_{awaits,blocks_the_runtime_thread}`
  asserted the gaps between *observation* instants of the daemon's notify
  datagrams. Datagrams queue in the socket buffer, so a collector thread
  descheduled past one watchdog period reads a burst with one large gap in
  front of it and reddens a daemon that pinged perfectly. The assertion is now
  **counted, not timed**: the daemon must emit at least one keepalive per
  service period of the stall it was held through. The collector's wall-clock
  deadline became a liveness backstop rather than a measurement bound, for the
  same reason.
- `daemon::tests::storage_timer_samples_once_per_configured_interval` — a
  sixth member, found by the post-fix wave — slept 1,150 ms and asserted a
  1 s timer plus a blocking filesystem walk had landed, i.e. 150 ms of slack
  on a host that may run a hundred test threads. Its two halves have opposite
  relationships with load, so they are now asserted separately: the lower
  bound (no sample before the interval elapses) is asserted, because load can
  only delay a timer; the upper bound is waited for, with a liveness backstop
  that separates a suppressed tick from a late one.
- `test/flake-probe.sh` measures the population's rate: N concurrent full
  suites of one prebuilt test binary for a wall-clock budget, reporting runs,
  failures, and the full output of every failing run — the panic text included,
  because the expensive part of a wave is catching a failure, not counting it,
  and a name alone costs the next wave a re-reproduction. The load condition is
  the concurrent suites themselves, never a spinner. Documented in
  `CONTRIBUTING.md` so successive waves are comparable.
- **The bound, pooled across the waves run at `e12ce64`: 1 failure in 605 runs
  at three or more concurrent suites (~0.17%)** — the lane's 244 post-fix runs
  plus an independent 361. The one failure was the fifth member again, with the
  assertion uncaptured, so the residual is non-zero and the mechanism of that
  observation is unidentified. ~0.17% is under the historical ~0.74% and is not
  zero; the population stays open, and this is a bound, not a repair claim.
  A later eval wave on the shipped head pooled it up to 3/761 (~0.39%) and named
  four further wall-clock-deadline members — the running record is on #419.
### Daemon under load (#431, #428, #420)

One mechanism, three surfaces: work that scales with the durable corpus,
executed where it can starve everything else. Measured live on the
coordinator on 2026-08-06/07 at ~25–30k durable rows.

#### #431 — dispatch loop stays live at estate scale; query service moves off the dispatch thread

The daemon's runtime is a single thread: the dispatch `select!`, every RPC
connection, and every fresh query shared it. Two query builders were also
quadratic in the corpus — `query.jobs`/`query.job` re-scanned the whole
lifecycle history and witness ledger once per task anchor, and decorating a
collection resolved trace lanes with a whole-table scan per item. At ~30k
durable rows one such call held the select loop for minutes; a synthetic
30k-row corpus reproduced the estate's 60–183 s deaf windows as a
never-returning `query.jobs` (did not finish in 9.5 minutes, debug build)
while `tick_leases` stayed at microseconds.

- `query_jobs`/`query_job` now group history and witness records by task in
  one pass instead of filtering the whole corpus per anchor; trace-lane
  decoration in `query.jobs` resolves lanes through a per-anchor index
  (`anchor_trace_availability`) instead of the per-item whole-table scan.
- Fresh-query construction, response serialization, and page-snapshot
  splitting/sizing (`pagination::prepare_snapshot`) run on the blocking pool
  via `spawn_blocking`, over immutable snapshots taken under the context
  lock. What remains on the dispatch thread is amortized O(live jobs) per
  query, plus one O(corpus) snapshot-cache rebuild per mutation on the first
  query that follows it. The lifecycle store hands out a
  cached `Arc` snapshot (`LifecycleStore::shared_snapshot`), rebuilt at most
  once per mutation instead of deep-cloned per query; the flow lineage and
  membership caches moved from `Rc` to `Arc` so the blocking pool can read
  them.
- Reads that race a scheduling mutation answer from their frozen snapshot:
  the mutation is entirely invisible to that answer or entirely visible to a
  later one, never partially visible (tested). Admission keeps making
  progress while corpus-scale queries are in flight (tested). The
  self-reported dispatch-loop absence line and the watchdog
  keepalive/withhold behaviour are untouched — the instrument that found
  this defect still tells the truth.
- Acceptance is estate-scale and in-tree: a generated 30,000-row durable
  corpus (plus 40 live rows recovery re-presents and runs) under a
  continuous `query.jobs`/`query.job`/`query.status` storm with admissions
  landing mid-storm. The dispatch loop's maximum absence, measured at its
  own lease-tick boundaries, stays under a 5 s bound (healthy cadence is
  100 ms; the unfixed loop fails by minutes), and a `query.job` issued
  mid-storm answers inside the estate's 10 s client deadline.

#### #428 — the unit-facts startup phase renews its budget from inside the loop

`collect_local_unit_facts` probes the executor once per durable event row
that is not canonically terminal (and every local row unconditionally), so
the unit-facts phase is O(event corpus) — ~90–95 s at the coordinator's
~25k rows, exactly astride its single 90 s `EXTEND_TIMEOUT_USEC=` budget,
which put the 2026-08-06 switch into a restart loop.

- The loop now reports progress once per visited row, and the daemon turns
  those callbacks into time-throttled `EXTEND_TIMEOUT_USEC=` renewals (every
  10 s of progress, `STATUS=starting: unit-facts (k/N rows)`), so the 90 s
  budget bounds progress stalls rather than the phase's total cost: a daemon
  still visiting rows keeps starting; one wedged on a single probe dies on
  the same clock. A fast startup sends nothing extra.
- Tested at both ends and mutation-proved: the loop reports exactly one
  progress callback per durable event row (skipped canonically-terminal
  remote rows included, still unprobed), and the renewal datagrams flow
  through the notify socket from inside the loop — deleting the in-loop
  callback turns both tests red.
- The coordinator's interim dotfiles override
  (`TimeoutStartSec = mkForce "10min"`) becomes revertible once this lands;
  that revert is the operator's, not this change's.

#### #420 — two residues of the context.jobs prune

Both arrived with #395, which retires terminal jobs out of `context.jobs`;
neither changed behaviour today, both were unbounded-or-untrue surfaces.

- `unreachable_paused_jobs` reclaims uuids whose job is absent from the
  live map: a pool-loss-paused job that then completed or was cancelled used
  to pin its uuid in the set for the daemon's lifetime, because the GC read
  only the map that no longer retains terminal jobs. A retired job can never
  be resumed, so any pool-return sweep of the set now drops such uuids.
  Mutation-proved: reverting the reclaim arm turns the test red.
- `cancel`'s already-terminal answer derives `"was"` from the query
  fact that admitted the retired job instead of asserting `"completed"`:
  a row recovered as `Deleted` (latest witness verdict `cancelled`)
  answers `"deleted-cache"`, the same label its query projection uses.
  Mutation-proved: fabricating the constant back turns the test red.

### Campaign mechanism (#429, #432, #433, #424)

The spec-build campaign path surfaced by the first real ad-hoc campaign
(dotfiles#163): the arm CLI and the packaged driver disagree on the campaign
agent schema, a congested daemon converts a late advisory projection into node
death, the digest-mismatch receipt withholds the evidence needed to act on it,
and a failed agent pass launders its stray write into the next baseline.

#### #429 — the arm CLI and the packaged driver now agree on the campaign agent schema

The Rust CLI's `CampaignAgent` was a 7-field `deny_unknown_fields` struct while
the packaged driver's `forge_manifest` unconditionally normalized an 8th field
(`diagnosisSandboxPolicy`, defaulted `"read-only"`) into the canonical agent it
hashes. No manifest could make the two digests agree, so every forge-native arm
failed reconcile with "live issue executable graph does not match the armed
digest."

- The field genuinely governs diagnosis-node launch behaviour that ad-hoc
  campaigns reach: for a forge-native campaign the effective agent is the
  driver's normalized one, and `spec-build.js` passes
  `effective.agent.diagnosisSandboxPolicy` to the diagnosis node's sandbox
  policy. So the field is restored to `CampaignAgent` (option 2) rather than
  deleted, both halves now carry it, and the CLI default (`read-only`) matches
  the driver's normalization byte-for-byte.
- Added a schema-parity regression test
  (`graph_digest_is_byte_identical_between_the_cli_and_the_packaged_driver`)
  that computes the graph digest through the Rust `sha256_json` path and
  through the packaged `spec_build_driver.py` `canonical_sha256` path (run
  under `python3` as a subprocess, the packaged file, not a copy of its logic)
  and asserts byte equality — so version skew inside a pin fails in CI instead
  of at first arm. It runs two manifests: one carrying every optional field
  explicitly, and one carrying only the required ones so each half fills the
  rest from its own default. Deleting the driver's single
  `diagnosisSandboxPolicy` line makes the test fail (mutation-proven).
- The defaults fixture found a second skew of the same class: the driver
  validated `manifest.repository` with `repo_config`, which requires
  `baseBranch`, `remote` and `forge`, while the arm CLI's `CampaignRepository`
  defaults all three. A manifest omitting any of them armed cleanly and then
  died at reconcile inside the driver. `forge_manifest` now fills the arm
  CLI's exact defaults before validating, so both halves normalize the same
  manifest to the same canonical value.

#### #432 — a congested daemon no longer converts a late advisory projection into node death

After a driver node completed, the flow host polled `query.job` for the
finalMessage projection for at most a compile-time 10 s and then failed the node
`result-schema-mismatch` — killing work whose exit evidence had already passed,
because the daemon's dispatch loop was briefly stalled. The capture is advisory
by declaration; a projection that never arrived is daemon congestion, not a
schema violation.

- A terminal node whose exit evidence passed but whose advisory projection did
  not arrive inside the window is now classified `retryable-projection`
  (bounded exponential-backoff retries inside a configurable wait, then a
  receipt naming congestion: "projection unavailable within N ms; daemon
  congested?") and is never rewritten into `result-schema-mismatch`, both in
  the live client and in the engine host. A node whose exit evidence failed
  keeps the pre-existing `result-projection-timeout` behaviour.
- The projection wait is configurable, default 10 s. The seam that reaches a
  campaign is `tally campaign arm --projection-wait-ms MILLISECONDS`: the value
  is recorded in the registration and put on the argv of every `tally flow run`
  the campaign dispatches, including the ones `campaign poll` dispatches later.
  A campaign pass runs as a daemon-launched transient unit whose environment is
  an explicit `--setenv` list, so an environment-only knob would never have
  reached it. A `flow run` launched by hand takes
  `--result-projection-wait-ms`, or `TALLY_RESULT_PROJECTION_TIMEOUT_MS` with
  the flag winning. The knob stays out of the digest-bearing manifest, so
  widening a host's wait is not a change to what was approved. Documented in
  `doc/src/flows/campaigns.md`.
- Tests: one stall longer than the window is bounded and classified
  `retryable-projection` under a narrow window and completes the node under a
  widened one (the pass survives, on the projection rather than on exit
  evidence alone); a failed node keeps `result-projection-timeout`; the engine
  propagates `retryable-projection` instead of `result-schema-mismatch` on both
  the thrown and the settled path; the flag/environment precedence is pinned;
  and a registration written before the knob existed still loads. Restoring
  the fatal classification, the engine rewrite, or removing the retry loop
  makes the respective test fail (mutation-proven).
- The registration→argv delivery is pinned, not just the recording. The
  dispatched pass's argv is built by `dispatch_flow_argv` (split out for the
  same reason `continuation_argv` is), and
  `a_recorded_projection_wait_reaches_the_dispatched_pass_argv` asserts that
  `Some(n)` yields `--result-projection-wait-ms n` spelled exactly as
  `FlowRunArgs` parses it, and that `None` yields the pre-#432 argv element for
  element — that argv is hashed into the enqueue payload, so a stray element
  would move every existing campaign's payload identity. Deleting the push, or
  making it unconditional, each makes it fail (mutation-proven). The arm-side
  `--projection-wait-ms 0` refusal is pinned by
  `a_zero_projection_wait_is_refused_at_arm`.

#### #433 — the reconcile digest-mismatch receipt prints both digests and the first divergent path

On reconcile digest mismatch the operator saw only "live issue executable graph
does not match the armed digest; inspect it and explicitly re-arm" — no digests,
no diff. Four arms of dotfiles#163 were burned on plausible-but-wrong theories
before source-diving both canonicalizations found #429; the first divergent
canonical path would have named the defect instantly.

- The receipt now prints BOTH digests (armed and live-computed) in the
  `sha256:` form the arm CLI uses, and the first divergent canonical path from
  a canonical-key-order walk of the armed manifest against the live normalized
  one (e.g. `manifest.agent.diagnosisSandboxPolicy: absent-in-armed /
  present-in-live`).
- What the walk publishes, stated exactly: a path plus a shape —
  absent-in-armed/present-in-live, a JSON type name, an array length, or the
  bare fact that a scalar differs. Never a value. A path segment is a manifest
  key name or an array index, so a key an operator chose (a gate id, a task id,
  a steward environment variable's name) can appear; what is stored under it
  cannot. Task titles and bodies live outside the manifest and the walk is only
  ever handed the two manifests, so operator prose never reaches it.
- The arm CLI carries its canonical manifest in the pass brief as
  `armedManifest` (evidence for the receipt; it is never part of the executable
  graph digest), and `spec-build.js` forwards it to the reconcile node. A
  campaign armed before this existed carries none: the brief then omits the key
  entirely and the receipt says the path is unavailable rather than inventing
  one from the live side alone.
- The gate's verdict is unchanged: it still refuses and tells the operator to
  inspect and re-arm; this only stops the receipt starving them of evidence.
- Tests, through the real `issue_graph_worklist` refusal path: two manifests
  differing in exactly one nested key assert the receipt names that path and
  both digests and withholds the value; a divergence that is only a task body
  asserts the receipt says the manifest matched and does not republish the
  body; an absent `armedManifest` asserts the unavailable wording. Dropping the
  path computation makes them fail (mutation-proven).

#### #424 — the tree-delta gate now runs on a pass whose agent failed, and no baseline is overwritten unjudged

An agent node that did not pass returned at stage `"agent"`, before `ownership`
and before the `treeDelta` node. The next pass's `prep` then re-fingerprinted
the worktree unconditionally, taking a baseline that already contained the
previous pass's stray write — so an uncommitted out-of-allowlist write made by
a failing agent could never be seen by any gate again. A failing agent is the
single most likely context for a rogue write and it was the one context the
gate was silent in.

- **A baseline is never overwritten unjudged.** `action_tree_delta` clears the
  pre-agent fingerprint the instant it reads it, pass or fail, so a fingerprint
  still on disk at `prep` time means the pass it belongs to was never judged.
  `snapshot_before_agent` now preserves it in that case and rotates only when
  the previous pass was judged. The next gate to run therefore judges the whole
  span since the last judged baseline.
- **A pass whose agent node failed still runs the gate**, in place of
  `ownership`, with `ownershipRan: false`. Only a declared allowlist can govern
  there: `ownership` never ran, so no certified `ownedPaths` exist to fall back
  to.
- **No allowlist, no pass.** If ownership never ran and the task declares no
  `conflictDomains`, the gate refuses with a receipt naming exactly why and
  leaves the baseline in place, so the writes it could not judge stay judgeable
  once an allowlist exists. An explicitly empty `conflictDomains: []` is a
  declaration, not an absence, and still judges (any delta is a breach).
- #439 subsequently made both implementation schema arms preserve an omitted
  serial-task key. The failed-agent refusal is therefore a reachable,
  fail-closed campaign path, while a passing agent reaches the exact
  `ownedPaths` fallback after ownership. Flow tests pin both arms and assert
  key presence through the gate and diagnosis payloads.
- The refusal is priced as a gate verdict (`failureClass` → `"ungated"`), never
  as the agent's work being wrong: it spends none of the task's two steering
  attempts. It aborts the lane through the same both-receipts-at-once path a
  breach takes, but under its own sentence — the #386 breach sentence claims an
  out-of-allowlist write was found, and a gate that could not look has
  established no such thing. `action_steer` takes an `abortReason` and composes
  the matching label; absent keeps the #386 breach wording exactly.
- The `treeDelta` result gains `ownershipRan`, so a reader of a witnessed
  receipt can tell which of the gate's two call sites produced the verdict, and
  can see that a failed pass was in fact judged.
- The worst-case flow-node budget is unchanged: the agent-failure lane runs
  prep, agent, treeDelta, diff, diagnosis, steer and cleanup, well inside the
  11-per-lane allowance, and the merge-failure lane that sets the worst case
  already counted `treeDelta`. `campaignMaxNodes`/`max_flow_nodes` do not move.
- Tests: the eval's reproduction shape is caught directly at pass 1; a pass that
  ends unjudged has its baseline preserved and pass 2's gate still sees the
  stray write (restoring the unconditional re-snapshot makes that test red —
  mutation-proven); a judged baseline still rotates; the refusal is loud, names
  why, and does not claim a breach; the `ungated` class is pinned in the
  executable `spec-build.js` realm; and the posted receipt for an ungated abort
  never carries the breach sentence.
- The flow-side wiring itself is bound end to end.
  `crates/tally-flow/tests/spec_build_failed_agent_gate.rs` runs the real
  `spec-build.js` against a scripted client with the agent node failed and
  asserts the `tree-delta-<task>` node is dispatched, after the agent node,
  carrying `ownershipRan: false` and no `ownedPaths`; that a breaching gate is
  what the pass then reports; and that a clean gate leaves the agent failure
  priced as work. Deleting the failed-agent gate block, or flipping its
  `ownershipRan` to true, each makes it red (mutation-proven) — previously both
  mutations left the whole workspace green.
### flows & cli residue (#427, #416, #414, #418)

Four residues from earlier waves, each with its shape already argued in its
issue: a drain client that spent the fleet failure alarm on a busy daemon, a
direct-file verb family whose default data dir ignored the deployment it was
run against, an exit-20 remedy that advertised a command that cannot parse,
and a doc-pin check whose own message claimed the wrong thing.

#### #427 — a drain whose RPC deadline expires on a busy daemon is a retryable skip

`tally-drain.timer` fires every five seconds, and a saturated daemon can take
longer than the 60s client deadline to answer `queue.drain` on a connection
that was established. That exited 1 and surfaced as a per-user unit failure —
~52 in one day on the coordinator, every one self-healing on the next tick.

- `tally daemon drain` now treats `queue.drain` deadline-exceeded as a
  retryable skip: exit 0, with the line naming the expired deadline still
  written to stderr, plus that the skip is retryable. The safety argument is
  the event files': they are durable on disk and the next drain picks them
  up, so nothing is lost.
- The skip is narrow: only `queue.drain`'s own client deadline on the
  periodic spelling is absorbed. `tally queue drain` keeps failing on the
  same hang, and every other established-connection error — including a
  daemon that is listening and refuses — keeps its exit code.
- Proven at both seams: a unit test pins the classification (deadline on
  `queue.drain` is the skip; another method's deadline, a rearm-window
  exhaustion, a refusal, and an absence are all not), and an integration
  test runs the real binary against a server that connects and never
  answers: `daemon drain` exits 0 and names the case, `queue drain` exits 1.
- The Home Manager `tally-drain` unit's `daemon drain` spelling is now
  pinned in the module contract, as the NixOS one already was — the
  user-unit half is the one whose failures the fleet watcher reports, and
  `queue drain` there would leave both this absorption and #411's inert.

#### #416 — `TALLY_DATA_DIR`, honoured by the default and exported by both modules

The direct-file verbs resolved an omitted `--data-dir` to the user data
directory, so `reader-state archive` on a deployment printed an affirmative
record, exited 0, and wrote a brand-new store in the wrong place — a silent
no-op with a success message.

- `default_data_dir()` now honours `TALLY_DATA_DIR`, taken verbatim as the
  directory. Precedence: an explicit `--data-dir` flag at the call site,
  then `TALLY_DATA_DIR`, then the XDG default (`$XDG_DATA_HOME/tally`, else
  `~/.local/share/tally`). Unset or empty, local use resolves exactly as
  before.
- The variable is taken verbatim, not searched: aimed at something that
  cannot hold the store, a write verb fails naming that path rather than
  falling back to the XDG default, because a fallback would restore the
  silent no-op this closes.
- Both modules export the variable alongside the data directory they
  already configure — on their units (daemon, witness-emit, retention) and
  in the operator's own environment, which is where an omitted `--data-dir`
  actually resolved to the wrong store: `home.sessionVariables` on Home
  Manager, `environment.variables` on NixOS, both `mkDefault` so an
  operator's declaration wins. On a NixOS deployment that store is mode
  0700 and owned by the service user, so an operator who is not that user
  is now refused by name instead of quietly writing a store elsewhere.
- Proven at the seam by subprocess tests for every precedence tier (flag
  beats variable, variable beats a *set* `XDG_DATA_HOME` and yields to none,
  empty is unset, HOME fallback unchanged, an unusable value fails loudly,
  and a read verb follows the variable to a seeded ledger), and at the
  modules by evaluated assertions that the export exists with the configured
  path on both units and both login environments.

#### #414 — no executable command for a malformed run identity

A flag-shaped `flowRunId` from a foreign producer (a leading `-` after
trim) still rendered a `remedy` interpolating it, advertising a command
clap refuses with exit 2 — `--flow-run-id --reason` parses the id as the
next flag.

- The exit-20 rendering now splits on the flag shape in both fields an
  operator reads: the `remedy` member is `null`, and the refusal message
  keeps the why-clause but replaces the command with its own sentence
  naming a malformed run identity — the same rendering family as the empty
  case, but distinct from it: a run was named, badly, which is not "no run
  named".
- The raw `flowRunId` stays visible in `details`, preserving #401 item 3's
  invariant. The two emptiness definitions (`trim().is_empty()` in both
  sites) stay untouched and in agreement, and no UUID validation is
  introduced anywhere in the fourteen-member map — the leading dash after
  trim is the entire test.
- The documented `remedy` nullity rule — one wording, pinned identical on
  `submission-and-replay.md` and `errors.md` — now states both shapes that
  produce `null`, and a doc pin checks the sentence against what the code
  does rather than against itself.
- Mutation-proven both ways: dropping either guard (the `remedy` arm or the
  sentence branch) turns the new test red, and the same test asserts a
  well-formed id still gets its command in both fields.

#### #418 — the doc-pin check tells the truth about what it rejects

The end-of-span check in `supersession_docs.rs` fired on any `|` line after
an `:end` marker, including a blank line followed by a *complete* second
table — which renders perfectly, because a blank line ends a table in
markdown — and its panic message claimed a marker inside a table splits it,
steering the obvious fix back into the rendering defect the pin exists to
prevent.

- The check now skips blank lines and distinguishes the shapes: a following
  complete table (header + separator rows) is accepted; it fires only when
  the content after the marker is a bare row, with or without a blank line.
  The panic message and the rustdoc now describe the shape actually
  rejected. Probe A (blank line, bare rows) stays red; probe B (blank line,
  complete table) goes green — both as synthetic in-test documents, and the
  mutation that restores the old check turns probe B red. "Complete" means a
  real delimiter row: a horizontal rule under a row is still a bare row, and
  a third probe pins that.
- The live `replay-divergence` test's label comment pointed at "the
  in-process tests in `tally-flow`" for coverage that was not there. The
  comment now names the two tests that bind `recordedLabel`/`currentLabel`
  at the site this fixture's refusal is actually raised from — the
  dedup-conflict path in `crates/tally/src/flow_live.rs`, whose label
  binding already existed — and separately names the runner's own
  comparison site, where the mock ledger now returns one label while the
  script derives another so that site is bound too. The stated limit is
  unchanged; only the pointer was wrong.

### Steward driver gates (#385, #386)

Two mechanisms that both live at the boundary between an unattended agent and
the campaign's public or repository state: one holds every prose surface the
steward publishes to a machine-checkable grammar, the other detects a lane
that wrote outside its authorized paths after the write already happened.

#### #385 — the narrate slot's content contract extends to PR prose, the closing summary, and steering notes

The steward narrate slot already validated conventional-commit messages
deterministically; PR prose, the closing summary, and steering notes had no
outcome-first grammar check at all.

- Added `validate_outcome_first()` to `spec_build_driver.py`: a leading
  sentence before any list, a past-tense opening verb, no exclamation mark,
  and a bounded length — the managed-agents content contract, made
  machine-checkable and applied uniformly to steward-proposed and
  driver-rendered text alike.
- `validated_narration()` now enforces it on the proposal body (PR prose and
  the squash commit body); a narrator whose proposal fails both attempts
  falls back to the task-id template as before, but the fallback body now
  carries a durable, bounded fact that it fired and why — no silent
  template, per the AUGUST-02 lesson.
- The closing summary's leading sentence now reads outcome-first ("Settled N
  of M task(s) against durable merge/checkpoint facts.") and self-validates
  against the same grammar, so a future template edit that drifts from the
  contract fails the node loudly instead of publishing unchecked prose.
- Steering notes (the diagnose slot's output) are now validated the same
  way, and are constructive-correction shaped: when the failing task's gate
  evidence names a check id (and, for a `forbidPaths` rejection, an
  offending path), the diagnosis must name it too or it is replaced with a
  deterministic fallback note carrying the rejection reason.
- Audited the daemon-authored `MESSAGE` strings on both paths that produce
  them. `crates/tally-core/src/journal.rs`'s synthesized default now opens
  with a past-tense verb for all 11 `TallyEvent` kinds (`Enqueued`,
  `Dispatched`, `Recorded a heartbeat for`, …), with a test asserting the
  format for every kind by name so a 12th event added later fails loudly
  instead of shipping unaudited. That default is unreachable for
  `evidence_pass`/`evidence_fail`, whose `MESSAGE` is always the evidence
  check's own reason, so every reason string written inline in
  `crates/tally-core/src/evidence.rs` was reworded to lead with an outcome
  too (`Matched exit code 0 == 0`, `Recorded a witness span of 0.25s`,
  `Confirmed the artifact exists (…)`, `Validated the store path`, `Matched
  the content hash …`, and their failing forms) and is held to the same
  shape by its own test, which drives the passing and failing arm of every
  check kind. Two failing arms keep their existing wording: a failed
  artifact read and a rejected store path report `EvidenceError`'s and
  `NixStoreError`'s `Display` text, which tally does author but shares with
  every other display site of those errors — rewording it to suit the
  journal would change error prose on surfaces that have nothing to do with
  the journal.

#### #386 — a tree-delta permission gate around campaign agent nodes

The SSSF `permissions.py` import: permission is verified the way every other
claim in this system is — after the fact, against the repo itself. Tally's
hardening presets are preventive; this is the detective complement, and it
lands where the unified worktree manager already owns lane lifecycle.

- `campaign_worktrees.py` gained `change_set_fingerprint()` (a path → digest
  for every entry git lists, tracked or untracked), a persisted before/after
  snapshot pair, and `change_set_delta()`, which reports every path that
  appeared, disappeared, or changed — content-based, so a reversion of an
  uncommitted change back to its prior bytes is caught the same as a forward
  edit no commit history could ever see. Every listed entry gets a row: a
  regular file by its content, a symlink by its target string (never
  followed, so the gate cannot be walked out of the worktree), and anything
  unreadable — a mode-000 file, a directory, a fifo — by a metadata-derived
  stand-in, so a write the driver cannot open is still judged rather than
  dropped.
- `prep` now fingerprints the worktree immediately before every implementation
  task's agent node runs; a new `treeDelta` driver action compares that
  snapshot against the worktree's content once the agent node and the
  ownership gate have both finished, and fails, naming every offending path,
  on any delta outside the task's allowlist. A pass whose agent node fails
  returns before this node, so its uncommitted writes are not judged by it —
  a committed stray is still caught by the ownership gate on the next pass;
  the uncommitted case on a failing pass is open (refs #424).
- The allowlist is per-task and never silently permissive: a task's declared
  `conflictDomains` (non-empty) is the allowlist; an explicitly empty
  `conflictDomains` allows nothing; an absent `conflictDomains` falls back to
  exactly the paths the ownership node just certified as the task's own
  committed change-set — the agent's proven work is self-authorizing, nothing
  else is.
- A breach aborts the lane rather than buying a retry or a steering attempt —
  the write already happened, so there is nothing to redo. It reuses the
  existing diagnosis ledger: `steer` posts both the attempt-1 and attempt-2
  diagnosis receipts atomically for a breach, so the task is permanently
  blocked as of this pass, the offending paths are witnessed in the posted
  comment and in the failing `treeDelta` job's own evidence (`query
  job`/`query proof` show it like any other campaign gate failure), and the
  path list still reaches the steward's diagnose slot. The breach comment is
  held to #385's content contract and to the same public length bound the
  ordinary steering path guarantees; where the steward's prose is refused,
  the refusal replaces the prose without swallowing the breach.
- A new e2e scenario proves the reversion case against a real worktree and a
  real `git checkout` reversion, not a mocked file-state dict.

### pi adapter residue (#405, #406)

The follow-ups the #387 lane's config-only cap deferred. #405 is the Rust-side
and cosmetic residue of that cap; #406 is the config-side consistency work its
round-2 evaluation found.

#### #405 — the pi capture is now what the docs say it is, and a committed test runs it

Three `crate::occupancy` doc statements still justified pi's undeclared usage
mapping by "no real capture". The capture exists; what it justifies is
*declining*, because pi states usage per assistant message and never per
attempt, so a declared `inputTokens` there would report one turn's figures as
an attempt's spend. The module doc now says that, and the `context_tokens`
doc no longer lists pi among the adapters with no occupancy scrape — it has
one.

`test/fixtures/traces/pi.jsonl` is now read by the trace round-trip
acceptance test, which previously covered only claude-code and codex. It is a
pure real capture with no synthesised unknown event and no invented trailing
garbage, so the two tail assertions written around the other two fixtures are
conditional on that tail; what replaces them for pi is stronger in the
direction a real capture cares about — every one of its lines must parse — plus
assertions on pi's own framing (the session header, an assistant `message_end`
carrying a `toolCall` block, the separate `tool_execution_end`, the
`message_update` echo, and usage stated inside the message rather than at the
top level).

Placing a pre-prompt option on an adapter whose argv has no `--` terminator no
longer fails. `render_launch_prefix` required a trailing `--` to place
`prePromptArgv`, `approvalPolicy`, `sandboxPolicy`, `launch.cwdArgv`,
`launch.model` or `launch.effort`, which made a **pi-derived** adapter — pi is
the one preset that declares no terminator, because pi rejects one outright —
refuse every pre-prompt option with an error naming a convention it abandoned
on purpose. (The shipped `pi` preset never reached that error: it declares
`launch = {}`, so a pre-prompt option is refused earlier, on authorization.)
Options are now appended at the end of a terminator-less prefix, which is where
a harness expects its own flags; a prefix that does end in `--` still gets them
before the terminator, and an adapter with no argv at all still fails, now
saying why.

Two cosmetics: the synthetic `input_tokens` block in `flake.nix` is labelled as
synthetic and as *not* pi's key set, so it cannot be misread as evidence about
pi's wire format beside the real-capture block below it; and the pi preset
records that its `message_update` echo makes stdout grow with the square of a
turn's length, so a long pi campaign reaches the 16 MiB trace read bound far
sooner than a codex or claude-code one. Truncation is reported rather than
hidden, so that is a sizing note, not a defect.

#### #406 — the valid-turn guard now covers every pi capture an operator reads

#387 guarded `occupancy` against `stopReason: aborted` and `error` and left
`finalMessage` reading every assistant `message_end`. An attempt that ended on
an aborted turn carrying partial text therefore reported that truncated
fragment as the node's answer, unmarked and indistinguishable from a complete
one, while occupancy correctly held at the last valid turn. `model`
(`$..model`, last match) had the same shape and a louder consequence: it
pinned a model from an excluded turn, and both the rendered resume argv and
the model recorded for the completed job carried it. All three captures now
carry the same two `stopReason` clauses **and** the same scoping to assistant
`message_end`, and the scoping is the half that does the work: pi emits
`message_start` / `message_update` / `message_end` for every message, all
carrying the same `AgentMessage` and so the same model, under
`stopReason: pending` until the message closes. A filter that matches those
mid-stream records excludes an invalid turn's `message_end` and then reads the
same model straight back out of its `pending` records. `usage` stays
unguarded, as before and for the stated reason. The `adapter-presets` check
asserts the aborted fixture's whole rendered argv, not just the capture,
because the argv is where the wrong model became operator-visible.

The scoping narrows one case, deliberately: an attempt whose stream never
closed an assistant `message_end` — a SIGINT-truncated run, say — now yields
no `model` capture, so its resume refuses with `resume capture "model" is
absent` instead of rendering one. Such a stream states only the model of a
turn whose outcome is unknown, and an aborted turn's mid-stream records are
indistinguishable from an open valid turn's until its `message_end` arrives,
so no pattern both excludes the first and recovers from the second.

There is no way out of that on this preset, and the consequence is worth
stating rather than leaving to be discovered. A pi-*derived* adapter that
declares `launch.model` can have a job pin one, but the shipped `pi` preset
declares `launch = {}`, so a job-supplied model is refused before any template
renders — `model override is not authorized by this adapter`. A pi attempt
whose stream never closed an assistant `message_end` therefore cannot be
resumed by tally at all: the operator re-runs it from scratch, or hand-authors
a pi-derived adapter that declares `launch.model`. That is a real narrowing
against the previous `$..model`, which did render such a resume — pinned to a
model no completed turn was known to have used.

`test/fixtures/traces/pi-aborted-turn.jsonl` could not observe any of this.
Its real aborted message was aborted during model load, so it carried no
`text` block at all and `finalMessage` fell through to the last valid turn
whether guarded or not. It now carries a partial `text` block, and the aborted
turn is spliced in as a whole turn — `message_start`, `message_update`,
`message_end` — rather than as a bare `message_end`, because that bare shape
is not one `pi --mode json` can emit and a fixture without the mid-stream
records cannot see a `model` guard reading an excluded turn's model out of
them. The added block and the two derived lifecycle records are labelled in
the directory's README beside the splice it already documented. That README
also now answers, once, what these fixtures structurally cannot see: the
`error` half of the guard, a turn left open at end of stream, the echo's
growth at campaign scale, anything about resume behaviour, and anything about
redaction.

Two comment corrections. The occupancy guard's evidence ordering was inverted:
for a non-interactive `pi --mode json`, `error` is the reachable in-stream
invalid-turn branch (pi's own context-overflow signal), while an in-stream
`aborted` `message_end` cannot be produced headlessly at all — SIGINT
truncates before any assistant `message_end`, exit 130 — so the aborted turn's
provenance is pi's session store, and the comment now says that instead of
"proven from real pi data". And the leading-dash narrowing has a quiet half
worth naming: a payload that *is* a pi flag is consumed as that flag and pi
launches a fresh session with no work to do, where a non-flag leading-dash
payload fails loudly.

### Daemon startup & generation residue (#419, #379, #407, #378)

One lane about work that runs at daemon startup, and state written under one
regime and judged under another.

#### #419 — the second `tally-core --lib` flake population, and what it actually was

**Four** known members, not two: the issue body names two, a comment on the
issue names a third in a third module, and verifying the repair of that third
one surfaced a fourth in a fourth module. Every one reproduced red inside the
full `-p tally-core --lib` suite and green in isolation, at roughly one
full-suite run in thirteen to twenty-four, and every one is a race against
sibling tests in the same process. None is the mechanism the issue proposed, and
all four are fixed here. They are **four distinct mechanisms**, not four
symptoms of one — the opposite of what the issue's comment expected, and the
single most important thing this subsection records, because it is what decides
whether the population can be called closed.

- `retention::tests::capture_locks_expire_by_age_only_when_no_holder_has_them`
  released its capture-lock holder by closing the file and immediately asserted
  that the sweep collects it. `flock` binds to the open file description, not
  the descriptor, so every `fork` this process performs — every
  `Command::spawn` a sibling test makes, on any thread — duplicates that
  description into the child until the child `exec`s, and the lock outlives the
  close. The sweep then reads a live holder that no longer exists and prunes 0
  instead of 1. The test now releases with an explicit `LOCK_UN`, which removes
  the lock from the description itself so no duplicate can outlive it — the
  same thing the production holder (`UnitReservation::drop`) already does.
  The issue's hypothesis — a probe answering "cannot determine" as "do not
  prune" — is not what happens: the sweep maps only `WouldBlock` to a skip and
  every other errno to a hard error, so an exhausted descriptor table would
  have failed the test with an I/O error, not a `0`.
- `daemon::tests::a_gpu_pool_jobs_witness_carries_measured_gpu_seconds_and_charge`
  asserted on the completion lifecycle event without awaiting the post-ack
  task that emits it. `completed_event` is emitted from `spawn_local` by
  design, so a terminal `await_job` says nothing about whether it has run. The
  test now calls `drain_post_ack_tasks`, which is what the eleven other tests
  that observe post-ack state already do.
- `executor::tests::launcher_failure_without_visible_unit_preserves_error_promptly`
  wrapped the call under test in a 100 ms `tokio::time::timeout` — a wall-clock
  deadline assumption inside a test that forks and execs a shell script, so a
  loaded host failed it with `Elapsed(())` while nothing was actually masked.
  The property is now counted rather than timed: the masked behaviour is
  `reclaim_identity_exact` entering its retry loop, which inspects the identity
  201 times or waits out the 60 s launch-visibility timeout, where the prompt
  path inspects exactly twice. Two orders of magnitude apart, and load cannot
  perturb a count.
- `producers::tests::interactive_cancellation_still_terminates_gh` waited for
  its helper by polling for the existence of a pid file, but the fake `gh` it
  installs published that pid with a bare `> file` redirection, which creates
  the name before `printf` writes into it. Under load the reader wins that race,
  reads `""` and fails `parse::<i32>` with `ParseIntError { kind: Empty }`. The
  fake now writes a sibling and renames, so the name never resolves to a partial
  state and existence really does imply a readable pid.

Every fix removes the window rather than retrying through it, and the suite is
still fully parallel — serializing it stays a non-goal, because a false red is
cheap and a false green must remain impossible.

**The population is not declared closed.** "Latent parallel-execution flakes" is
not an enumerable set; the fourth member was found by verifying the third, and
each new member has had its own unrelated mechanism rather than sharing one. The
exit criterion the issue states is a measured rate over a wave, which no single
lane can observe. Four known members are fixed and each is proven under the load
condition the issue names; the issue stays open for that wave-scale
verification.

#### #379 — the startup budget is per phase now, and it says where the time went

Everything before `READY=1` is charged to `TimeoutStartSec` and never to
`WatchdogSec`, and the first estate-scale measurement of that budget was 61 s of
90 s, on a trend adding 5–8 s per heavy day. The 90 s was not a decision: the
daemon unit declared no `TimeoutStartSec` at all and inherited the manager
default. Worse, the journal was silent from `Starting` to the first
late-startup warning, so the 61 s could be measured but not attributed.

- `Daemon::open` and the pre-`READY` half of `run_loop` are divided into twelve
  named phases, and each boundary sends `EXTEND_TIMEOUT_USEC=` — the mechanism
  systemd provides for exactly this case, and which this daemon did not use.
  The limit stops being "how long may the whole of startup take" and becomes
  "how long may any one phase take", so an estate that has grown keeps starting
  while a daemon wedged in a phase still dies on the same clock. `STATUS=`
  names the running phase, so a slow start is legible in `systemctl status`.
- One line before `READY=1` names every phase and its wall-clock. That line is
  the durable artefact: the next lane that adds startup work has a number to
  check against, and is expected to add a phase of its own so its cost is
  attributable rather than folded into a neighbour's. The phase list is pinned
  by two tests for the same reason — one over what `Daemon::open` returns, and
  one over the rendered report line `run_loop` actually emits, which is the only
  surface carrying the phase `run_loop` itself opens.
- Both modules now declare `TimeoutStartSec = "90s"` on the daemon unit,
  matching `daemon::startup::STARTUP_PHASE_BUDGET`, so the limit is a choice
  the module made rather than whichever default the manager carried.
- `doc/src/operating/recovery.md` records the measurement, the mechanism, and
  how to grep one phase across restarts to see which part of startup is
  growing.

Raising a static budget was the alternative and was rejected: it buys headroom
without telling anyone when it is being consumed, which is the state that
produced this issue.

#### #407 — failure-stderr recovery is a one-shot, and says so in one line

`reconcile_failure_stderr` walked every terminal `Failed` witness record at
every startup and re-probed captures that were permanently gone: 227 identical
warnings on the startup #379 measured, about 2,951 across five days, enough to
bury the genuine startup signal beside them, and a cost that grew with failure
history without bound.

The files are missing for a reason upstream of this pass, and it is not
retention. `write_capture_generation` is fsynced before `systemd-run` creates
the unit, and `archive_current_capture` returns early when any of the capture
set is absent — so an attempt that failed before its stderr stream existed
leaves a generation marker that nothing ever retires, and the recovery pass
read that marker as "recoverable" at every start, forever.

- The pass now persists a cursor, `state/failure-stderr-cursor.json`, holding
  the witness sequence through which recovery has reached a definitive answer.
  A record's captures are final by the time it is terminal, so a second attempt
  can only reach the same answer; the steady-state cost is now zero probes
  rather than one per historical failure.
- Only two outcomes are definitive: the projection was written, or the source
  is absent (`NotFound`). Anything else — a contended lock, a stream that
  cannot be opened without following a link — leaves the cursor short of that
  record, so a later start retries it. The cursor is a contiguous high-water
  mark, not a maximum, so a record behind a deferred one is retried too.
- The 227 per-record warnings are one line with named fields:
  `examined= recovered= absent= deferred= reconciledThroughSeq=`. A per-record
  line survives only for the deferred class, which is rare and actionable.
- Measured: 227 doomed probes cost 10.7 ms of filesystem work, falling to 37 µs
  once reconciled. Against #379's 61 s that saving is negligible and is stated
  as such; what the fix actually buys is that the cost stops growing, and that
  a startup's log is readable.

The richer answer for the log line would have been a `TALLY_EVENT` journal
record with real fields. That was deliberately not done: adding an event type
this phase would collide with the audit of the existing eleven.

#### #378 — pre-label campaign captures are stranded, and `tally migrate capture-labels` moves them

The issue claimed no impact and asked for the trace first. The trace says the
data is operator-visible and silently unreachable.

`retained_capture_paths` is what `query.run` calls to attach `capturePath` and
`stderrTail` to a failure, and it resolves every stream through `capture_stem`
with no fallback to the bare-uuid name — the only fallbacks it carries are
`.err`-versus-`.adapter.err` suffix ones. `query.log` does not resolve captures
at all, so the surface named in the issue was the wrong one; the affected
surfaces are `query.run`, `query.trace`, and the recovery-time stderr excerpt.
The capture *generation* marker is keyed on the bare uuid in both binaries, so
it still matches, and that is what makes this quiet: the lookup succeeds and
reports the failure as having no capture rather than reporting that it could
not find one. So the answer is (a) — genuinely unreachable — with the sting
that nothing anywhere says the bytes are still on disk.

- `tally migrate capture-labels` is the `unit-exit-labels` sibling the issue
  predicted. It moves `capture/<uuid>.*` and `capture/archive/<uuid>/` to the
  `<uuid>.<task>` stem, plan-first, idempotent, coordinator-only, with
  remote-owned rows reported rather than claimed. The rename is a prefix
  substitution, so a stream this migration has never heard of moves with the
  rest; nothing is rewritten, and the bare-uuid-keyed `unit-exit/` records are
  deliberately untouched.
- An entry present under both stems is reported in `skipped[]`, not resolved:
  the command does not choose between two captures.
- Strict derivation stays. A permanent bare-uuid read-path fallback was
  rejected under the policy #371 settled — it would make every future reader
  carry a historical naming scheme, and would silently resolve a different
  row's capture on a stem collision.
- `doc/src/operating/recovery.md` and `doc/src/operating/cli.md` record the
  finding and the procedure. Unlike its sibling, no startup error names this
  command, because nothing refuses — which is precisely why the finding had to
  be written down.

### Recurring-cost hygiene (#396, #411, #395, #404)

One lane of four fixes with a shared shape: each one makes something the fleet
pays for repeatedly stop costing. A flake stops red-gating innocent shas, a
deploy stops raising a false alarm, a map stops growing, a response stops
repeating itself.

#### #396 — the `ETXTBSY` race in `fake_gh` no longer red-gates innocent shas

`fake_gh` wrote a shell script and marked it executable, then exec'd it. The
kernel refuses to `execve` a file any process still holds open for writing, and
in a parallel test binary a sibling thread that forks between `fs::write`'s open
and close carries that write fd into its child until the child's own `execve`
closes it. Under load that is a `Text file busy` failure in a test whose diff
never touched `campaign.rs`; it cost a full gate cycle on sha `80eb6c0`, which
passed on a quiet host.

Rather than retry through the race, `fake_gh` and the three remaining
write-then-`chmod` helpers now use the idiom #117 introduced for exactly this:
the behaviour is published through a non-executable sidecar and the exec target
is a symlink to a checked-in provider nothing ever opens for writing, so the
window never opens. That property — the exec target is a file this process
never opens at all — is asserted at both shared installers, so all four
converted helpers are covered by construction; asserting only that the program
runs would have passed for a written-then-`chmod`ed one too, which is the whole
race. A test also holds a write fd open to show the hazard is real rather than
theorised, instead of waiting for load to hold it.

#### #411 — a periodic drain that finds no daemon is a skip, not a failure

`tally-drain.timer` fires every five seconds and a `tally-daemon` restart takes
longer than that, so every activation that changes the daemon unit had a good
chance of catching a drain mid-flight, exiting 3 against a socket being
replaced, and producing a real per-user unit failure. It self-clears, but the
fleet's journal watcher cannot tell it apart from the failure burst that watcher
exists to catch.

- `ConditionPathExists` on `tally-drain` now also names the socket the command
  connects to, in both the NixOS and home-manager modules. The existing config
  guard is kept rather than replaced, because systemd ANDs repeated conditions.
  A drain scheduled while the daemon is down is recorded as a skip, not a
  failure.
- `tally daemon drain` exits 0 when that socket is unreachable, covering the
  narrow race the condition cannot. Scoped to the connect-time absence alone:
  `tally queue drain` is untouched, a daemon that is listening and refuses the
  drain still fails, and the line naming the case is still written to stderr.

`BindsTo`/`PartOf` was considered and rejected — it changes stop-time semantics
for an in-flight drain to buy a problem these two already solve.

#### #395 — `context.jobs` is the daemon's live set, and stays one

The map had no `remove`, `retain` or `clear` anywhere in the tree: it grew with
every job the daemon had admitted since start and never shrank, and a `Job` is
far larger than the membership record whose growth it dominates. The cost is on
the hot path, not just resident — the compaction live set, the dedup sweep and
the guardrail child count are all rebuilt over it on the admission path.

A job that reaches a terminal disposition now leaves the map. Every one of those
consumers already discarded terminal entries on the way past, so this is neutral
for all of them by construction. Nothing is lost with the entry: `cancel` still
answers `alreadyTerminal` and `--resume-from` still continues a finished
session, both from the row seed and query fact that already outlive the job and
are restored across a restart. An id the daemon never admitted is still not
found.

One guard changes meaning with the prune and is worth naming, because it is
durable if it is wrong: `finish_job` re-checks the job under the write lock
after awaiting the scrape, capture and accounting without it, and a job retired
inside that window must end that execution quietly rather than append a second
canonical witness for it. That window is now reachable in a test, which
force-cancels the job inside it and asserts exactly one canonical witness for
the `(task, attempt, leaseEpoch)`.

#### #404 — the attestation chain is not read before it is needed, and a standup states its constants once

- **Deferred read.** `read_attestations` parsed and hash-verified the whole
  append-only ledger on every `query.run` and `query.standup`, before the run id
  was known to exist. Both now defer that read behind the predicate that decides
  whether there is anything to sum, and in each case the deferral predicate is
  the *same function* the real answer is computed from, so skipping the read
  cannot change what comes back.
- **`usageBasis`.** `StandupDigest` gains a `usageBasis` object stating the
  rollup's `provenance`, `composition` and `costBasis` once; each entry in
  `runs` omits the three it would otherwise repeat verbatim (~650 bytes per run,
  ~325 KB per response at 500 runs). The hoist is safe because those three are
  invariant by construction — one writer each, assigned from compile-time
  constants with no dependence on the run — and it is not assumed: a rollup
  whose statements ever differ carries its own inline rather than inheriting a
  digest-level claim that would be false for it. `query run`, which returns one
  rollup, still carries all three inline.

  `usageBasis` is what an omitted entry field is filled from on the way back in
  — the **producer's** copy, which travelled with the payload — and a reader
  falls back to its own compiled constants only for a digest that carries no
  basis at all. Filling from the reader's constants instead would make the
  digest and its own entries disagree whenever the two builds differ, which on a
  fleet whose coordinator pin trails its workers is the ordinary case rather
  than the exotic one.

  On any payload a current build produces, `usageBasis` is present exactly when
  `runs` is non-empty, and both are omitted from the wire when empty — because
  the window touched no flow run, or because reader-state hid every run it had
  (`archivedRunsHidden` separates those two). That is a property of what this
  build emits, not a rule for reading any payload: **a producer that predates the
  field emits `runs` with no basis**, stating the three constants inline on each
  entry, so a reader must not infer an empty `runs` from an absent `usageBasis`.
  In both cases there is nothing for the reader's own constants to displace —
  the entries carry their own statements, or there are no entries.

### Added

- **Lane: operator conveniences — evals gain a checkable coverage-manifest
  schema and a deterministic checker (#388).** A findings file (an eval's
  plain-Markdown output) can now carry a `<!-- eval-coverage-manifest:v1 -->`
  fenced-JSON section stating, per acceptance bullet and per reviewed file,
  `covered` / `reused` / `failed`-with-a-typed-class
  (`timeout` / `budget` / `input` / `unknown`, the last a mandatory
  catch-all) — an item-level taxonomy kept separate from a run-level one
  (`timeout` / `budget` / `crash` / `unknown`) covering the eval run itself.
  `test/eval_manifest_check.py` validates a findings file's manifest section
  and reports uncovered surface: an `expected` block naming the bullets/files
  the eval was supposed to account for, cross-checked against what the
  manifest actually covers. Proven to reject a manifest that omits a reviewed
  file entirely and one that types a failure with an unrecognized class
  (`test/eval_manifest_check_test.py`, `test/fixtures/eval-manifest/`). Not
  wired into `nix flake check` or `test/fleet-gate.sh` — the issue's explicit
  cap is no gate-side change; an orchestrator close-out names the checker
  instead of vouching for the claim by hand. Emitting the manifest from a
  real eval is an adoption step for the next dispatched wave, not a code
  change this lane can claim.

  *Round-1 repair:* the success line no longer prints a bare `ok` — a
  manifest with no `expected` surface declared (or one declaring only empty
  lists) now says so explicitly ("coverage NOT checked") instead of reading
  identically to a manifest whose declared surface was fully accounted for.
  The checker also now refuses a findings file carrying more than one marked
  block instead of silently grading the first — the adoption path of quoting
  this module's own docstring example inside a findings file previously
  meant the quoted example got graded, not the real manifest.

  *Round-2 repair:* the round-1 success line described the **declared**
  surface count as "N/N covered", but `covered` is one of the schema's three
  status terms and a declared key is satisfied by an entry of *any* status —
  so a manifest whose every declared bullet `failed` printed "2/2 bullets
  covered" beside `failed=3`. The clause now reads
  `N/N bullets accounted for (M covered, K reused, F failed)`, computed from
  the statuses of the entries themselves, and declared keys are deduplicated
  so repeating one cannot inflate the denominator. Separately, the two
  success cases were textually distinct but **mechanically identical** — both
  exited 0 and both matched `: ok` — so the orchestrator close-out this
  checker exists for could not tell them apart. Exit codes are now a
  documented contract (`0` every declared surface accounted for — which is
  not the same as verified; `1` refused; `2` usage; `3` schema-valid but
  coverage not checked), and each success line carries a stable
  `coverage=checked` / `coverage=unchecked` token.

  *Round-3 repair:* the exit-code table said exit 0 "licenses 'the eval
  covered what it said it would'" — round 2's own word conflation surviving
  in the one place its sweep did not reach, the machine-facing contract. At
  that point a manifest whose every declared item `failed` exited 0. The row
  was corrected to state what the code then guaranteed (every declared
  surface *accounted for*) and to point at the
  `covered=`/`reused=`/`failed=` tokens. That round was doc-only; #426 above
  now supersedes the zero-covered exit behavior.

- **Lane: operator conveniences — reader-state (`archived`, a free-form
  triage tag) on flow runs, never set by a run (#389).** A new durable store
  (`crates/tally-core/src/reader_state.rs`, `reader-state.jsonl` in the
  daemon's data directory) holds per-flow-run `archived` and a triage tag,
  outside the witness/attestation ledgers and excluded from every hash
  chain. It is written **only** by a new `tally reader-state
  {archive,unarchive,tag,untag,show}` CLI verb, which writes the file
  directly against the daemon's data directory (pass `--data-dir` if it is
  not the default) — no daemon socket, no RPC call, so no daemon or
  reconciler code path can touch it. `query run` now exposes `archived` and
  `triageTag` (and prints a loud `-- ARCHIVED` banner in its human text
  view); `query jobs` and `query standup` gain `--archived`/`--no-archived`
  (default: hidden) and filter on it. `query standup`'s digest gains two
  separate hidden counts — `archivedHidden` (task entries hidden, across
  `completed`/`gateFails`/`cancelled`/`inFlight`) and `archivedRunsHidden`
  (`runs` cost rows hidden, including a run that only *attached* a task
  rather than creating it) — each accumulated as the collections are
  filtered, by the same call that filters them and never by a separate
  recount; two window-wide aggregates, `reused` and `canonicalGpuSeconds`, are
  deliberately **not** reader-state filtered and remain window totals. A
  corrupt or missing reader-state store degrades every reader to "nothing is
  archived" rather than failing the query (`ReaderState::read_advisory`),
  and the store self-compacts past `READER_STATE_COMPACT_THRESHOLD` records
  so a scripted toggle loop cannot grow it forever. Runs only, by design: no
  UI, no cross-host sync, no per-task granularity.

  *Round-1 repair:* the initial `archivedHidden` count omitted `runs` row
  removals entirely — a run that only attached a task (durable membership,
  not its orchestration capsule) had its cost row silently dropped with the
  count staying zero. Split into `archivedHidden`/`archivedRunsHidden` and
  covered by a regression test reproducing exactly that shape
  (`apply_reader_state_to_standup_counts_an_attach_only_archived_run_that_hides_no_task_entry`).
  A second test now pins that `archivedHidden` is a before/after difference
  and not a recount over `details`, and a third pins the `query.jobs`
  pagination cache-key fingerprint against dropping `archived`. `query jobs
  --flow-run <archived-run>` silently withholding items with no signal in
  the response is issue #415, not fixed in this repair; the direct-file
  verbs' data-directory default is issue #416.

  *Round-2 repair:* the "every removal is counted" property was pinned for
  one of the five collections the filter touches — dropping `cancelled`,
  `gateFails` or `inFlight` from the count, or adding any further uncounted
  filter, left the whole suite green while the digest under-reported what it
  had withheld. Rather than adding one test per hole, filtering and counting
  are now a single operation (`retain_counting`) — a removal made through
  that helper cannot miss its counter — and the function closes with a
  `debug_assertions`-only conservation check that catches a removal
  bypassing the helper in any of the collections its enumerator names. That
  enumerator destructures `StandupDigest` exhaustively, so a new field does
  not compile until it is named; binding it to `_` is then a visible
  decision that the field is not filtered here.

  *Seam with #404:* reader-state filtering can empty `runs` after
  `apply_standup_usage` has already stated a `usageBasis` for it, which would
  have left a digest claiming how its runs were summed while showing no runs.
  The invariant `usageBasis` documents — present exactly when `runs` is
  non-empty — is kept rather than weakened: the reader-state pass clears the
  basis when it hides the last run. It is now a property of the *composition*
  of the two calls, pinned by a composition-level test, since each lane tested
  its own function in isolation and neither gate could see the pair.

- **`query run` and `query standup` answer "what did this run cost" (#384).**
  `query.run` gains a `usage` object and `query.standup` gains a `runs` array
  carrying the same object per flow run the window touched. Both are summed
  **per attempt from the advisory attestation ledger**, keyed by
  `taskUuid`/`attempt`/`leaseEpoch`, over the run's **durable membership**
  (#391) — so a retried task is charged for every attempt the ledger holds,
  not only for the latest attempt its durable row keeps, and a node a run was
  handed but whose row names its creating run is inside the sum rather than
  silently missing from it. The ledger is the whole of what the rollup can
  see: it covers every attempt the ledger could speak for, and says so in
  those terms rather than claiming every attempt that ever ran.

  Three properties are load-bearing, because the failure this surface exists
  to prevent is a figure computed from the wrong evidence, wrong in the
  reassuring direction, and shaped exactly like a correct one:

  - **`inputTokens` alone is not the fresh-input figure**, so the rollup
    publishes `freshInputTokens = inputTokens + cacheWriteTokens` and states
    that addition on the wire in `composition`. claude-code's
    `cache_creation_input_tokens` are fresh, uncached prompt tokens its
    `input_tokens` excludes; a sum over `inputTokens` alone understates any
    cache-writing harness by its whole cache-write volume while printing a
    number that looks directly comparable to codex's. `reasoningTokens` is
    rolled up for visibility and never added to any total, because codex
    nests it inside `output_tokens`. Each harness's own
    `inputTokensAsReported` is deliberately not summed: the two conventions
    are not commensurable.
  - **Coverage is stated, never implied.** `coverage` counts member tasks,
    observed attempts, and attempts that reported usage apart from each typed
    absence (`not-reported`, `not-declared`, and an attestation predating the
    usage record), plus the member tasks the ledger holds nothing about and
    whether the chain verified at all. A ledger that failed verification or
    could not be read sums nothing and says so rather than answering with a
    confident zero. `attemptsReportedWithoutFigures` counts the attempts that
    reported usage **no** declared field path resolved out of, where absence
    is not unreadability and the record is still `reported`. Counting those as
    covered would grade a run whose adapter mapping resolved nothing as
    complete and costless; they raise `reported-without-figures` instead.
    That bucket is *total* drift only.
  - **A single renamed harness key is caught too.** When any of the four
    components the total is a sum of — `inputTokens`, `cacheReadTokens`,
    `cacheWriteTokens`, `outputTokens` — was reported by fewer attempts than
    `attemptsReportedWithComponents`, that component's sum is over a subset of
    those attempts and the rollup raises `partial-components`. Drift in one
    key leaves every other figure resolving, so the attempt still contributes
    and is not in `attemptsReportedWithoutFigures`; on a real claude-code
    capture one renamed `cache_read_input_tokens` silently removes 97% of the
    run's tokens from the total. Two exclusions are deliberate:
    `reasoningTokens` is not checked, because claude-code reports no reasoning
    figure and it enters no total, so checking it would fire on every claude
    run; and the denominator excludes attempts whose harness stated a total of
    its own and reported no component beside it — what an adapter declaring
    only a `totalTokens` mapping produces — because such an attempt declared
    no components to be missing.
  - **An attempt that reported only a total, beside attempts that reported
    components, raises `total-only-attempts`.** The exemption above is one
    *reported* shape wide, which is not the same promise as "an adapter that
    declared components is always judged": the rollup reads attestations, never
    the declared field map, so an adapter declaring components *and* a total,
    whose harness renames every component key at once, reports the exempted
    shape. Whenever such an attempt sits beside attempts that did report
    components, the component sums demonstrably cover fewer attempts than the
    total does, and the run says so — whichever kind of adapter produced them.
    Distinct from `partial-components` on purpose: that one means a component
    is missing *within* the attempts being judged, this one means an attempt is
    missing from the judgement altogether. The one case reported evidence
    cannot separate is a run where *every* attempt is total-only, which needs
    the declared field set the attestation does not carry.
  - **Authority is graded, and the grade is about the adapter, not the
    harness's reputation.** The whole rollup is `advisory-provider-capture` —
    harnesses reporting on themselves. `totalTokens.source` is
    `harness-reported` only when the adapter declared a `totalTokens` mapping
    and the harness filled it. **No shipped preset declares one**: codex's
    real `turn.completed` carries no `total_tokens`, and claude-code's
    `result` event carries a cumulative usage object of components without a
    total among them. So both presets read `derived-from-components` today,
    including a run spanning both, and `harness-reported` / `mixed` are
    reachable through an operator-defined adapter that declares the mapping.
    Every reason the sums are partial is a named `caveats` entry.

  `cost` is the harness's own `costUsd`, summed only where reported, and it
  carries a `basis` statement onto the wire: tally's cgroup `charge` is a
  distinct quantity, is not summed here, and is a **floor** that includes
  tally's own exit-recorder overhead (waiver `W-382-RECORDER`, #382). The
  human `tally query run` view prints the block with its coverage attached,
  because a terminal reader is exactly the one who will not go looking for
  the coverage object in `--json` — including the daemon's own `basis`
  sentence rather than a copy of it, and `--` for any component no attempt
  reported, so an absence and a measured zero never print the same
  characters. A daemon that predates the field sends no
  `usage` object and the human view prints nothing for it, since an absent
  field is not a claim about the run. Cross-run and fleet aggregation,
  budgeting, and enforcement remain out of scope.


- **The `pi` preset declares its trace framing, and an occupancy capture, from
  a real `pi --mode json` capture (#387).** `pi` carried launch and resume
  argv and every scrape capture but no `trace = { stream; framing; }` block,
  so a pi node produced no `TraceGeneration` and no `TraceLane` and
  `tally query trace` rendered no lane for it — an observability hole that
  read as "nothing happened" rather than as "nothing was declared", in the one
  adapter that has not yet run a campaign here. It now declares
  `stdout`/`json-lines`, which is what pi's own `docs/json.md` documents and
  what the capture now checked in at `test/fixtures/traces/pi.jsonl` does: 21
  retained lines on stdout, zero bytes on stderr. Config only; no Rust change
  was needed or made.

  The same capture settles pi's usage key names — every assistant message
  carries `{ input, output, cacheRead, cacheWrite, reasoning, totalTokens,
  cost }`, with `input` exclusive of both cache halves (second turn: input
  190, cacheRead 842, output 46, totalTokens 1078) — and settles something
  else: pi states usage **per assistant message and never per attempt**.
  There is no `turn.completed`-style roll-up anywhere in its stream. A
  declared spend mapping would therefore report one turn as an attempt's
  usage and understate every multi-turn pi node, so `usage` stays
  unmapped — now for a stated reason rather than for want of evidence. The
  honest reading of a per-turn figure is occupancy, and `pi` declares one,
  scoped to assistant `message_end` events under
  `residentInputTokens`/`residentCacheReadTokens`/`residentCacheWriteTokens`,
  which resolves to 1032 resident tokens on the checked-in capture. The
  capture also excludes turns pi marks `aborted` (and, by analogy with SSSF's
  `calculateContextTokens`, `error`): pi zero-fills every token field on an
  aborted turn, and `context_tokens` returns `None` only when all three
  resident fields are *absent*, so an unguarded scrape would report `Some(0)`
  — a fabricated empty context — for a session carrying a full one.
  `test/fixtures/traces/pi-aborted-turn.jsonl` is that stream, and the
  `adapter-presets` flake check asserts the guarded capture resolves it to
  the last valid turn. That check renders the preset against both fixtures
  and asserts every resolved value, so the declaration is proved against
  recorded bytes rather than against a stream written to agree with it.

- **Context occupancy is recorded beside session identity, everywhere
  `sessionRef` is (#383).** "Context is occupancy, not spend" — the number
  that decides whether a session can absorb another task, not what it cost,
  and **not** the cumulative total `crate::usage::observe` (#381) already
  normalizes under `totalTokens`: that figure is the last `usage` object
  anywhere in the stream, which for claude-code is the `result` event's
  session-lifetime roll-up and for codex is the final `turn.completed`'s
  cumulative total — both grow without bound across a session and would
  render as many multiples of a fixed context window if read as occupancy.

  `contextTokens` instead reads the tokens resident in the context window as
  of the attempt's **last valid assistant turn**: input plus both cache
  halves, excluding that turn's own output. The `claude-code` preset declares
  a dedicated `occupancy` capture scoped to only `type == "assistant"` events
  (`$[?@.type == 'assistant'].message.usage`, not `usage`'s stream-wide
  `$..usage`), under logical field names of its own
  (`residentInputTokens`/`residentCacheReadTokens`/`residentCacheWriteTokens`)
  so a lookup for one concern can never resolve against the other's declared
  capture inside `usage::resolve`'s searches-every-declared-capture
  semantics. `codex exec --json` states no comparable per-turn figure — one
  `turn.completed` per exec, carrying only the cumulative shape — so `codex`
  declares no `occupancy` capture and `contextTokens` reads `None` for codex,
  matching #381's precedent for `pi`'s undeclared usage mapping rather than
  restating the cumulative total under occupancy's name.

  `contextWindow` is the ceiling that total is measured against, with two
  independent, distinguishable provenances: a harness that states its own
  window inside the captured stream (the `claude-code` preset declares a
  `contextWindow` capture beside `usage`, `usageCost`, and `occupancy`,
  resolved at `modelUsage.*.contextWindow` — a real field in this project's
  own redacted corpus) and an operator-declared ceiling in the adapter's
  `extraConfig.contextWindow`. A stream-stated window wins when both are
  present; neither is fabricated, so `codex` and `pi` declare no scrape for
  it — no real capture from either has ever stated one.

  Both fields are independently optional: a scraped `contextTokens` with no
  known `contextWindow` is legitimate and does not blank the first, and
  absence never renders as zero. `journal.rs` task rows carry
  `TALLY_CONTEXT_TOKENS`/`TALLY_CONTEXT_WINDOW` beside `TALLY_SESSION_REF`
  (both `Conditional`, mirroring `TALLY_GPU_SECONDS`); `trace.rs` lanes carry
  them beside `session_ref` on every `TraceLane` and `TraceRecord`, including
  the journal-reconstructed fallback path a query surface falls back to after
  retention trims the live row; `query_v2.rs`'s `JobSummary` and
  `RowDetailFact` carry them beside `usage`, rendering as a `SourcedValue`
  with `advisory-provider-capture` authority for a scraped window and a new
  `advisory-config` authority (not `durable-admission-fact`: a config ceiling
  is read from live adapter configuration and does not survive a daemon
  restart the way a durable row field does) for a configured one. A job with
  a lost capture still records a config-declared ceiling, since it depends on
  nothing scraped. `query jobs --session` and `query trace` both expose both
  fields. `RowSeed.contextTokens` / `.contextWindow` are transport-only, for
  the same reason `usage` is: no write path persists them, and both are
  recomputable from the adapter configuration and the retained captures, so
  no row-version migration was owed. Recording only — no scheduling or
  admission behavior reads these fields; a future admission heuristic is a
  separate operator ruling.

- **The exit recorder fills `charge` and, for GPU-pool jobs, `gpuSeconds` from
  real systemd cgroup accounting (#382).** These witness fields have existed
  since the schema was designed but no write path ever set them: `charge` was
  always `None`, and the daemon's own completion lifecycle event fabricated
  `gpuSeconds: Some(0.0)` on every job regardless of whether anything was
  measured. `__record-unit-exit`, run as `ExecStopPost` while the transient
  unit is still queryable (before `--collect` can garbage-collect it), now
  issues one `systemctl --user show --property=CPUUsageNSec
  --property=ExecMainStartTimestampMonotonic
  --property=ExecMainExitTimestampMonotonic` and embeds the result as a new
  optional `accounting` field on `UnitExitRecord`. `CPUUsageNSec` becomes the
  generic per-job `Charge{unit: "cpu-second", amount, class: "measured"}`; the
  two monotonic timestamps become `gpuSeconds` for a job whose pool
  **explicitly** declares `resource = "vram"`, measured as systemd's own
  main-process wall-clock runtime rather than CPU-cgroup time, which would
  understate a job that mostly waits on the device. That runtime is a lower
  bound on how long the job actually held the pool's lease, not the lease
  span itself — the lease is held from admission through completion
  handling, which strictly contains it, so `gpuSeconds` understates true
  occupancy by a small, fixed per-job overhead.
  `witness::canonical_gpu_seconds` (unchanged) now sums a real,
  non-fabricated figure.

  `resource` is `PoolConfig`'s one field where "declared" and "effective"
  must be told apart: `ResourceKind::Vram` is `resource`'s own default, so a
  pool whose config says nothing about `resource` at all — `{"capacity": 4}`
  — must not read as a GPU pool. `PoolConfig.resource` is now
  `Option<ResourceKind>`; every admission decision that predates #382 reads
  the unchanged effective value through the new `PoolConfig::resource()`
  accessor (`unwrap_or_default()`, still `vram` when undeclared), while
  `gpuSeconds` is gated on the new `LeaseEngine::declared_resource_kind`,
  which answers only the narrower question and returns `None` for a pool
  that declared nothing.

  The NixOS/Home Manager `resource` pool option carries the same
  distinction: it is now `nullOr (enum [...])` with `default = null`
  (previously a required-shaped enum defaulting to the string `"vram"`), and
  the rendered runtime config only emits a `resource` key when the operator
  set one. Every one of the module's own admission-relevant assertions
  (mutex shape, budget-gb, windowed-consumption, usage-meter, and the three
  campaign-pool checks) reads the same defaulted-to-`vram` value they always
  did, through a new `effectivePoolResource` helper — the Nix-side mirror of
  `PoolConfig::resource()`. Only `gpuSeconds` sees the narrower, undefaulted
  reading. A checked-in fixture
  (`test/fixtures/pools/resource-declaration.golden.json`), re-rendered and
  diffed on every `nix flake check` (`checks.pool-resource-declaration`) and
  read back by a Rust test, pins that Nix's rendering and Rust's parsing of
  it cannot drift apart silently.

  A failed probe — a missing `systemctl`, a nonzero exit, a malformed
  property — is a typed absence (`accounting: None`) logged to the job's
  captured stderr, never a fabricated zero and never a reason to fail the
  exit record itself: accounting is advisory to the verdict. `[not set]`
  (accounting disabled for a specific property) reads the same way, and so
  does a monotonic timestamp systemd reports as the literal `0` for a unit
  that never ran — a real-but-non-obvious sentinel, confirmed against real
  systemd, that would otherwise mint a `gpuSeconds: Some(0.0)` nobody
  measured. The new `accounting` field is additive and optional with no
  schema-version bump: a record an older binary wrote round-trips unchanged,
  and a fixture pinned to that exact pre-#382 shape proves it.

  `doc/src/reference/witness-format.md` documents both fields' exact
  semantics: `gpuSeconds` is the declared-pool unit's main-process wall-clock
  runtime — a lower bound on lease occupancy, not GPU compute time and not
  an exact occupancy figure; `charge` is whole-cgroup CPU-seconds, including
  the exit recorder's own overhead (single-digit milliseconds, dominant on
  very short jobs, proportionally negligible on longer ones) — a known floor
  left for the eventual billing-aggregation lane to decide whether to
  subtract.

  Two now-stale sentences in the #381 usage-breakdown documentation
  (`crates/tally-core/src/usage.rs`, `doc/src/concepts/adapters.md`) said
  "codex has no cache-write category at all"; #381 itself had already
  falsified that by declaring codex's `cache_write_input_tokens` key. Both are
  corrected: codex does declare a cache-write category, observed at 0 on
  every real capture so far.

- **Per-attempt harness usage is normalized at the adapter boundary and
  persisted (#381).** Raw provider usage objects already landed in the advisory
  attestation ledger, but nothing reconciled them across harness shapes, no
  query surface exposed them, and the pool-meter feeder collapsed everything to
  a single number. A capture may now declare a `fields` map — a logical field
  name to the ordered candidate paths that carry it inside the captured value
  (`$` or the empty string is the captured value itself; anything else is
  dot-separated object keys, with numeric segments indexing arrays). The
  normalizer reads `inputTokens` or `inputTokensWithCacheRead`,
  `cacheReadTokens`, `cacheWriteTokens`, `outputTokens`, `reasoningTokens`
  (nested within output, never added to it), `totalTokens`, and `costUsd`.
  Adding a harness is an attrset in `nix/lib/adapters.nix`, never a Rust match
  on an adapter's name. The `codex` and `claude-code` presets declare every key
  their real captures carry — for codex that is all five, including the
  `cache_write_input_tokens` that is 0 on every real turn, because a measured
  zero must be stated rather than left absent. `pi` declares none until a real
  capture has been seen, and keeps the legacy reading.

  The fixtures behind this are excerpts of real captures, not hand-authored
  streams; see `test/fixtures/usage/README.md` for provenance and redaction.

  The record distinguishes three states and renders none of them as a zero:
  `not-declared` (the adapter configured no usage scrape), `not-reported` (a
  scrape was declared and the stream carried none), and `reported` — where a
  harness-reported zero lives, because it is a measurement. A total the harness
  did not state is derived from the components and labelled `derived-from-
  components`; a stated total its own components contradict is kept beside the
  computed sum rather than either being corrected. Cost is only what the harness
  reported: tally has no pricing table and computes no dollar figure.

  The durable seat is the `adapter-scrape` attestation, which carries the record
  beside the raw captures under the same task/attempt/lease-epoch key; the row
  carries it in memory only, so the two typed absences read back as a missing
  field after a restart rather than as a stated absence. `tally query job`
  renders it as a `SourcedValue` with `advisory-provider-capture` authority and
  `adapter-scrape` provenance; authorities are not collapsed and the witness
  schema is unchanged. Records written before the field existed read back
  unchanged and add no key.

  The built-in pool meter now reads the normalized record and charges the same
  number it charged before on every shape a harness emits — a harness-stated
  total, else the harness's own input figure plus its output figure, never a
  zero, and nothing at all when a figure arrives in a shape that is not a count.
  A capture with no declared `fields` keeps that reading verbatim, so the richer
  breakdown does not become a bigger bill on any pool. Two shapes no harness
  emits do diverge, both upward: a key present with a JSON `null`, and a
  whole-valued float, each of which the old reader refused to parse and this one
  charges.

### Fixed

- **The exit-20 `details` contract is now held to evidence instead of to
  itself (#400).** #397 gave the five supersession codes one fourteen-member
  `details` shape and documented it in two pages. What it could not give was a
  reason to believe either claim: the contract test compared production output
  with the same constant production iterates over, so a member could be added
  or reordered while every assertion passed and all three prose copies rotted,
  and the family's one wire rename was exercised only by in-process stubs that
  would have agreed with any name.

  - **#400 — the prose copies are pinned to the constant, and the rename is
    proved by a real process.** `crates/tally-flow/tests/supersession_docs.rs`
    parses the marker-delimited member table in
    `doc/src/flows/submission-and-replay.md` and the per-code table in
    `doc/src/reference/errors.md` and holds both to
    `SUPERSESSION_DETAIL_FIELDS` / `SUPERSESSION_CODES` — membership, order,
    and the derived members whose *values* the docs state (`transient`,
    `resolution`, `divergentInput`, and whether a code advertises a `remedy`).
    The `remedy` nullity rule is now stated once, in one wording, in both
    pages. The value that rule states and the member it blames are read out of
    the sentence and used to drive the check against the code, so the sentence
    is load-bearing: the two copies drifting apart, a wording that states a
    different value, a wording that blames a different member, and an empty
    span all fail. `errors.md` names `recordedLabel` and
    `currentLabel` where it used to say "both labels", so its `replay-divergence`
    row also carries the renamed members. A live daemon-driven
    `replay-divergence` (`flow_live::a_live_replay_divergence_names_the_current_hash_and_label_on_the_wire`)
    replays an admitted ordinal whose payload changed and asserts
    `currentHash` / `currentLabel` — and the absence of the pre-rename
    `expectedHash` / `expectedLabel` — on a real runner's stdout, the first
    live exit-20 assertion in the suite that is not an identity pin.

  - **#401 — the `remedy` guard reaches the field a human reads, and agrees
    with the repo's own definition of "no run named".** #397 guarded the
    `remedy` *detail*; `identity_refusal_remedy_sentence`, which embeds the
    same invocation in the operator-visible `message`, was left unguarded, so
    a refusal that could not say which run it was about still advertised a
    `tally flow supersede` missing its `--flow-run-id` value — exit 2 in an
    operator's hands. It now returns the why-clause without the command. Both
    guards test blankness as `trim().is_empty()`, which is what `run_script`
    has always meant by it, so a whitespace-only `flowRunId` from a foreign
    producer no longer renders an inert command either. Neither was reachable
    from any in-tree call site; both functions are public.

    A **flag-shaped** `flowRunId` — a foreign producer sending `--reason` or
    `-h` as the run id — is *not* closed by this, and is not claimed to be: it
    is not blank under anyone's definition, so both the `remedy` and the
    message still render a command that exits 2. Suppressing it would
    contradict the ruling below, and validating the member as a UUID is a wider
    contract change than this asked for, so the correct behaviour is left to be
    decided rather than assumed.

    A `flowRunId` a producer sent as something other than a string is now
    preserved rather than replaced with `null`. Every other member of the
    contract keeps whatever the producer sent, and "the producer named a run
    badly" is a different fact from "the producer named no run" — which is
    what the doc row promises `null` means. No `remedy` is derived from it.

- **The `pi` preset's launch and resume argv could not run (#387).** Both
  ended in `--`, tally's option-terminator convention across presets. pi has
  no end-of-options separator: it rejects one with `Error: Unknown option:
  --`, exits 1, and writes zero bytes to stdout — so every pi node was a dead
  launch, no capture was ever produced, and `sessionRef`, `occupancy` and
  `finalMessage` could never resolve. The trailing `--` is dropped from both
  argv lists and from the operator-facing preset table. The cost of dropping
  it is stated rather than absorbed: pi parses a workload argv whose first
  element begins with `-` as a flag, and nothing enforces otherwise.

  Two adjacent pi behaviours are now documented rather than fixed, because
  no configuration can fix them. pi keys its session store by the directory
  it was launched in, so a resume from a different cwd prints
  `Session found in different project` on stdout, prompts on stderr, and
  exits 0 having done nothing; pinning `--session-dir` does not change this,
  because pi still requires exact equality with the session's recorded cwd
  (`sessionCwdMatches`) and otherwise falls through to the same
  cross-project branch. A pi node must be resumed in the directory it was
  launched in, and pi exposes no cwd flag for `launch.cwdArgv` to assert it.

- **The exit-20 flow refusals now carry one `details` contract, identical at
  every raising site (#390).** `script-changed-mid-run`,
  `args-changed-mid-run`, `catalog-changed-mid-run`, `flow-run-superseded`, and
  `replay-divergence` all exit 20 and all mean the same thing to an operator —
  the run's recorded identity and the work in front of it disagree — but their
  `details` shape depended on where the refusal was raised. The three identity
  pins reported `recordedHash`/`currentHash`; `replay-divergence` reported the
  same disagreement as `expectedHash`/`recordedHash` and named no `flowRunId`
  at all; and its two mid-run discovery paths disagreed with *each other*, one
  carrying `taskUuid` and `kernelError` and the other neither. A monitor
  branching on an exit-20 reason had to special-case the site to find the hash
  that moved — two eras of error plumbing coexisting on a code family every
  daily-driven flow can hit.

  All five now emit the same fourteen members at every site, `null` where a
  code has nothing to say: `flowRunId`, `divergentInput`, `recordedHash`,
  `currentHash`, `recordedLabel`, `currentLabel`, `taskUuid`,
  `successorFlowRunId`, `reason`, `recordedAt`, `kernelError`, `remedy`,
  `transient`, `resolution`. `divergentInput` extends to `payload` for
  `replay-divergence`, so the member that says *what* disagrees is populated
  for four of the five; `flow-run-superseded` leaves it `null` because nothing
  diverged — the run was retired by decision, and its successor is named
  instead. The rename is on the wire: divergence's `expectedHash` and
  `expectedLabel` are now `currentHash` and `currentLabel`, the same names the
  identity pins already used for "what this runner computed now".

  One shared constructor in `tally-flow` builds the map for every site, and
  `ClientError::into_flow` completes it for any refusal that reaches the runner
  from somewhere else, so a bare code with no details still lands on the
  documented shape rather than a thinner one. Completion fills, it never
  invents: a refusal that named no run reports `flowRunId: null` — an empty
  string is the same fact written differently and renders the same way — and
  `remedy` is `null` with it, because `tally flow supersede --flow-run-id`
  with nothing to put after it is not a command an operator can run. The five codes keep their names,
  their semantics, their exit code, and the message text `tally flow run`
  renders; `ordinal` remains a top-level field, present exactly when a node is
  implicated. `flow-run-superseded` has no mid-run site and `replay-divergence`
  no startup site — lineage is read once by the startup `inspect_run` scan, and
  a payload cannot diverge before an ordinal exists — and the contract test
  says so in place of asserting a site that cannot happen.

- **Flow-run membership is now a durable admission fact, so a node a run
  attached to or reused is visible in that run's own window (#380, W-316).**
  Membership used to be recomputed on every query by scanning durable rows and
  witness records for an orchestration capsule naming the run. Three admissions
  write no row of their own — `attached`, and full-mode `reused` and `terminal`
  — and each hands the caller a task UUID for work that is real and running
  while the row, and therefore the scanned membership, stays with whichever run
  created it. A re-triggered campaign that attached to nodes still in flight
  from its previous run got a `query log --flow-run` window that showed the same
  items forever, with `nextCursor: null` and nothing elided, while the work ran.
  No page cap was involved, so none of the truncation machinery fired. That is
  the #247 report, made legible by #316/#354 and now repaired at the root.

  Every admission carrying an orchestration capsule appends
  `{schemaVersion, flowRunId, taskUuid, disposition, nodeOrdinal?, nodeLabel?,
  recordedAt}` to a new durable ledger, `<data-dir>/flow-membership.jsonl`, and
  fsyncs it **before the admission is acknowledged** — for all five
  dispositions. A `conflict` admits nothing and therefore records nothing, which
  is asserted rather than assumed. `nodeOrdinal` and `nodeLabel` are the
  *submitting* run's, which for a row-less admission is the only place they are
  written down at all.

  `query log --flow-run`, `query jobs --flow-run`, `query run`, and
  `query proof --flow-run` resolve a run to the **union** of that ledger and the
  original scan, so nothing regresses across an upgrade: a run whose rows were
  written before the ledger existed still resolves exactly as it did, from its
  rows. Removing the ledger restores the pre-#380 answer node for node.

  The enqueue kernel is unchanged. The two-part key (`dedupKey` identity ×
  `payloadHash` work-equality) resolves to the same five dispositions with the
  same evidence-probed reuse; membership is recorded by a wrapper that never
  inspects the key and cannot return a different disposition than the kernel
  decided. An admission carrying no capsule takes none of this path and does not
  create the file. The flow-node cap still counts durable rows, so attaching to
  another run's node does not consume a node of the run that attached.

  `query jobs` now also reports `flowRunTasks` when a `flowRun` filter was
  supplied, matching `query log`.

  Operator-visible consequences: `flowRunTasks: 0` means *the daemon holds no
  membership for that run ID*. That is narrower than "the run admitted nothing",
  and the difference is the point: the commonest cause is a mistyped or stale ID,
  but a repaired or deleted ledger, a compacted-out idle run, and an admission
  that reported `membershipDegraded` all produce a zero for a run that did admit
  work. The CLI's stderr notice names all of them rather than closing the
  question. No row field changed, so no row migration is required.

  A damaged or unwritable ledger is checked **before** the kernel commits, so a
  flow admission is refused outright — no durable row, no `enqueued` lifecycle
  event, no dispatch — with `resolution: repair-flow-membership-ledger`. Which
  task UUID a run is handed is not known until the admission has been decided,
  so the write itself necessarily follows the commit; a ledger that becomes
  unusable in that window yields an **acknowledged** admission carrying a
  `membershipDegraded` object (and a journalled `flow-membership-degraded` line)
  rather than a denial, because telling a caller its admission failed while the
  node dispatches and runs would orphan live work. Every client that admits —
  `tally enqueue`, `queue continue`, `adapter smoke`, `campaign`, and the live
  flow runner, which is the path that produces flow-run membership at scale —
  prints that warning on stderr, so the operator who caused the degradation
  learns about it where they caused it rather than by grepping the journal. An interrupted append (torn
  final line) is skipped on read and truncated on the next append; a record
  written by a *newer* daemon — unknown field, unknown disposition, higher
  `schemaVersion` — is read on the fields this daemon understands, so a pin
  rollback cannot take run-scoped queries out.

  The ledger is compacted past 20,000 records — one per admitted flow node —
  down to 18,000, dropping whole runs **least-recently-touched** first. Never
  part of a run, and never a run holding an executing task or the run whose
  record is being written: keying eviction on a run's *first* record would make
  it anti-correlated with liveness, deleting the membership of exactly the
  campaigns still under observation, and a compaction that evicted its own
  caller's run would report a durable membership that is not there. "Live" here
  means every job that has not completed — running, queued, **or paused** — so a
  queue an operator has paused keeps its membership. If nothing is evictable the
  ledger exceeds its target rather than deleting membership in use: it grows by
  one record per flow admission, says so on the daemon journal every time
  (naming `queue resume` as the drain, since a paused queue does not finish on
  its own), and compacts on the next admission after that work completes. The bound is sized by the one-time
  parse (~200 ms at 20,000) rather than copied from the rare-event lineage
  ledger, and the low-water mark means a compaction is followed by thousands of
  ordinary appends instead of another compaction. Compaction is a
  write-and-rename, so a crash mid-rewrite cannot leave a silently smaller run
  set, and it re-emits a newer daemon's unknown fields and disposition values
  verbatim rather than stripping them. Per-admission cost is flat in ledger
  size: 2.13 / 1.91 / 2.16 / 2.02 ms across ledgers of 0, 5,000, 20,000, and
  25,000 records (debug profile; `membership_admission_cost_sweep`). The flatness
  is the claim, not the constant — absolute numbers move with host load, and at
  an empty ledger the figure is indistinguishable from the pre-#380 path because
  at zero records there was nothing to improve. What was removed is the growth:
  the same sweep against the first draft read 2.8 ms empty, 17.4 ms at 10,000,
  77.5 ms at 50,000, and 977 ms past the bound.

- **The systemd watchdog keepalive no longer shares the dispatch loop, so a
  busy daemon is no longer killed for being busy (#370).** `WATCHDOG=1` was
  emitted from a `tokio::select!` arm in the daemon's dispatch loop. A
  `select!` arm is only polled when the loop comes back around to poll it, and
  it does not come back around while another arm's *body* is awaiting — a
  terminal transaction, a lifecycle compaction, a witness fsync under an
  estate-sized context. One slow body therefore held the keepalive for as long
  as it ran, and at thirty seconds systemd sent `SIGABRT`. That is the
  2026-07-30 00:01–00:03 sequence in the coordinator journal: four
  `Watchdog timeout (limit 30s)!` kills in three minutes, of a daemon that was
  working.

  The keepalive now runs on a dedicated OS thread (`tally-watchdog`) that holds
  no daemon state and takes no daemon lock, so nothing the daemon does can
  delay the datagram. It is not thereby licensed to lie: the thread never
  speaks for itself, and pings only while the dispatch loop has come back
  around within its headroom, which the loop stamps before every `select!`. The
  100 ms lease tick is what makes that meaningful — a healthy loop stamps at
  10 Hz even with nothing to do, so staleness means *stuck*, not *idle*.

  The headroom is `10 × WatchdogSec`, and it is the same number whether the arm
  body is parked on an `await` or blocked in a syscall. That matters more than
  it sounds: the runtime is single-threaded, and the expensive part of a
  terminal witness append or a lifecycle compaction is `flock` / `write_all` /
  `sync_all`, not an `await`. A liveness witness stamped by a runtime task
  would stop for exactly those calls, so it would have been the tighter bound
  in precisely the case that needs the looser one — the daemon would have got
  roughly one service period of headroom for its slowest synchronous work while
  being documented as having ten. One witness, stamped by the loop, avoids
  that.

  Past the headroom the keepalive falls silent, says so on stderr, and
  systemd's own timer runs to completion, so a wedged daemon is still killed —
  at `11 × WatchdogSec` rather than `1 ×`. That window is not silent: an
  overdue loop is reported from `2 × WatchdogSec` onward, every two periods,
  while the keepalive is still standing for it. Both nix modules now carry the
  derivation next to `WatchdogSec = "30s"`, and a test pins what those divisors
  come to at that value.

- **The orphaned-projection sweep no longer declares a delivered projection
  lost, and retracts the records that said so (#372 repair).** The startup
  sweep decided orphan-ness from `config.producers` alone. It never consulted
  the `producers/gh-completed/` idempotency marker — the durable proof that a
  projection reached the forge — so on the first start after a producer was
  retired it named *every* completed GitHub row of that producer still in the
  recovery plan, back to the `events/done` horizon, as "the forge-side
  projection is lost". The live storage-warning path had the same inversion:
  `post_storage_warning_once` resolved the producer on its first line, above
  its own marker check, so a receipt already on the forge was orphaned too.

  This was wrong in the reassuring direction — the operator was told more was
  lost than was lost — and it wrote the false claim to the strongest surface in
  the tree, as a hash-chained `projection-orphaned` attestation carrying
  `retryAuthority: "terminal-no-retry"`. The live completion path never had the
  bug: `complete_gh_once_with_completion` reads the marker before it resolves
  the producer. Both paths now take that ordering, through one shared marker
  lookup. A record written under the old reading is withdrawn on the first
  start after this change, and because an append-only chain cannot be edited,
  the claim it stood on is answered with a `projection-orphan-retracted`
  attestation naming the same identity rather than quietly dropped.

  Three consequences of the same repair:

  - Whether a claim has been witnessed is now decided by the attestation chain
    rather than by the presence of the record file. That closes two states the
    old flag could not reach: a record written by an observation that died
    before it could append was never witnessed by any later one, and a record
    collected by retention and re-derived on a later start would have been
    witnessed twice.
  - `producers/gh-orphaned/` joins `PRODUCER_MARKER_DIRECTORIES`, so `tally gc`
    collects it at `retention.producerMarkerHorizon` like every other
    per-dispatch `producers/` set. A record can only reach that age after the
    acknowledged event it describes has left `events/done`, and a collected
    record therefore does not come back. The Nix option documentation and the
    retention table name the fifth directory.
  - A producer *replaced* by one of another kind under the same name is
    terminal too. `KindMismatch` says exactly what `UnknownProducer` says —
    this configuration cannot produce a GitHub projection for this origin, and
    only an operator edit changes a configuration — so it took the same
    terminal outcome instead of the same forever-retry the issue was filed
    about. `Mutation`, `Io`, and torn-marker failures remain retryable.

- **`tally producer orphaned` and the startup report no longer go silent on one
  unreadable record (#372 repair).** Both read the whole directory and stop at
  the first file they cannot parse, discarding what they had already read, so a
  single record from a newer schema — the realistic trigger is a package
  rollback — hid every readable record and made the command the daemon's own
  log line advertises exit 1. The pass now accumulates: unusable files are
  reported by path and reason beside the usable ones, in the report and under
  an `unreadable` key in the JSON, which is the discipline `UnitFactFailures`
  already established for the recovery sweep.

- **A post-ack forge projection whose producer has been removed is now terminal
  instead of retrying forever (#372).** Removing a producer block is documented
  operator work — the wave-3 close-out instructed exactly that for a retired
  campaign — but the projections admitted under it had no defined fate. Their
  worker resolved the producer from the effective configuration, failed with
  `unknown producer "<name>"`, and retried at a one-minute ceiling
  indefinitely: five completed tasks on the coordinator estate produced 170 log
  lines in 30 minutes and would have produced them until the daemon was
  restarted, and then again after it.

  Nothing was ever in doubt about those tasks. The completion is settled and
  witnessed; only the forge-side projection is owed, and it can never be paid
  while the producer is gone. That is now said once, in a defined state:
  `unknown producer` yields a terminal `projection-orphaned` outcome, recorded
  under `<stateDir>/producers/gh-orphaned/` and witnessed once on the advisory
  attestation chain. Every other failure — a forge outage, a rate limit — is
  unchanged and still retried. Storage-warning receipts, orphaned by the same
  removal, take the same path.

  The set is reported in one pass at daemon start rather than discovered one
  line per projection per minute, and `tally producer orphaned --state-dir
  <PATH>` lists it at any time. That command reads the state directory alone,
  because the situation it describes is precisely one in which the
  configuration no longer names the producer. Nothing consults the records to
  decide what to do: the population is re-derived from the configuration on
  every start, so restoring a mistakenly removed producer block projects the
  completion after all and each stale record clears itself when its projection
  settles.

- **The unit-exit migration no longer advertises a repair it cannot perform, and
  no longer answers "clean" for a directory holding no durable rows
  (#371 repair).** Both defects had the same effect: an operator follows the
  documented command, is told everything is fine, restarts, and crash-loops
  again with no signal.

  The refusal, the migration's own skip reason, `operating/recovery.md`,
  `operating/cli.md`, and the upgrade note above all said that a record owned by
  a remote executor should be migrated by running the same command against that
  worker's state directory. That command is a guaranteed no-op there. The
  labeled name is derived from the durable rows, and those exist only on the
  coordinator — a worker runs no tally daemon and has no `events/` — so the
  invocation reads zero rows, rewrites nothing, and exits 0 with an empty
  report. For remote-owned rows that was worse than saying nothing: it retired
  the by-hand repair with a command that reports success. Every one of those
  five claims is retracted. The migration now states plainly that it repairs
  coordinator records only, and each remote-owned row is reported with the facts
  the hand repair needs — `executor`, `recordPath` (resolved from the
  coordinator's `executors.<name>.stateDir`), `preLabelUnit`, and
  `expectedUnit` — so nothing has to be rediscovered from the source. The
  startup refusal carries the same caveat and counts the affected rows.

  A `--state-dir` naming a directory that is not a coordinator's state tree —
  a typo, or a worker's — read as zero acknowledged rows and reported clean.
  Both that directory and its `events/` must now exist, or the command fails
  before doing anything. `--config` is now read for the `executors` map that
  names remote records; it does not and cannot select a state directory,
  because tally's configuration has no such key, and `cli.md` said otherwise.
  That documentation now matches what `tally gc` three sections earlier already
  said: without `--state-dir` the CLI resolves `$XDG_STATE_HOME/tally`, which is
  not the NixOS module's `/var/lib/tally/state`.

  The refusal, `cli.md`, `recovery.md`, and the upgrade note now also say which
  user to run as. Exit records are written mode 0600 and nothing repairs
  ownership afterwards, so a record rewritten under `sudo` on a NixOS
  deployment is one the service user can no longer read — trading a name
  mismatch for a permission failure.

  `troubleshooting.md` now records that `tally flow supersede` is refused with
  `flow-lineage-conflict` while the run still has unfinished nodes, so an
  in-flight run — the population an upgrade actually strands — needs
  `tally flow cancel` first. The remedy the error prints is the second of two
  steps, not the only one.

- **A pin advance no longer crash-loops the daemon on pre-label unit-exit
  records, and recovery no longer hides the population behind one restart per
  record (#371).** Campaign task labels entered the execution unit name, so a
  row whose orchestration carries a `taskRef` now owns
  `tally-job-<campaign>-<task>-<uuid>.service` where it previously owned
  `tally-job-<uuid>.service`. `UnitExitRecord::validate` compares that name byte
  for byte, so every `unit-exit/<uuid>.json` written by an earlier binary for a
  campaign task refused startup — and because collection died on the *first*
  invalid record, an operator discovered the next one only by paying another
  ~25 s restart. On the host that hit this, 23 of 6,985 acknowledged events
  carried a `taskRef` and each one wedged startup in sequence.

  Collection now probes every acknowledged row before raising anything and
  reports all unusable records in one pass, each naming its row, its executor,
  the unit that was expected, and the record's own name. Records that are
  exactly the pre-label rename are marked as such and the refusal names the
  one-shot forward migration that clears them; a mismatch of any other shape is
  reported without being advertised as migratable, because guessing at a name
  is how a migration renames a record into something recovery still refuses.

  Strict validation is unchanged and no shim accepts the old name. The new
  `tally migrate unit-exit-labels --state-dir PATH [--apply]` rewrites the
  `unit` field of pre-label records to the name the current derivation
  produces, deriving both halves of the rename from the same
  `row_execution_identity` recovery derives its expectation from. Without
  `--apply` it prints the plan as JSON and writes nothing. It is idempotent,
  touches only records whose recorded name is exactly the pre-label name for
  their own row, lists everything else under `skipped` with a reason, and leaves
  `invocationId`, `attempt`, `leaseEpoch`, `serviceResult`, and the exit
  metadata untouched. The witness ledger is neither read nor written. No backup
  copy is kept, because the pre-label name is a pure function of the record's
  own file name and a copy would carry nothing the surviving file does not.

  **Upgrade note:** if the daemon refuses to start after an upgrade with
  `executor fact collection failed: N acknowledged row(s) have unusable local
  execution facts` and `[pre-label unit-exit record]`, run
  `tally migrate unit-exit-labels --state-dir <STATE_DIR>` to review the plan,
  then the same command with `--apply`, then start the daemon. Copy
  `<STATE_DIR>` from the refusal, and run the command as the user that owns that
  directory — under the shipped systemd units that is the service user, not
  root. Rows dispatched to a remote executor cannot be repaired by this command
  on either host; they are listed under `skipped` with the record's path on the
  owning worker and the exact name to write, and must be rewritten there by
  hand. Deleting the records instead removes the evidence recovery uses to
  decide whether replay is safe.

- **A flow run recorded by an older binary is no longer refused with nothing but
  two hashes (#371).** `argsHash` pins the bytes the runner serialized, not a
  canonical form of the logical value, so moving the runner's arguments off argv
  and into the brief file changed the hash for arguments nobody edited: four
  in-flight runs on one estate failed `args-changed-mid-run` where `jq -c` of
  the unchanged arguments file reproduced the *current* hash exactly, and the
  24/7 drain sat behind its consecutive-failure fuse until the pinned run ids
  were rotated by hand. `resolution: "supersede"` told a supervisor the class of
  operation; it never told a person which command to type.

  All three `*-changed-mid-run` refusals — at startup and mid-run alike — now
  say that the pin covers the exact serialized bytes and that a run recorded by
  an earlier tally can therefore be refused for an input it never changed, and
  end with the `tally flow supersede` invocation that retires the run, with the
  matching `--reason`. The same string is available to machines as a new
  `remedy` detail. `transient`, `resolution`, and `divergentInput` are
  unchanged, so nothing branching on the existing contract moves.

  There is deliberately no migration on this side and there cannot be one:
  re-deriving the recorded hash from the current arguments is the same operation
  as dropping the pin, and the pin exists because only the operator can attest
  the arguments are unchanged. Both halves of this fix therefore ship the same
  policy — strict validation stays, and the refusal names one documented
  command.

  **Upgrade note:** every operator with a long-running flow hits this on the
  next advance that moves how arguments are serialized. An in-flight run started
  before the upgrade must be retired and restarted as a successor:
  `tally flow supersede --flow-run-id <OLD> --new-flow-run-id <FRESH-UUID>
  --reason args-changed`. Persist the successor UUID before calling —
  idempotency is keyed on the whole triple, so minting a fresh UUID per attempt
  records a different rollover.

- **Three post-merge repairs on the campaigns docs batch (#319 repair).** The
  kind-less gate fixture #319 added was also concatenated into the Home Manager
  fixture whose whole job is to fail *as an assertion*. Because `kind` is an
  enum with no default, forcing that gate throws at the option system before
  Home Manager's assertion machinery runs, so `invalidCampaignHome` started
  failing for the missing field instead of for the two gate assertions the check
  is named after — leaving `assert !invalidCampaignAttempt.success` green even
  if the campaign-gate assertions were unwired from the module entirely. That
  fixture is back on the field-fixture list alone; `missingKindCampaignAttempt`
  and its `kind = "command"` control keep the kind-less coverage, and the Home
  Manager activation once again fails with `tally campaign gate … fields must
  agree with kind` and the `'**'`-component message.

  The continuation passages named three of the five conditions that write a
  continuation event. The flow writes one when a task merged, a checkpoint
  passed, machine steering was published, **a machinery retry was posted, or a
  checkpoint deferred** — and the last two are exactly the passes that produced
  no completion, so a reader could conclude that a lane which only faulted stops
  the campaign, the opposite of the behaviour. All four prose sites now match
  the flow's own `advanced` predicate and the file's pseudocode.

  `services.tally.campaigns.<name>.mention` is a back-compat contract this
  changelog declared load-bearing and nothing checked. `checks.campaign-render`
  now pins it, so a silent change to the shipped default is caught and retiring
  it stays a deliberate release-boundary decision. The pin is the digest of the
  rendered default rather than the literal, because `grep -rn "@tally" doc/
  flake.nix` returning nothing is an acceptance criterion of #319 and that file
  is in its scope; the digest moves if and only if the default does, and the
  failure message says what to do either way.

- **The campaign mention example no longer at-mentions a real, unrelated GitHub
  account (#246).** `mention = "@tally build"` was the copyable literal in
  `doc/src/flows/campaigns.md` and in the shipped flake fixtures. tally matches
  that token literally, but the comment carrying it is a real comment on a real
  issue, so GitHub resolved `@tally` — an account with nothing to do with this
  project — and notified it on every trigger of every campaign that copied the
  block. The documented example is now `@<your-login> build` with an explicit
  warning that the token is a live GitHub mention, that it must never name a
  third party, and that naming your own login is what composes with
  `allowSelfTriggered` and `allowedActors`. The flake's campaign fixture and
  its intake event fixtures move in lockstep to `@operator build`, matching the
  `operator` trigger actor those events already declare, and the fixture that
  exercises the shipped defaults now reads its trigger grammar back out of the
  rendered config instead of repeating a mention literal.

  **Migration:** `services.tally.campaigns.<name>.mention` still *defaults* to
  `@tally build`, because changing it would silently retire the trigger grammar
  of every deployed campaign that relies on the default. Set it explicitly to
  your own login — or to a token with no `@` at all, which is an equally valid
  trigger grammar — on every campaign. The option's own description now says
  so, and the same warning was added to the generic
  `producers.<name>.triggers.mentions` example.

- **The campaign gate `kind` migration is named, and an omitted `kind` is now a
  fixture (#276 finding 3).** The entry recording that gate kinds "are now
  explicit" never said that `kind` is a *required* field on every entry of
  `services.tally.campaigns.<name>.gates`, with no default, so an out-of-repo
  configuration written before the change fails evaluation with `The option
  '…gates."[definition 1-entry 1]".kind' was accessed but has no value defined`
  and nothing pointed at the cause.

  **Migration:** add `kind = "command"` to every gate that declares
  `preflightArgv`/`argv` and `kind = "forbidPaths"` to every gate that declares
  `forbidPaths`; the two field sets may not be mixed. The refusal is now
  proved rather than asserted in prose: `checks.campaign-gates-rejected` carries
  a gate that omits `kind`, requires its evaluation to fail, and requires the
  byte-identical gate with `kind = "command"` supplied to evaluate — so the
  failure means the missing field and not some other defect of the fixture.
  Eval-only; no flow node, and `campaignMaxNodes`/`max_flow_nodes` is untouched
  at 52.

- **Campaign docs no longer describe the machine self-continuation as a GitHub
  comment (#306 follow-up).** A campaign's next-pass nudge became a JSON drop in
  the shipped events directory, but `campaigns.md` still documented a
  "pass-continuation producer" that "matches only the exact continuation
  command", listed the continuation among the receipts a split campaign's
  `issueRepository` carries, and said the projection switches were inherited by
  a second producer that no longer exists. Three option descriptions carried the
  same stale mechanism — `allowSelfTriggered` pointed at the deleted producer,
  `campaigns.<name>.runtimeMaxSec` said continuation "lives in marked pull
  requests and issue comments", and `campaignPoll.enable` called the timer the
  continuation mechanism rather than the recovery path for a lost event. The
  `campaign-timer-doc-drift` check's own rationale explained itself in terms of
  the deleted `/tally reconcile` comment, which is exactly the kind of thing the
  next reader trusts.

- **`campaigns.md` now states how a forge-native checkpoint receipt binds
  (#297 finding 5).** That finding recorded checkpoints as silently unavailable
  to forge-native campaigns; #295 typed them end to end instead, so what was
  missing was the receipt contract. A new section states that the receipt
  identity is `<task>-<source digest>/<base revision>`, that the digest half is
  the admitted executable-graph digest a forge-native pass refuses to run
  without, that the revision half is re-resolved from the freshly fetched base
  on every pass, and what each half moving does: a moved base re-executes the
  checkpoint at the new revision, a re-armed graph edit invalidates every
  receipt at once, and base movement *during* a checkpoint publishes the
  truthful receipt and then fails the lane.

- **The TaskChampion projection is out of the book.** The removal and its
  migration note stay in this file, which is where a reader migrating a
  version-2 `query.storage` consumer should look; the mentions that survived
  across `doc/src/` are now written in terms of what remains — the inert
  `taskdata/` directories an operator may reclaim, and the section
  `schemaVersion` 3 no longer carries.

- **The preflight witness no longer mutates the base a later gate's probe is
  judged against (#320 repair).** #320 ran each command gate's non-gating
  real-`argv` witness immediately after that gate's own probe, so for two or more
  command gates the order was `probe(g1) → argv(g1) → probe(g2)` on one shared
  worktree. A gate's `argv` is the merge criterion — the one command the design
  expects to build and write — so gate 2's base-safe probe was judged against a
  base gate 1 had already changed. A probe that asserts its own gate's output is
  absent on the base, which is the shape this repository's own examples teach,
  then went red because of an unrelated gate and the campaign refused admission
  naming the innocent gate, on every pass, forever: no agent is ever dispatched,
  so the first merge that would end preflight can never happen.

  Every probe now runs first, on the genuinely pristine base; the witnesses
  follow only if all of them passed, in declaration order. The live fixture is
  armed rather than incidentally green: the witness branch of the fixture gate
  writes a marker into the lane and the fixture's probe asserts that marker is
  absent, so the interleaved ordering turns the second gate's probe red. The
  submission order is asserted directly as well.

  Three integrity repairs ride along. `checks.campaign-preflight-probe-drift`
  grepped one exact spelling and was therefore green while the file it guards
  shipped two no-op probes it could not see; it now reads every `preflightArgv`
  declaration on the Nix surface and refuses both the cannot-fail family (`true`,
  `/bin/true`, `/usr/bin/true`, `:`, and the `sh -c` forms) and probes that are a
  single bare existence test, with an explicit `no-op-probe-allowed:` opt-out for
  fixtures that exist to be rejected. The `argv`, `preflightArgv`, `runtimeMaxSec`
  and `gates` option descriptions now state that a command gate's `argv` also
  executes once on the preflight lane before any agent — the declarative
  operator's contract said post-agent only — and point a criterion that must
  never see an unbuilt base at a checkpoint node instead. The four campaign
  fixtures that #320 left probing for the existence of `/bin/true` now carry the
  same representative probe-and-gate pair as the reference fixture.

- **Six post-merge repairs on the NixOS campaign surface (#303 repair).** The
  poll service ordered itself after `network-online.target` without wanting it,
  which orders against nothing: nixpkgs warned on every evaluation of such a
  host, and the timer's first tick 15 s into a boot could run its authenticated
  forge read before the network was up and leave a failed unit behind. It now
  `wants` that target — and refuses to start at all until the forge identity
  exists, so a host whose secret is not provisioned yet skips the scan instead
  of failing a unit every tick.

  The identity writer now also emits `~/.config/gh/config.yml` (`version: "1"`,
  mode `0600`). That is the file `gh` writes for itself on first use, failing
  the whole call when it cannot, and writing it at activation makes the home
  read-only-safe for every consumer — including the `shell` adapter that runs a
  campaign's own self-continuation, which never had the home in its writable
  paths. The existing writable-home allowances stay for self-healing; nothing
  requires them after a successful activation.

  Teardown, ordering, and diagnosis: turning `campaignForge.enable` off now
  removes the identity files it wrote, keyed on a marker so a `homeDir` pointed
  at a pre-existing home keeps its own `.gitconfig`; the activation snippet is
  ordered after `setupSecrets`/`agenixInstall` when the estate runs sops-nix or
  agenix, rather than relying on names sorting favourably; and a missing or
  unreadable `tokenFile` now fails with a message naming the option instead of a
  bare shell redirection error.

  Guard and doc gaps: `checks.campaign-render` now holds the NixOS poll program
  to the same `--once`/no-`--wait` contract as the Home Manager one and pins its
  `--config`, `--socket`, and `--state-dir`, so the registry the timer scans
  cannot silently drift from the one an interactive `arm` writes. The worked
  `campaign arm` example gains `--allow-actor`, which the bot identity makes
  mandatory rather than optional, and `faq.md` and `fleet-deployment.md` no
  longer state that the NixOS module renders no campaign units.

- **Repaired the seven post-merge findings on the flow-lineage successor path
  (#251 repair).** The mechanism shipped correct for the states its tests
  constructed; the defects were in the states they did not — a predecessor that
  does not exist, a run ID written in a different but still valid UUID
  rendering, and a ledger whose last line is torn.

  **A rollover must name a real run, in one canonical rendering.**
  `flow.supersede` validated only that both IDs *parsed* as UUIDs and then
  stored the caller's raw spelling as the ledger key, so an invented or
  mis-rendered predecessor answered `ok: true, disposition: "recorded"` and
  recovered nothing for the run actually being replayed — the silent no-op the
  whole feature exists to eliminate, and irreversible because the successor UUID
  was then durably burned. Both IDs are now canonicalized to hyphenated
  lowercase on every write *and* every lookup, including `query.lineage`,
  `query.run`, and the runner's own startup scan, so the upper-case,
  unhyphenated, and braced renderings all name one run; records written by the
  previous build in another rendering are absorbed by the same canonicalization
  on read, so nothing needs migrating. A predecessor with no durable node, or
  with no recorded `orchestration.scriptHash`, is refused as `not_found` — such
  a run can never trip an identity pin, so it can never need retiring — and a
  predecessor whose rows disagree about a pinned hash is refused as
  `flow-lineage-conflict` rather than recorded with an arbitrary one. The
  documented predecessor hashes are therefore never silently omitted.

  **One torn line no longer stops every flow run in the estate.** Every flow
  start reads the lineage index, and `FlowLineage::read` failed the whole read
  on the first unusable line, so an interrupted append — a crash, a power loss,
  or a short write under ENOSPC, which the #251 incident's own low-disk
  condition makes concrete — blocked runs that had no rollover at all, and
  bricked the recovery operation itself. An unterminated final record is now
  skipped on read and truncated by the next write, following
  `truncate_incomplete_attestation_tail`. A *complete* record that cannot be
  decoded still fails closed, deliberately: skipping it could resurrect a run an
  operator durably retired. That failure now carries the new
  `flow-lineage-unusable` wire code with `transient: false` and
  `resolution: "repair-lineage-ledger"`, and the troubleshooting chapter
  documents the one-line repair, so it can never strand a supervisor as an
  anonymous internal fault.

  **The lineage store is bounded, cached, and inventoried.**
  `<dataDir>/flow-lineage.jsonl` was unbounded, absent from the retention
  inventory, and re-parsed in full on every flow start — measured at ~1.1 s per
  start against a 160,000-record file, on the machine whose ten-minute
  supervisor cadence is the point of the issue. The daemon now caches the parsed
  index and revalidates it against the file's length and mtime, so a hand repair
  is still picked up without a restart, and the ledger keeps its newest 100,000
  records, compacting through an atomic rewrite on the append that would cross
  the bound — the `changes.jsonl` count-bound shape, safe here because this
  store is an index and not a proof chain. It now appears in
  `retention.md`'s "What still grows" table, and is created `0600` like its
  sibling ledgers instead of world-readable.

  **The `transient`/`resolution` contract covers every classified code, on both
  paths.** It previously reached three of the four fatal replay codes and only
  when raised at startup, so `replay-divergence` and every mid-run identity
  refusal handed back by the daemon carried no facts at all, and a supervisor
  written to the documented recipe would retry them forever. One table now
  stamps the pair wherever a classified error is constructed — startup pins,
  mid-run daemon refusals, and the client's own translations — so one wire code
  never has two `details` contracts. `replay-divergence` and the three
  `*-history-conflict` codes resolve to `investigate` (a rollover does not clear
  them); the transient daemon codes carry `transient: true, resolution: "retry"`.
  `errors.md` states that absence of the pair means unclassified, not transient.

  **Documentation matches the behaviour.** `errors.md`'s automation recipe is
  now the one that works: persist the successor UUID before calling (a fresh
  UUID per attempt is a conflict, not a retry), read `query.lineage` and adopt
  `supersededBy` on `flow-lineage-conflict`, and cancel a live predecessor
  first. `submission-and-replay.md` states the scope of the replay refusal that
  the merged pull request promised and did not include: it is runner-side and
  startup-only, is not an admission-time prohibition, and does not stop a
  runner already in flight. `query.lineage` validates its run ID like
  `flow.supersede` does instead of answering any string with a well-formed
  "not superseded" view.

### Added

- **The real gate argv is witnessed at t=0, without gating (#320).** Since the
  preflight/post-change split, a campaign's actual merge criterion — a command
  gate's `argv` — first executed only after the first agent cycle. What ran at
  t=0 was the `preflightArgv` proxy, declared base-safe and never validated to be
  representative. The pristine-base preflight lane now runs every command gate's
  base-safe probe first and then, only if all of them passed, each gate's real
  `argv` once as a non-gating `preflight-witness-<id>` node: same lane, same
  `CAMPAIGN_TASK_ID`, same deadline, same `taskRef`, no `exit:0` evidence,
  verdict discarded. A run never fails because of it, so a base that is
  legitimately red until an agent builds something stays tolerated — but the exit
  code and stderr of the exact argv on the exact host land in the witness record
  and the capture files before the first agent cycle rather than after it. A red
  probe stops the pass and nothing is witnessed.

  The pass node budget grows accordingly: `campaignMaxNodes` and the CLI's
  independent `max_flow_nodes` now reserve `2 + 2 × commandGateCount` preflight
  nodes instead of `2 + commandGateCount`, moving the pinned budget for the
  reference campaign shape from 51 to 52 at every assertion site. Existing
  campaigns need no configuration change; `maxNodes` is computed.

  Three probe-hygiene repairs ship with it. The copyable documentation examples
  probed with `cargo --version` / `cargo fmt --version` — precisely what the
  document's own warning calls insufficient — and now probe the compiler driver,
  the offline workspace manifest, and rustfmt actually formatting something. The
  shipped Nix fixtures no longer preflight with a command that cannot fail, and a
  new evaluated check reads every probe on that surface and refuses the ones that
  prove nothing. And the
  campaign documentation now records the one preflight residue an operator can
  observe — a runner killed mid-preflight leaves its `_campaign-preflight`
  worktree and branch — together with its recovery path, which is the next
  pass's sweep and never a manual `git worktree remove`.

- **Campaigns on the NixOS module, identity first (#303).** The system module
  deployed the daemon and asserted the campaign surface away, so a forge-native
  campaign armed against a host with no user session had no pools, no driver
  adapter, and nothing to dispatch into. `services.tally.campaignForge.enable`
  now renders that whole execution surface in one switch — the `campaign`,
  `campaign-agent`, `campaign-control`, and `flow` pools, the packaged
  spec-build driver adapter, the fanout floor, the `campaign-continuation`
  registry entry, and `tally-campaign-poll.service` with its timer as system
  units. Off by default: a poll timer without the surface beneath it would fire
  on schedule and fail every tick, which is worse than absent.

  The identity is the substance. Home Manager campaigns inherit the operator's
  own authenticated `gh` and `git`; a system service account has neither, and
  every campaign job — the driver's pull requests and merges, the agent's
  commits, the poll scan — runs as that account in its own user manager, where
  a unit environment does not reach and the shipped driver reads no
  `LoadCredential`. So the account gets a real home
  (`campaignForge.homeDir`, `/var/lib/tally/forge` by default) and activation
  materialises exactly two `0600` files in it: a `gh` hosts file holding the
  declared `login` and the token read from `campaignForge.tokenFile`, and a
  `.gitconfig` binding the commit identity and a `gh auth git-credential`
  helper. The token is piped to the identity writer on standard input, so it is
  never a program argument and never enters the Nix store, and the file it is
  read from needs no particular ownership. Enabling the surface without a login
  or a token file is refused at evaluation.

  Declared `services.tally.campaigns` stay Home Manager only and the assertion
  still refuses them, now saying why: they are driven by a managed GitHub
  producer unit, and this module renders no producer units. The one entry it
  does carry, `campaign-continuation`, renders no unit anywhere — `tally-drain`
  already drains that directory — and the daemon now creates
  `<stateDir>/events` on both modules alike.

- **A witnessed successor path for fatal replay divergence (#251).** Replay
  identity refusals are correct, but a refusal alone was not a recovery: a
  supervised runner that persists one `flowRunId` per work item and retries it
  across Nix/Home Manager activations could only re-observe
  `script-changed-mid-run` or `args-changed-mid-run` forever, because the old
  generation's script or `args.tools` store path no longer existed and nothing
  durable said the run had been abandoned. Three such items adjacent in a
  worklist tripped a supervisor's failure fuse on every pass and starved
  thousands of independent later items.

  `tally flow supersede --flow-run-id OLD --new-flow-run-id NEW --reason
  generation-change` (RPC `flow.supersede`) records that transition durably in
  `<dataDir>/flow-lineage.jsonl`. The old run is preserved unchanged — same
  rows, witnesses, and history; the successor is not created and inherits
  nothing, so reusing application artifacts across the boundary remains the
  consumer's concern. Reasons are closed (`generation-change`, `script-changed`,
  `args-changed`, `catalog-changed`, `operator`), and the daemon records the
  abandoned generation's own script/args/catalog hashes from its rows rather
  than trusting the caller. Repeating the identical call answers
  `disposition: "reused"` and writes nothing, so an unattended supervisor may
  issue it, crash, and issue it again. Contradictions fail closed with the new
  `flow-lineage-conflict` wire code: a second different successor, a successor
  already claimed by another predecessor, a rollover that would close a cycle, a
  predecessor with unfinished nodes, or a successor that already started.

  Replaying a superseded ID is refused before any hash comparison with the new
  `flow-run-superseded` code (exit 20), which names `successorFlowRunId`, the
  reason, and the timestamp. The three startup identity pins now also carry
  `flowRunId`, `divergentInput`, `transient: false`, and `resolution`, so a
  supervisor distinguishes a permanent identity refusal from a transient daemon
  or transport failure without parsing prose.

  New `query.lineage` / `tally query lineage RUN` reports `superseded`,
  `supersededBy`, `supersedes`, the whole `chain` oldest-first, and
  `currentFlowRunId`; a run with no rollover answers an empty lineage rather
  than `not_found`. `query.run` gained `supersededBy`/`supersedes` and the new
  `superseded` state, which the human rendering prints above the task board.

### Fixed

- Repaired the wave-3 residue sweep (#332 repair): the capability probe read
  untrusted comment bodies, and the comment-window fix landed on the wrong walk.

  **A served sub-issue walk is never a capability refusal.** The probe's
  substring fallback ran before it checked whether the call had even failed, and
  it scanned the whole response — which carries every comment body on every task
  thread. A comment on a public repository is writable by any account, and by
  the campaign's own agents through the machine receipts tally posts to task
  threads, so quoting an ordinary GraphQL error (or quoting issue #334, whose
  body contains the literal string `UNDEFINED_FIELD`) was enough to answer the
  capability gate — and the gate fails *open* into degraded mode, for the life
  of that arm, with the projection label as the only evidence. That is the exact
  outcome #334 item 2 was filed to remove, reached by a new door. The typed
  `errors[]` check is unchanged and still runs on any exit status; the textual
  fallback now runs only on a failed call and only over `errors[].message` and
  `gh`'s own stderr. A response body is never scanned.

  **The comment window was guarded on the receipt walk, not the steering read.**
  Two walks in this tree ask for `comments(last: 100)`. The one repaired
  previously is the driver's, whose comments are machine-authored-filtered and
  feed the diagnosis and retry ledger. The steering an agent is briefed with
  comes from the CLI's `SUB_ISSUE_THREAD_QUERY`, which was untouched — so the
  harm the bullet named, an operator's steering comment scrolling out of the
  window and never reaching the agent, was still live. That query now requests
  `pageInfo { hasPreviousPage }` and the steering read warns, per sub-issue,
  when the window was exhausted. Reported, never refused: the window is
  exhausted by ordinary human discussion. The driver walk's warning is reworded
  to name the consequence that actually follows *there* — a task's oldest
  receipts falling out of the ledger and its attempt budget resetting, which is
  #334 item 6's harm arriving through a second door — instead of sending an
  operator to look for a lost steering comment.

  **The idle poll no longer reads the master issue twice.** `run_campaign_poll`
  reads the master first, because a closed master prunes the registration rather
  than failing the scan, and then `fetch_campaign_graph` read it again. An idle
  tick now costs three REST reads per armed campaign — the authenticated actor,
  the master, and its sub-issue list — and the option description and the
  campaigns page say three instead of the two they claimed.

  Also corrected `ingest`'s docstring in the spec-build driver, which stated the
  opposite receipt precedence to its own code, its call-site comment and its
  tests: task threads are ingested first, so where both surfaces carry the same
  `(kind, task, attempt)` the **thread** copy is counted and the master copy is
  reported as the duplicate.

- Swept the campaign-wave residue (#332, #334, #337, #340), plus one finding
  routed from the #318 evaluation.

  **One drainer for the events directory (#332).** `tally-drain.timer` already
  drained `${stateDir}/events` unconditionally on every tally home at a
  five-second cadence, and the campaign layer's `campaign-continuation`
  producer then rendered a second oneshot and timer at the same cadence over
  the same directory. The drain RPC claims the whole directory whoever calls
  it — the `producer` parameter only stamps the durable admission origin — so
  the second timer bought no coverage, cost one systemd unit and one call per
  interval on every host whether or not it ran campaigns, and made the
  `origin.producer` recorded for a campaign's own self-continuation flip
  between `null` and `campaign-continuation` depending on which timer won the
  race. `producers.<name>.selfDrain` is a new events-dir option, false for
  `campaign-continuation`: the registry entry stays as the declared contract
  and `tally-drain` is the single drainer. `campaigns.<name>` now also refuses
  the reserved name `continuation`, naming the campaign rather than the
  internal producer it would have replaced, and the campaign layer declares
  `${stateDir}/events` in the `spec-build-driver` adapter's
  `extraWritablePaths`, so hardening that adapter cannot silently break a
  campaign's self-continuation. The continue node's write reports through the
  driver's bounded failure path instead of an unhandled `OSError`.

  **The sub-issue capability probe answers only schema questions (#334).**
  `tally campaign arm` degraded to the checkbox projection on *any* probe
  failure, so one transport error, rate limit, or 502 could cost a campaign its
  per-task steering threads, its merged-oracle walk, and its anomaly surface
  for the rest of its life, with the projection label as the only evidence.
  Only a GraphQL schema refusal (`UNDEFINED_FIELD` / `undefinedField` / a
  field-not-found message) is a capability answer now; anything else fails the
  arm and says why.

  **The poll stops paying for a walk that finds nothing (#334).** Every tick
  read the steering surfaces — a full bounded GraphQL traversal of every
  sub-issue thread — before comparing anything. The scan now compares the
  master and sub-issue `updated_at`/`state` values it already fetched over REST
  and runs the walk only when one has moved, so an idle armed campaign costs
  two REST reads per tick and the interval's documented cheapness is true.

  **The Rust checkpoint-receipt reader matched neither namespace (#334).** The
  driver publishes a receipt at `<family>/<baseRevision>`; the projection built
  `<family>` and queried it as if it were the ref name, in both the hidden
  `refs/tally/…` namespace and the legacy `refs/tags/…` one, so the compat
  fallback was dead code and a completed checkpoint never ticked its box.
  Projection now globs the family and accepts a receipt the base branch
  contains. The two implementations are pinned together by shared vectors in
  `test/fixtures/spec-build/checkpoint-refs.json`, asserted from
  `campaign.rs` and from `spec_build_checkpoint_receipts_test.py`.

  **A truncated task-thread comment window is reported (#334).** The walk asked
  for `comments(last: 100)` with no `pageInfo`, so a long human discussion
  silently dropped the oldest comments from the steering read. It now carries
  `pageInfo` and warns; it does not fail the pass, because ordinary discussion
  must not halt a campaign.

  **An upgraded campaign keeps its ledger (#334).** Machine receipts recorded
  on the master before a campaign had task threads were ignored once it had
  them, which reset each task's diagnosis and retry counters mid-flight: a task
  could take one more agent attempt than its budget allows and re-post a public
  comment it had already made. The ledger now reads both surfaces and counts
  one receipt per `(kind, task, attempt)`, preferring the task thread.

  **A deferred checkpoint lane spends no budget (#337).** The #308 loop bound
  relies on a `failureClass` arm that matched only stage `checkpoint`. A
  checkpoint lane also fails at `prep` and at `checkpoint:record`, so a
  checkpoint the reconciler had just declared to have no meaningful verdict yet
  still bought a machinery retry and then a steering attempt out of its own
  budget, and could reach escalation without ever having had a real attempt.
  The whole deferred lane is unpriced now.

  **A sweeper for the producer marker directories (#340).**
  `producers/gh-triggers`, `gh-completed`, `gh-comments` and
  `gh-storage-warnings` each wrote one file per dispatch and were collected by
  nothing — no sweeper, no retention entry, no tmpfiles rule. `tally gc` now
  collects all four under a new `retention.producerMarkerHorizon` /
  `--producer-marker-horizon` (180 days by default, matching the ingress audit
  envelope). A per-marker `.lock` goes only with its own marker and only when
  unheld; the directory-wide `mutations.lock` is never collected.

  **A sticky re-publication is one round trip again (#340).** The sticky path
  edited the comment and then spent a second GraphQL query purely to re-read
  the item state for an assertion that gated nothing — the edit had already
  landed — so on a thread under one page of comments the "sticky" path cost
  *two* calls where the scan it replaced cost one. The state assertion now
  rides the thread scan the create and adopt paths run anyway.

  **Duplicate-acknowledgement suppression moved to the decision point (#340).**
  A duplicate trigger acknowledgement was built, dispatched, recorded in the
  receipt as acknowledged, and then silently discarded by the one production
  sink — so the receipt claimed a publication that never happened, and any
  future sink re-introduced the #245 public duplicate by default. No
  acknowledgement is built for a duplicate now, a sink handed one errors, and
  the vestigial `duplicateAcknowledged` receipt field is retired (still
  accepted on existing receipts, never written).

  **Routed from the #318 evaluation:** `action_prep`'s already-prepared early
  return sat before the fetch and before the worklist/worktree coherence check,
  so a prep retry within one flow run that straddled a remote force-replacement
  returned the stale lane and its stale `baseRev` with no error — the resume
  door bypassed the fail-closed guard the fresh-cut door has. The check now
  runs first, and an existing lane whose own base no longer descends from the
  witnessed revision is refused rather than resumed.

  Also added the repository's first executable coverage of `spec-build.js`
  itself: a Boa-backed harness that evaluates the flow source exactly as the
  engine does and calls its pure helpers, replacing the ripgrep string matches
  that were all that guarded the per-task steering composition.
- The fleet gate can no longer be widened by an ambient environment variable,
  and the knob now covers the budgets that were actually tight (#325).
  `test/fleet-gate.sh` runs `cargo test` directly on the host through `nix
  develop`, which is not `--ignore-environment`, so a `TALLY_TEST_TIMEOUT_SCALE`
  left over from reproducing #299 reached the ladder and made a run with 10x
  wait budgets byte-indistinguishable from an honest one in the transcript that
  is the merge evidence. The gate now scrubs that variable and honours
  `TALLY_GATE_TIMEOUT_SCALE` instead, recording its value — or `1 (unscaled)` —
  on a `timeout-scale:` line in the transcript header, so deliberately widening
  a loaded host stays possible and self-describing. Separately, the knob reached
  only the five `tokio::time::timeout` budgets in `flow_live.rs` and none of the
  21 tighter 10-second polling deadlines, which were fixed iteration counts
  (`for _ in 0..400 { …; sleep(25ms) }`) rather than budgets — they drifted with
  RPC latency and gated the largest fan-outs. The three wait helpers and the two
  inline loops now take a scaled wall-clock deadline and name the knob and its
  value when one expires. Finally, `TALLY_TEST_TIMEOUT_SCALE` accepted values in
  `(0, 1)`, which *narrowed* every budget it reached and produced reds that read
  as product timeouts, and values large enough to overflow `Duration::mul_f64`
  inside libcore where nothing named the variable; the accepted range is now
  `[1, 1000]` and every rejection names the variable, its value, and the reason
  for the bound it crossed. `test/fleet-gate.sh` validates `TALLY_GATE_TIMEOUT_SCALE`
  against that same range, so a value the Rust knob will refuse is refused at
  second zero rather than an hour later inside the ladder's `cargo test`.

- A retained adapter-smoke commit probe is now bounded, named, and never seeded
  for a failure that has nothing to do with the adapter (#328). `tally adapter
  smoke --assert-commit` seeded its throwaway git repository *before* the
  enqueue RPC, so an unreachable daemon left a full repository behind; the seed
  now happens after the connection is open. Retaining the repository on an
  adapter failure is still deliberate — a failed probe is the evidence — but
  every failure past the seed now names the retained path, not just the
  commit-assertion failure, and `tally gc --state-dir DIR` sweeps `probe-*`
  under `DIR/adapter-smoke/` on the capture-archive horizon, reporting
  `adapterProbesExamined`/`adapterProbesPruned`. Nothing had ever known that
  prefix, so every retained repository was permanent.

  `tally adapter smoke` gained **`--state-dir PATH`**, the state directory the
  default probe root derives from, because a sweep and a producer that resolve
  different state directories reap nothing. Without it the CLI resolves
  `$XDG_STATE_HOME/tally`, which on a NixOS deployment is not the module's
  `stateDir` (`/var/lib/tally/state`) that the retention timer hands to
  `tally gc` — so on that path the growth this closed was still unbounded. Pass
  the same directory to both. `--probe-root` still names a directory outright
  and is *not* swept unless it happens to be `<gc state dir>/adapter-smoke/`;
  that limit is now stated in `doc/src/operating/cli.md` rather than implied.

- `services.tally.producers.<name>.reviewers` no longer accepts a login the
  daemon will refuse (routed from the #318 evaluation). The Nix assertion
  enforced GitHub's login grammar with no length bound while
  `producers/validate.rs` capped it at 39 characters, so a 40-character entry
  deployed green through `nixos-rebuild` or Home Manager and then failed at
  daemon config load — a green deploy with a dead daemon. The grammar now lives
  in `nix/lib/gh-login.nix` with the bound included, and both sides run the
  pinned corpus at `test/fixtures/gh-login/vectors.json`, so neither can drift
  alone.

- Repaired the two-repository campaign seam (#321): a split campaign could not
  pass a checkpoint, and every pull request it opened published a wrong
  cross-repository reference.

  1. **A checkpoint task in a split campaign failed permanently, on every
     pass.** The reconciler adds `source.repository` to its witness whenever the
     campaign is split and forwards that object verbatim into the checkpoint
     node, which re-validated it against a closed key set that was never
     widened — so the extra key was a hard error. No receipt was written, the
     frontier never advanced past the checkpoint, and each pass burned a
     machinery retry and then escalated. Since a spec-corpus worklist phases
     itself with checkpoints, this hit the exact shape the seam exists for. The
     checkpoint node now admits the key it is sent (and validates its form), and
     the seam's own fixture worklist carries a checkpoint task so the suite runs
     the node end to end.
  2. **Pull request bodies named the wrong repository.** The campaign
     back-reference was rendered as `<code repo>#<campaign issue number>`, which
     GitHub resolves against the *code* repository — an unrelated issue or pull
     request there, cross-referenced once per task on a public surface. It is
     now rendered against the repository the campaign issue actually lives on,
     which for a single-repository campaign is the same string it always was.
  3. **The closing summary cited a revision that does not exist in the
     repository whose merges it lists.** A split campaign's worklist revision
     resolves only in the spec repository while every merge and checkpoint row
     beneath it names code-repository artifacts. The summary now says which
     repository each revision belongs to and names the code base revision
     alongside the worklist pin. A single-repository campaign has one history
     and keeps its unqualified one-line form.

  Also corrected two claims. The seam section of Flows → Campaigns and the
  `#321` entry below now state that the full-form `Closes owner/name#<n>` and
  the cross-repository completion narrowing are **staged, not reachable**: both
  require tasks carrying their own sub-issues, which only the forge-native read
  path produces, and a forge-native campaign refuses the roles. And the `#318`
  entry's item 5 said the prep brief carries `source.revision`; the tree ships
  `baseRevision`, and the entry now says that.

### Added

- Landed the two-repository campaign seam (#321): a spec-corpus campaign can
  now read its worklist from one repository, land its lanes, publish branches
  and pull requests on a second, and keep its campaign issue thread — and every
  machine receipt — on a third. Three campaign options, `codeRepository`,
  `specRepository` and `issueRepository`, bind those roles to entries of the
  campaign's existing `repositories` map, and each defaults inward (issue → spec
  → the repository the campaign issue was read from). A campaign that sets none
  of them renders arguments that do not carry the roles at all and takes exactly
  the pre-seam path, so single-repository behaviour is unchanged. The empirical
  probe recorded on the issue verified cross-repository closure live before any
  of this was written: a code-repository pull request carrying
  `Closes owner/spec#<n>` closes the spec-repository sub-issue on squash merge,
  the parent's `subIssuesSummary` advances across repositories, and
  `closedByPullRequestsReferences` still returns the merged pull request, so
  §9.1.2's oracle survives the split. The same probe's control showed a bare
  `Closes #<n>` links and closes nothing across repositories, which is why every
  `owner/name#<n>` a split campaign writes — starting with the campaign
  back-reference in each pull request body — is rendered against the repository
  it actually resolves in. Two further behaviours are **staged rather than
  reachable**: the full-form `Closes owner/name#<n>` and the requirement that a
  cross-repository closing reference be on the campaign's `codeRepository` both
  need tasks that carry their own sub-issues, which only the forge-native read
  path produces, and a forge-native campaign refuses the roles. A split campaign
  therefore runs the degraded projection today; reconciling task sub-issues with
  the worklist-artifact path is design work that has not been done, and the seam
  section of Flows → Campaigns says so. The
  witness splits accordingly: the reconcile result reports the worklist's
  pinned spec revision (with `source.repository` when split) alongside
  `baseRevision`, the code base tip that lane bases, checkpoint receipts and
  merged-commit ancestry are anchored to. An ad-hoc forge-native campaign is
  single-repository by construction and refuses the roles rather than partially
  honouring them. Adds no flow node; the `campaignMaxNodes` pin is untouched.

- Repaired #316: reproduced the #247 frozen window, and made a `--flow-run`
  window say when it is not evidence about the run. `--flow-run` membership is
  not a durable property — tally recomputes it on every call by scanning
  durable rows and witness records for an orchestration capsule naming the run,
  and an admission that writes no row (`attached`, and full-mode `reused` and
  `terminal`) leaves the submitting run holding a task UUID that is not one of
  its members. Its events are then filtered out of that run's own window with
  no page cap involved: **same items, `nextCursor: null`, while the work
  runs** — exactly the reported #247 shape, which the earlier stale-page-one
  diagnosis could not produce, because a null cursor means the page is the
  window. `repro_247_an_attached_node_is_invisible_to_the_run_that_submitted_it`
  pins it against a live daemon. The seam itself is **not fixed**: closing it
  needs durable per-run membership for row-less admissions, which is the
  enqueue kernel's frozen surface. Instead every run-scoped `query.log`
  response now reports `flowRunTasks`, the number of task UUIDs the filter
  resolved to, and `flowRunTasks: 0` — an empty window that is not a fact about
  the run — is called out on stderr rather than left to look like quiet. The
  monitoring contract in Operating → Observability gains a section on the
  membership seam, a rule that a terminal verdict must not be read off the
  incremental stream (a journal terminal whose witness lands after you polled
  past its cursor is never re-delivered enriched), and a corrected proof of
  quiet: read `items`, not `position`, because `position` is the head of the
  whole lifecycle stream and advances whenever anything else on the daemon
  does.

- Made flow-run-scoped truncation legible, and gave `query log` a durable
  incremental position (#316, closing #247). Human `tally query log` and
  `tally query jobs` now follow the page cursor to the end of the filtered
  window inside the one invocation instead of printing the first capped page,
  which was permanently stale by construction: the lifecycle window is ordered
  oldest-first, so page one of a long run never changes however far the run
  advances, and the only truncation signal went to stderr where a monitor
  diffing stdout never saw it. Anything that stops the output from being the
  whole window is now one unambiguous stderr line — a page cursor that expired
  mid-window (the query restarts once and says so), elided oversized fields, or
  a position that predates retained history. Every paginated envelope carries
  `truncated` and `elidedItems`; `--json` and an explicit `--cursor` keep
  single-page semantics, so a caller that owns the cursor still cannot mistake
  a page for a window, and `query jobs` gained `--json` for that purpose.
  `query log --after <position>` takes a durable `log-v1:<lifecycle>:<witness>`
  coordinate, reported as `position` on every response and distinct from both
  the `--since` time filter (unchanged) and the ephemeral `--cursor`; because
  the reported position is the stream head, `--after` plus empty items is a
  proof that a run is quiet rather than an absence of matches. A position that
  predates retained lifecycle history is reported as `positionGap` rather than
  served as a silent partial continuation. An item that alone exceeds the
  48 KiB response cap no longer fails the whole query: its largest string
  fields are truncated and the item is marked with an `elided` object naming
  the JSON Pointers that were cut, so a campaign runner whose argv embeds an
  issue body cannot make its run unmonitorable. Only an item oversized because
  of its structure remains a hard error, and that error now names itself. Every
  new print on these paths goes through the panic-safe `outln!`/`errln!` and
  compacts the daemon-sourced values it echoes, so a walked window is a quiet
  exit 0 for a reader that hangs up mid-window and carries no terminal control
  on either surface. The monitoring contract is documented in Operating →
  Observability.

- Added a run-scoped campaign digest and its markdown renderer, published as a
  closing summary on **both** terminal outcomes: completion and escalation at
  frontier quiescence. The completion path took over the existing
  `tally:campaign-complete:v1` comment — a campaign still posts exactly one
  machine comment before it closes the issue, and that comment is now the
  digest — and a quiescent campaign gets the same digest beside its escalation,
  reflecting partial state. The digest is derived from facts the pass already
  witnessed (merged pull requests, checkpoint receipts, diagnosis/retry
  receipts, the reconciler's own arithmetic) and adds no state store. Both
  summaries are always fresh comments, never an upsert, and both render inside
  the existing reconcile and escalate nodes: `spec-build.js` node count is
  unchanged and `max_flow_nodes` still asserts 51. On a local forge the summary
  is a durable blob under `refs/tally/spec-build/v1/<scope>/summary/<outcome>`.
  The reconcile result carries `closingSummary` and the escalate result carries
  `summary`. A forge-native campaign posts the completion summary and then
  closes its sub-issues and master issue; a file-worklist campaign posts the
  same summary on its master issue and leaves it open, because that issue is a
  projection whose lifecycle tally has never owned. That path previously
  published nothing at all on completion.

- Added `services.tally.campaigns.<name>.gitAiAwaitSec` (default 60), the merge
  node's budget for git-ai's settlement barrier, and an evaluated assertion
  relating it to that node's own deadline: while `gitAiBinding` is not `off`,
  `driverRuntimeMaxSec` must be at least twice the budget. The barrier runs
  inside the merge node, so a campaign that paired a short deadline with a long
  barrier was killed mid-wait on every task and reported a node timeout instead
  of a binding receipt.
- Added a revision mode to `tally witness verify-authorship`:
  `--revision <oid> --note-sha256 <digest>` verifies one repository-native note
  directly instead of a witnessed task lane. The campaign merge node binds the
  commit the forge minted when it squashed, which the witness ledger never
  names, so the ledger mode could not reach it at all; the merge receipt records
  the revision and the digest and this re-derives the digest from the
  repository. The digest is required, so a pass is always a comparison.
  `--revision` and `--task` are mutually exclusive, and the report's schema
  version moves to 2: `ledgerPath`, `ledger`, and `taskUuid` are omitted in
  revision mode.

- Added `services.tally.campaigns.<name>.gitAiBinding`, an enum of `off`
  (default), `advisory`, and `required`, arming the fourth proof axis on the
  commit a campaign integrates. A forge-side squash arrives with no authorship
  note at all — `doc/src/flows/git-ai-squash-fidelity.md` measured that
  attribution is re-minted at `git commit` time and only in the repository that
  made the commit — so under `advisory` and `required` the merge node re-mints
  the same integration in a detached worktree of the campaign checkout, proves
  the merged commit's first parent is the gated base and its tree equals the
  reconstruction's, copies the resulting note onto the integrated commit, and
  pushes `refs/notes/ai` to the campaign remote. The push is fast-forward-only
  and folds a diverged remote in with `cat_sort_uniq` rather than forcing.
  Every outcome is journaled with the merge node as an `authorship` receipt
  naming the posture, status, bound revision, notes-ref target, exact note
  digest, whether it published, and a typed reason. `advisory` never fails a
  node; `required` fails the merge on any status other than a published
  `bound`. The binding is a step inside the existing merge action and adds no
  flow node.
- Added the `Assisted-by: <adapter>:<model> (tally:<taskUuid> witness:<seq>)`
  trailer to campaign squash commit messages, byte-identical to the trailer the
  gh producer publishes. Every component comes from the settled implementation
  node — the campaign's agent adapter, the canonical model the daemon recorded,
  and the task UUID and witness sequence of the attempt being merged. With no
  canonical model recorded the node writes no trailer rather than a plausible
  one, and the narration validator now refuses a proposal that carries an
  `Assisted-by:` line: that authority belongs to the node.
- Added `model` to the flow job spec and
  `services.tally.campaigns.<name>.agentModel`. A flow's `job()` can now name
  the model its node runs under, normalized into the same `adapterOptions`
  kernel field catalog members already reach through their `launch` object, and
  still refused outright by an adapter that authorizes no model override. The
  flow node result carries the daemon's canonical model back to the script,
  which is what makes the campaign's provenance trailer sourced from the
  witnessed attempt rather than from the script's own input.

- Added `services.tally.campaigns.<name>.mergeMethod`, an enum of `merge` and
  `squash` that defaults to `squash`. A squashed campaign leaves one commit per
  task on the base branch instead of a merge commit carrying a template
  message. Under `squash` the GitHub merge node runs
  `gh pr merge --squash --match-head-commit <head> --subject <subject>
  --body <body>` and proves completion from the pull request's merge commit
  rather than from the task head, which a squash never makes an ancestor of the
  base branch; the `merge` path is unchanged. On a `forge = "local"` campaign
  the merge node commits a single squash on base and publishes a receipt ref in
  the campaign's hidden state namespace, which reconciliation reads alongside
  the existing branch-head ancestry proof.
- Added the steward's narrate slot at the publication boundary.
  `services.tally.campaigns.<name>.steward` binds an adapter from the open
  adapter map as a catalog role; that adapter's `argv`, `env`, and
  `scrape.finalMessage` are what supply the narrator's model, endpoint,
  credentials, and capture, and `stewardArgv` is appended to its argv. The
  publish node runs it, hands it a JSON narration request on stdin, and reads
  its proposal back from the declared capture, defaulting to
  `^TALLY_FINAL_MESSAGE=(.*)$`. A
  deterministic commitlint-shaped validator accepts or refuses the proposed
  conventional-commit text, re-requests once on refusal, and falls back to the
  brief-derived template on a second failure. The narration governs the pull
  request title and prose and the squash commit message; the node executes git
  and the model never does. The seam adds no flow node, so the campaign node
  budget is unchanged.
- Added `test/git-ai-squash-fidelity.sh` and its recorded findings under
  `doc/src/flows/git-ai-squash-fidelity.md`: the empirical check of what git-ai
  authorship survives a squash. Attribution re-mints **per-line** on a squash
  executed locally by a trace2-armed git, and does not appear at all on a
  squash performed by the forge and merely fetched. The spike skips when the
  externally provisioned `git-ai` binary is absent and is not part of any gate.

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

### Changed

- Cleared the residue the TaskChampion delete left behind (#326). `RELEASING.md`
  told an operator upgrading a host to "preserve witness, taskdb, lease,
  launch-marker, and worker state together" — but post-delete the only on-disk
  artefact "taskdb" can name is `<dataDir>/taskdata/` and its
  `taskdata.pre-rebuild-*` archives, which nothing reads, nothing sweeps, and
  which still count against the data-store byte budget. On the #252 incident
  host that sentence told the operator to preserve exactly the 270 GiB the
  CHANGELOG told them to reclaim; it now names the durable state explicitly and
  says outright that deleting `taskdata/` by hand is safe and is what returns
  the space. The terminal-ack comment in `daemon/run.rs` no longer lists
  "replica commit" as a post-ack step — that channel does not exist — and
  `TaskRow`, `TaskStatus`, `impl From<&TaskRow> for RowFact`, and the
  `TaskDbError::InvalidRow` variant are removed: nothing in the workspace
  constructed any of them after the delete, and `pub` in a library crate meant
  `dead_code` never said so.

- **`--limit` on the default `tally query jobs` / `tally query log` now sizes a
  page, not the result set** (#316). Those commands walk the cursor to the end
  of the filtered window inside one invocation, so `--limit 10` returns the
  whole window in pages of ten rather than at most ten items. Scripts that used
  `--limit` as an output bound should narrow with the filters instead, or pass
  `--json` (or an explicit `--cursor`) to keep single-page semantics and own
  the cursor.

- **Upgrade consideration — the free-space floor moved from 256 MiB to 8 GiB.**
  The `storage.dataDir`/`storage.stateDir` defaults are now
  `minimumFreeBytes = 8589934592` (8 GiB, the hard admission floor) and
  `warningFreeBytes = 17179869184` (16 GiB, the durable warning threshold),
  replacing the former 256 MiB floor, which covered only a few pathological
  replica rewrites. On an existing deployment whose host has less than 8 GiB
  free, the first daemon start after the upgrade refuses all new intake
  immediately — legibly, with an explicit refusal reason naming the observed and
  minimum bytes, and with already-admitted work left to finish — but with no
  other warning. Check free space before upgrading, or set `minimumFreeBytes`
  and `warningFreeBytes` explicitly; the recovery band is at least 1 GiB above
  whatever floor you choose, so intake stays closed until availability clears
  that band. Sizing guidance is in Operating → Retention and growth.
- Capture locks moved out of the job-writable `unit-exit/` directory and the
  daemon no longer blocks on one. New locks live at
  `<stateDir>/capture-lock/<uuid>.capture.lock`, a sibling that neither `strict`
  nor `production` grants to a job — both grant `unit-exit/` whole, because the
  `ExecStopPost` recorder writes the exit record there, so any job running as
  the daemon user could open and hold the lock the daemon waits on. `workspace`
  and `none` are stated exceptions: they grant the state directory whole or
  constrain nothing at all, so the relocation moves that surface off the
  narrowing presets rather than removing it everywhere, and both are already
  documented as for trusted programs only. The wait is now a bounded try-lock:
  five seconds of backoff, then a clear `capture lock … was still held` error
  instead of an indefinite `flock`. Dispatch takes the lock on the blocking
  pool, and the durable-wait RPC handlers project a terminal witness after
  releasing the daemon context write lock, also on a blocking thread — so a
  contended lock never parks the daemon's single async thread, whatever the
  preset. Callers already tolerate a failed excerpt materialization: a receipt
  loses its `stderrTail` rather than the daemon losing the ability to answer.
  Locks left behind in `unit-exit/` by an older daemon are never taken again and
  are drained by `tally gc`, which now sweeps both locations.
- A dispatch that cannot take the capture lock inside the deadline is recorded
  as `preempted`, not as a job failure. The unit was never launched, so
  attributing it to the agent burnt an attempt, wrote a `Failed` witness with
  exit code 1, and — with `postFailureEvidence` on — posted a public failure
  receipt with no evidence in it, all for a daemon-side file-locking condition.
  `preempted` carries a resource-return retry trigger, is excluded from
  canonical GPU seconds, and emits no failure receipt. No gate manifest is
  evaluated for an attempt that never ran.
- `tally gc`'s capture-lock sweep actually drains now. The daemon used to create
  the lock file *before* checking whether the capture generation still existed,
  so the startup reconciler — which replays every failed witness in the ledger
  at every start — re-minted one lock per historically failed task with a fresh
  mtime, resetting the age the sweep measures. A deployment restarting more
  often than `captureArchiveHorizon` therefore never collected a single lock.
  The generation is now checked before the lock is taken as well as under it, so
  a dead task mints nothing.
- Failure receipts now carry `stderrRedactions` beside `stderrRedacted`, so a
  reader can tell one dropped token from forty dropped lines. The boolean keeps
  its meaning; the count is the number of replacements present in the published
  tail. When redaction overflows the publication bound and the head is dropped,
  replacements that fell outside the surviving window are not counted — the
  number always describes the text in the receipt, and `stderrTruncated` says
  the head is gone. Redaction *matching* is unchanged.
- `retention.horizon` now doubles as the retry window for brief-bearing jobs.
  This is not new behavior and not a regression, but it has never carried a
  release note: once a job's brief has expired out of `<dataDir>/briefs`, that
  job can no longer be retried, and `queue retry` refuses it with `job <uuid>
  brief is no longer retained and cannot be retried; enqueue fresh work`. The
  remedy is exactly what the message says — enqueue fresh work; there is no
  flag that revives expired brief bytes. Operators who retry old campaign
  runners will meet this refusal after an upgrade and should read it as the
  horizon doing its job, not as breakage. Widen `retention.horizon` if the
  operational retry window needs to be longer.
- `tally gc` now prunes dead `unit-exit/<uuid>.capture.lock` files under
  `captureArchiveHorizon` and reports `captureLocksExamined` and
  `captureLocksPruned`. The failed-stderr reconciler re-mints one lock file per
  historically failed task at every daemon startup, so the directory grew one
  permanent file per dead task with no pruner to drain it. Two checks gate every
  unlink, because neither is sufficient alone: `flock` does not refresh mtime,
  so age alone cannot prove a lock is dead, and unlinking a held lock lets the
  next locker create a fresh file and break the mutual exclusion. A lock is
  removed only when it is both older than the horizon and provably unheld, via a
  non-blocking exclusive lock the sweep takes and holds across the unlink. Exit
  records and `<uuid>.capture.json` generations are untouched: they remain
  durable recovery input with no age-based pruner.
- `tally __producer-dispatch` now requires `--data-dir`. The hidden command used
  to fall back to the state directory, which silently recreated the split brief
  layout retired in #271 and wrote briefs into `<stateDir>/briefs` — a location
  the retention sweep now treats as a legacy store to drain. Generated Home
  Manager units always passed the flag, so only hand-run invocations were
  affected; those must now name the daemon data directory.

### Fixed

- Landed the five defects the August 1 audit recorded (#318). Each is
  independent; they share one entry because they shipped as one sweep.

  1. **A flow `codex()` node no longer loses its `-C <worktree>` argv.** Flows
     deliberately submit no raw `cwd` and carry structured workspace metadata
     instead. #232 fixed the *process* working directory by deriving it from
     `workspace.worktreePath`, but every adapter-argv render site still read the
     raw row `cwd` — `None` for every flow node — so a lane executed in the
     right directory with a witnessed argv that said nothing about it, and the
     durable record, the resume render, and any executor that does not inherit
     the request cwd all disagreed with reality. Admission, retry, recovery, and
     the execution request now resolve one effective working directory
     (`cwd`, else the workspace worktree), so the argv and the process cannot
     diverge again. An explicit payload `cwd` still wins. The enqueue kernel is
     untouched: the canonical payload hash still covers the submitted `cwd`
     only, so dedup arithmetic is unchanged.

  2. **`requestReview` requests a review instead of serializing a boolean.**
     Its entire effect was encoding `"requestReview":true` inside the machine
     completion comment; the producer's mutation vocabulary was comment /
     closeIssue / closePullRequest, so no review was requested and no human was
     notified. `gh` producers gain a `reviewers` list of GitHub logins, required
     non-empty whenever `requestReview` is on (enforced in both Nix eval and
     daemon validation). On fire, a pull request receives GitHub's own
     `requestReviews` mutation — additive, so it never cancels a review a human
     requested — and an issue, which has no review concept, receives one fresh
     marker-idempotent comment mentioning the reviewers. Fresh rather than
     upserted: anything that asks for a human has to actually notify one. The
     encoded field stays in the completion record as provenance, now beside the
     logins that were asked.

  3. **An unset `closeOnPass` no longer inherits `postEvidence`.** The fallback
     existed for configurations serialized before the field did, which is not a
     supported input; its effect was that a producer could close issues purely
     because evidence posting was on. Absent now means off. The
     `closeOnPass = true` requires `postEvidence = true` guards are unchanged.

  4. **`hardPreempt` on co-allocated pools is conjunctive, and the doc is
     right.** The documentation promised that hard reclaim requires the opt-in
     on every blocking pool; the code OR-ed the flag per victim lease, so a
     holder co-allocated on a pool with `hardPreempt` and one without it was
     killed even though the second pool's configuration promised its holders are
     never killed. The conjunctive semantics are now pinned in code and tests:
     a holder is hard-reclaim eligible only when every pool that same interrupt
     request asks *it* to yield in opts in. A pool's `false` is a promise to its
     own workloads.

  5. **A campaign lane cannot be cut from a history the witnessed worklist never
     described.** The reconciler witnesses the worklist at a revision; lane prep
     fetches later and cuts from whatever `remote/baseBranch` resolves to then,
     with nothing relating the two. Checkpoint lanes already asserted this
     relationship; implementation lanes asserted nothing, so a rewound or
     force-replaced remote silently produced lanes from an unrelated history.
     The prep brief now carries the reconciliation's `baseRevision` — the code
     revision the pass reasoned from, which equals the worklist revision unless
     the campaign spans two repositories — and prep fails closed after its fetch
     unless that revision is an ancestor of the fetched base head, with the same
     legible error shape as the checkpoint check.

- A slow storage tree walk can no longer overwrite a fresher free-space probe
  (#317, closing #292). Two writers update the monitor's view of filesystem
  availability: the cheap per-intake/periodic probe, and the periodic tree walk,
  which reads availability when it *starts* and installs it when it *lands*. A
  walk that finished after a probe therefore reinstalled pre-probe availability
  and moved `freeSpaceCheckedAt` backwards; if availability crossed the hard
  recovery band during that walk, the monitor emitted an Ok→Hard transition pair
  for nothing that happened — a fsynced warning record, a journal line, and a
  GitHub campaign receipt each way. The walk now keeps the probe's availability
  figures, the per-store level derived from them, and `freeSpaceCheckedAt`
  whenever the snapshot's probe is newer than the walk's sample stamp; tree
  sizes, growth per completion, and `sampledAt` still come from the walk.
  A tree walk can therefore no longer move `freeSpaceCheckedAt` backwards; the
  guard constrains the walk writer only, and a probe still assigns the stamp
  unconditionally from the wall clock. Admission math is unaffected either way — every admission
  re-probes before deciding — so this changes receipts, not who gets in.

- A command whose reader hangs up now ends quietly instead of panicking.
  `tally query run <id> | head -1` printed through stock `println!`, so the
  first write after the pipe closed panicked with `failed printing to stdout:
  Broken pipe` and a failing exit status. Every human-facing print in the CLI —
  `query`, `queue` (including `queue continue`), `lease`, `enqueue`, `flow`,
  `campaign`, `adapter`, `producer`, `witness`, `gc`, and the clap-generated
  help — now writes through a helper that reports a closed reader as exit 0
  with no message; any other write failure is still an error with its message.
  The `tally: <error>` line in the top-level error printer is dropped rather
  than panicked on, and the exit code stays the error's own. `clippy.toml`
  disallows `println!`/`eprintln!` so a converted file cannot regress silently;
  the writers that must keep printing unconditionally — the daemon's log
  surface in `tally-core`, the executor's captured diagnostics, and test
  harness output — carry an explicit `allow` naming the reason.

  The process-wide SIGPIPE disposition is untouched, so `daemon run`,
  `__remote-executor`, and `__record-unit-exit` keep seeing a closed socket as
  an error to report rather than a reason to die mid-write. Two paths keep
  their own behaviour by design: the flow runner's JSONL lifecycle stream still
  turns a write failure into a `FlowCaptureError` (its stdout is the daemon's
  ingest channel, not an operator's pipeline), and a command that would have
  exited nonzero exits 0 if the pipe breaks before its last line — the same
  outcome the default SIGPIPE disposition produces, for a reader that is gone
  either way.

- Terminal-output sanitization now covers the remaining daemon-sourced strings
  in `tally query run` and `tally query log`: flow name, campaign, flow-run id,
  run state, task status, transition timestamp, adapter, pool set, provenance,
  and the `--cursor` continuation hint on stderr. These fields are
  trusted-source today, so this is defense in depth rather than a live hole —
  but the renderer is no longer what makes that load-bearing. `compact_text`
  additionally drops U+061C ARABIC LETTER MARK, which reorders a line exactly
  as the already-filtered LRM and RLM do, and an unterminated CSI scan now
  stops at an embedded C0 control instead of eliding text up to the next
  `0x40-0x7e` byte, so malformed adapter output loses less legitimate text.

- A single corrupt or non-canonical `<64hex>.json` file in a brief store no
  longer aborts the whole retention sweep. `managed_brief_files` propagated the
  verification failure out of `run_gc`, which stopped it after GC-root pruning
  but before the brief, state-directory, and projection-archive sweeps — so
  capture-archive and producer-event pruning stopped too, and because GC never
  removed the offending file, every subsequent timer run failed identically and
  silently. Unverifiable files are now counted as `briefsUnverified` /
  `legacyBriefsUnverified` and skipped. They are not pruned and not renamed: an
  unverifiable file is unaddressable by any live brief hash, so it is inert, and
  it is the one case the sweep cannot parse well enough to act on. The nonzero
  count is reported on every run so the condition stays visible rather than
  being announced once.

- A campaign lane's prepared base is now derived from the lane's own history
  rather than recomputed from wherever the base branch points at prep time. When
  a lane directory disappeared but its branch survived, the driver adopted the
  branch and handed the flow a `baseRev` taken from the current
  `origin/<baseBranch>` — a commit the lane's head does not descend from. The
  ownership node then failed the task with `task head is not descended from its
  prepared base revision` for the rest of the pass, and the failure path fed the
  diagnosing steward a patch that appeared to delete files owned by the base
  branch which the task never touched — a wrong steering note, posted publicly.
  The base is now `git merge-base <lane head> <base tip>`, which is the same
  value as before on a fresh or published lane and an ancestor of the lane head
  by construction on an adopted one.
- Campaign lane identity is now written in one atomic act — a replacement
  `config.worktree` built with `git config --file` and renamed into place —
  instead of one `git config --worktree` call per field. A runner killed part way
  through the old sequence left a lane holding some of its identity, and
  `resume()` then wrote back only the fields it happened to know: the lane looked
  valid to every later pass while being permanently unable to answer for
  `baseRev`, so every prep in that pass failed with an error naming nothing an
  operator could act on. `resume()` now reports an incomplete lane as incomplete
  and the caller re-derives the missing fields from the lane itself, which is
  also the path an estate takes when it upgrades across the identity move over a
  live lane.
- The closing summary at frontier quiescence is now published before the
  escalation comment rather than after it. The escalation is what every later
  pass reads back to decide the campaign has already stopped, so a summary that
  failed after it had landed — a rate limit, a transient network error — was
  never retried by any later pass and the campaign silently lost its quiescent
  digest for good. Publishing the digest first means one transient failure
  retries the whole terminal act, and the summary's marker makes that retry
  idempotent.
- The campaign sweep now reclaims the pre-#312 `.state/<runHash>/<taskId>.json`
  lane markers belonging to its campaign once the run they name is proved dead,
  and reports each one in `cleaned`. Nothing writes those markers any more, so an
  estate that upgraded across the identity move kept them and their directories
  for ever with nothing able to explain them.
- The campaign merge node's `Assisted-by:` forgery guard now matches the way git
  reads a trailer. Git matches trailer keys case-insensitively, so a steward
  proposing `assisted-by:` passed validation and its line landed verbatim in a
  squash commit on the default branch of a public repository, where every
  git-native consumer reads it as an `Assisted-by` trailer. With the shipped
  `agentModel = null` default the node appends no trailer of its own, so the
  forged line was the message's entire trailer block — a provenance pointer to a
  task UUID and witness sequence nobody executed.
- The campaign merge node no longer folds diverged authorship notes together,
  and no longer pushes the campaign checkout's whole `refs/notes/ai`. The
  previous publication path used git's line-oriented `cat_sort_uniq` note-merge
  strategy on a `authorship/3.0.0` record whose line order is semantic, which
  published a structurally invalid note and — because `git notes merge` writes
  into the *local* ref — rewrote the daemon's witnessed code-result bindings in
  the campaign checkout, turning those tasks into a permanent
  `note-content-mismatch`. Publication now assembles a scratch ref from the
  remote's own tip plus the integrated commit's entry and pushes that, so only
  the integrated commit's note is published; a remote already carrying a
  different record for that revision is reported as a typed `conflict` and
  nothing is written. The receipt's `noteSha256` is read back from the remote
  after the push instead of being computed before it, and `notesRefTarget` names
  the campaign remote's ref rather than the checkout's. What the remote carries
  beyond that entry is not tally's: git-ai publishes `refs/notes/ai` itself on
  an ordinary `git push`, which is now measured and recorded in
  `doc/src/flows/git-ai-squash-fidelity.md`.
- The campaign binding is re-enterable. A later reconcile pass can dispatch the
  merge node again for a task whose pull request is already merged; that pass
  reconstructs the identical commit, which git-ai will not re-annotate, so the
  copy had no source and the binding regressed to `missing-note`. An integrated
  commit that already carries the note the step would have produced is now a
  completed binding.
- The campaign merge node removes the throwaway reconstruction's note after
  copying it onto the integrated commit. A notes entry is keyed by commit id as
  a path in the notes tree, so it outlived the unreachable commit it annotated
  and accumulated one dead note per merged task on a public forge.
- `gitAiBinding = "advisory"` can no longer fail a merge node. The binding runs
  after the merge has landed irreversibly, so a raised error reported a merged
  task as failed; the unguarded `git fetch --prune` and the workspace-root and
  temporary-directory setup could all raise. Every outcome, including an
  unexpected one, is now a typed receipt, and the reason names the remote and
  the git exit status rather than echoing transport stderr into a report that is
  quotable in public.
- The campaign binding's reconstruction now commits under its own identity. A
  local-forge merge that shared the merge node's identity, tree, parent, message
  and committer second produced the same object ID as the integrated commit, so
  `git notes copy` was a no-op onto itself and the copy path the whole binding
  turns on went unexercised by its own regression test.

- Make the sticky-comment recovery path publish what it was asked to publish.
  When no stored comment id was available and the marker scan found the comment
  already on the thread, the sink adopted that comment's id, issued no mutation,
  and returned success — so the body it was given was discarded and a forge-side
  refusal of the edit (secondary rate limit, 502, a locked comment) degraded
  "edit in place" into "do nothing, silently, and report success". The recovered
  comment is now written to, and a refused edit fails the publication with the
  forge's own error instead of being swallowed. A marker found on a comment with
  no node id refuses rather than publishing a duplicate.
- Stop publishing a public "Tally already recorded this trigger" comment when a
  GitHub producer re-observes a trigger that is already in its ledger — what
  every producer restart does to every historical trigger on a campaign issue.
  The duplicate outcome is producer-internal bookkeeping and is now never
  posted, and the completion check that missed the existing acknowledgement
  matches the receipt id rather than the decision suffix that wrote the marker,
  so acknowledgements already on public threads still count as complete. Ledger
  recording, trigger grammar, intake authorization, and enqueue dedup are
  unchanged. With duplicates silent at the decision level and receipts
  upserting, `postReceipt` deliberately stays one boolean: the proposed
  accepted-versus-duplicate option split is unnecessary and does not ship.
- Read a spec-build lane's path union, and the merge commits it rejects, from
  the base branch commit the lane actually sits on — the merge base of the lane
  head with the current base — rather than from the current tip or the stale
  prepared base. Every campaign merge is `--no-ff`, so a base branch that has
  integrated anything is full of merge commits and a lane that rebases onto it
  inherits them; resolving from the prepared base rejected that lane for
  "merging the base into your lane" when it had done the opposite. Resolving
  from the current tip held only until the base advanced once more behind the
  lane. The merge base holds in both cases and does not move as the base
  advances, so a gate receipt and the publication that re-checks it count the
  same paths.
- Take the current base branch tip from a fetch in the lane rather than from
  `refs/remotes/<remote>/<baseBranch>`. A lane worktree is a linked worktree of
  the campaign checkout, so that ref lives in the shared common Git directory
  and the agent can write it; pointing it at the lane head collapsed the
  ownership union to nothing, made every declared conflict domain vacuously
  satisfied, and published an ownership receipt claiming `ownedPaths: []` for a
  branch carrying unowned work. Integration still refused to merge it, but the
  branch and its pull request had already reached the public forge.
- Resolve the `forbidPaths` gate node and publication's re-check of its receipt
  against that same base. A lane that took the documented rebase remediation
  passed ownership and then went red at the gate on a mainline path a sibling
  had landed — the same spurious red the ownership fix removes, one node later.
- Reject a spec-build task lane whose history contains a merge commit, naming
  the real cause: "rebase instead of merging the base into your lane". The
  ownership union walks lane history with `git log -m`, which splits a merge
  and attributes both of its sides to the lane, so a lane that merged the base
  branch claimed every path its siblings had landed and failed on paths no task
  commit touched — with an ownership receipt that misattributed authorship to
  match.
- Resolve a spec-build lane's ownership union against the current base branch
  rather than the base the lane was prepared on, whenever the lane already
  contains that current base. Rebasing onto the advanced base is the documented
  remediation for a red constraint, and it used to pull every mainline commit
  landed since prep into the union and go spuriously red on paths outside the
  task. The receipt still names the base the lane was prepared and gated on,
  and the union narrows only onto a base the lane demonstrably contains, so
  nothing about this widens what a lane may touch.
- Cross-check spec-build publication against the campaign's configured
  `forbidPaths` gates instead of re-running the pattern set stored in the
  constraint receipt. Replaying a receipt against itself proved only that the
  receipt was self-consistent, so a campaign whose patterns were widened
  between the gate run and publication published against the superseded set.
  Drift now fails by gate id, as does a configured gate that reached
  publication with no witnessed receipt.
- Compare the ownership receipt's `domainsRequired` against the campaign's own
  parallelism at merge. The merge brief did not carry the campaign's value at
  all, so the last node that can still refuse to act on an upstream flag
  normalized it and trusted it — the pattern the rest of the integration path
  had already stopped using.
- Schedule spec-build checkpoint lanes after the pass's own merges. A
  checkpoint sharing a frontier with a mergeable implementation task had its
  fresh receipt invalidated by that same pass: the receipt is bound to the
  exact revision tested, the merges moved the base out from under it, and the
  next reconciliation found nothing and re-ran the whole checkpoint. Prepared
  after the merges, the tested revision is the one the next pass reconciles.
- Bound the checkpoint re-validation loop under a moving base. A checkpoint
  whose base advanced during validation still records its truthful receipt for
  the revision tested, but the lane now fails instead of reporting an advance,
  because that receipt names a revision the next reconciliation will not read.
  A base branch moving faster than a checkpoint runs used to re-execute it for
  ever while every pass reported `advanced`, posted a continuation, and never
  escalated; the failure spends the checkpoint's ordinary retry and steering
  budget and reaches escalation instead. Only movement from outside the pass
  can trip it, since a campaign's own merges land before its checkpoint lanes
  prepare.
- Fixed the campaign anomaly surface firing on the campaign's own work. A task
  pull request carries `Closes #<sub-issue>`, so the campaign closes its own
  sub-issues as it merges; editing one task brief and re-arming rotates every
  task's revision, so every already-merged task simultaneously lost its proof
  and kept a sub-issue the campaign had closed. Each of those was reported as a
  `closed-without-merged-proof` anomaly asserting that a human had closed it,
  and `tally query run` pinned the run in `needs-attention` — one false alarm
  per merged task, on the campaign's own documented edit-and-re-arm workflow,
  exactly when the board most needs to be readable. A sub-issue closed by a
  merged pull request carrying this campaign's marker at any revision is now
  recognised as the campaign's own closure and stays in the reconciler's
  ignored-marker warnings; only a hand closure is an anomaly.
- Fixed the sub-issue walk silently reading completion from a truncated page of
  closing pull-request references. `first:` returns the oldest references, so
  the dropped one was the newest — the likeliest current proof — and the task
  would then be re-dispatched into a publish node that hits its own merged pull
  request. The walk now requests `pageInfo` on that connection and fails the
  pass rather than narrowing what counts as proof.
- `tally campaign arm` now reports `subIssueWalk` and `projection` on its
  enqueueing path as well as under `--no-enqueue`. A campaign that armed
  degraded — no per-task steering threads, no merged-oracle walk, no anomalies —
  was otherwise indistinguishable from a native one until an operator's comment
  on a task sub-issue silently failed to reach its agent.
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

### Fixed

- Fixed a campaign squash merge on a `forge = "local"` repository wedging a task
  permanently after one lost base-branch push. The merge receipt ref was pushed
  without `--force`, and Git enforces fast-forward on every non-tag ref, so the
  next pass's rebased squash — necessarily a different object ID — was refused
  and the node failed before the base push it needed to make progress. The
  receipt carries no authority of its own, because the reader still requires the
  commit it names to be an ancestor of the witnessed base, so it is now forced
  and the fast-forward reconciliation that could never succeed is gone.
- Fixed steward narration silently discarding everything the bound adapter
  carries except `argv`. The adapter's `env` — where a narrator's endpoint and
  credentials live — and its declared `scrape.finalMessage` capture now reach
  the publish node, so an adapter configured the documented way works instead of
  failing twice and narrating from the template for ever. The narrator's
  environment is the publish node's plus that `env`, minus `TALLY_BRIEF`, which
  it has no business reading. A steward adapter that declares per-job launch
  policies, a hardening preset, or `extraWritablePaths` is now refused when the
  module is evaluated rather than run without them, because the narration seam
  runs a direct argv rather than a tally job and cannot apply them.

### Security

- Campaign narration proposals that carry a GitHub closing keyword
  (`Closes #12`, `resolved acme/spec#9`, a `.../issues/3` URL) or an `@mention`
  are now refused by the narration validator. A pull-request body is executable
  on GitHub and a squash commit message lands on the default branch, so an
  unfiltered narrator could close issues the campaign never named and notify
  people or teams on a public repository. The node's own `Closes #<sub-issue>`
  is unaffected; a bare `#<n>` cross-reference stays allowed because it
  backlinks and notifies nobody.

### Changed

- Campaign lane identity moved out of bespoke JSON marker files and into git's
  own per-worktree configuration (`extensions.worktreeConfig` plus
  `git config --worktree tally.*`). The markers under
  `<workspaceRoot>/.state/<runHash>/<taskId>.json` were a second copy of the
  truth that could outlive its lane or go missing while the lane survived; git
  creates the metadata with `git worktree add` and destroys it with
  `git worktree remove`, so lane enumeration and lane validation now read one
  source. No marker files are written any more, and the sweep authorizes
  deleting a lane git never registered from the campaign's own derived lane
  layout instead. The run-scoped pass record under `.state/passes/` is
  unaffected.
- `spec_build_driver.py` and `agency_nightly_driver.py` now create, resume, and
  validate worktrees through one shared manager, `campaign_worktrees.py`, which
  ships beside them in a single store directory. The two drivers previously
  implemented the same job twice with different invariants; resume now means
  the same thing in both — a lane whose recorded identity matches is adopted
  work and all, an existing branch with no worktree is re-adopted rather than
  refused, and anything else in the way is a typed conflict. The nightly
  driver's morning report is unchanged.
- The generated campaign producer's projection literals (`postReceipt`,
  `postEvidence`, `postGateSummary`, `requestReview`, `closeOnAcceptance`,
  `closeOnPass`, `neverMutate`) are now `lib.mkDefault`, so an estate can tune
  one campaign's public surface with an ordinary override instead of forking
  the producer builder or reaching for `mkForce`. Rendered defaults are
  unchanged.
- `tally witness verify-authorship` now compares the notes-ref target by
  ancestry instead of by equality. A notes ref grows whenever any commit in the
  repository is annotated — including a campaign merge node binding its squash
  commit — so equality reported `notes-ref-target-mismatch` for every
  repository that stayed in use after the binding. The witnessed target must
  still be an ancestor of the observed one; a ref that was rewritten, rolled
  back, or rebuilt still reports the typed mismatch. The proof is unchanged and
  remains exact: the note blob for the witnessed revision must hash to the
  witnessed digest.

- GitHub receipt and evidence comments are now sticky. Tally stores the node id
  `addComment` returns under the producer state directory, keyed by receipt or
  completion id, and edits that comment in place on later publications instead
  of paginating the whole thread looking for its marker. Markers stay in the
  comment body as the recovery key: a thread whose comment predates the stored
  id, or whose producer state was lost, is still recognized by one scan and
  adopts the existing comment rather than duplicating it, and a remembered
  comment that has since been deleted is forgotten and recreated once instead
  of wedging the sink. Steering, escalation, and closing-summary comments are
  deliberately outside this primitive and remain fresh comments, so the
  operator is still notified.
- Forge-native campaigns now read completion through one bounded GraphQL walk
  of the master issue's native sub-issues — parent → `subIssues` →
  `closedByPullRequestsReferences` → `pullRequest.merged` — instead of scanning
  the repository's recent merged pull requests. The walk narrows where a
  candidate may come from; it never widens what counts as proof, so a pull
  request reached this way still completes a task only under the exact
  revision-bound marker and the same base, head, merge-commit, and ancestry
  validation as before. `tally campaign arm` probes the walk once and records
  the answer in the registration; a forge that cannot serve it arms in degraded
  mode and keeps the checkbox projection, and the publish path's
  already-open-pull-request lookup now reads the task's stable head branch
  directly. Campaign proof no longer ages out of a forge-wide scan window.
- A closed sub-issue is no longer silent. `pullRequest.merged` remains the only
  completion oracle, so a sub-issue closed by hand while its task holds no
  revision-valid merged pull request leaves the task incomplete and records a
  typed `closed-without-merged-proof` anomaly. `tally query run` prints those
  anomalies above the task board and reports the run as `needs-attention`.
- Machine diagnoses and machinery-retry receipts for a task now post on that
  task's own sub-issue thread, and its retry brief reads them back from there
  scoped to that task. An allowed actor's comment on a task sub-issue reaches
  that task's agent as `steering.authorizedComments` and advances the
  observation revision. The master issue stays the campaign-wide channel:
  campaign-level steering still reaches every task, and escalation and the
  closing summary are still posted there.
- Removed the per-merge progress comment. Under the native sub-issue projection
  the parent's own progress bar is the projection and tally writes nothing —
  no comment and no checkbox edit; a degraded campaign still recomputes and
  repairs its checkboxes exactly as before.
- Moved new checkpoint receipts from `refs/tags/tally/spec-build/v1/` to the
  hidden `refs/tally/spec-build/v1/<scope>/checkpoint/` namespace the
  campaign's other durable state already uses. Tags are auto-fetched by every
  clone, so a private campaign's checkpoint ledger was becoming part of a
  public target repository's surface. Already-published tag receipts are still
  read and honored, so nothing is re-executed; `doc/src/flows/campaigns.md`
  documents how to clean a target that carries them.
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
