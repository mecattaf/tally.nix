# Changelog

All notable changes to tally.nix are recorded here. The format is based on
[Keep a Changelog], and the project intends to follow [Semantic Versioning] once version tags are
authorized.

## [Unreleased]

### Fixed

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
  `freeSpaceCheckedAt` is now monotonic across interleaved probe and walk
  applications. Admission math is unaffected either way — every admission
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
